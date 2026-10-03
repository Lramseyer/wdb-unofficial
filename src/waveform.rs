/// Waveform data — §9–§12 of the WDB format spec.
///
/// Handles: arena records, page directory, zlib decompression, value change
/// records, VHDL and Verilog value encodings, chunked/wide values.
use std::collections::HashMap;
use std::io::{Read, Seek};

use flate2::read::ZlibDecoder;

use crate::error::{Result, WdbError};
use crate::io::*;

// ---------------------------------------------------------------------------
// Page Directory
// ---------------------------------------------------------------------------

/// One page's location in the file — see §9.1 (Arena Record).
///
/// Offsets and lengths come from the two parallel arrays inside each
/// 0x4C0-byte arena record: the 100×u64 page file-offset array at +0x008
/// and the 100×u32 compressed-length array at +0x328.
#[derive(Debug, Clone)]
pub struct PageRef {
    /// File offset of the compressed page data — from the 100×u64 array at
    /// arena record offset +0x008 (§9.1).
    pub offset: u64,
    /// Compressed length in bytes — from the 100×u32 array at arena record
    /// offset +0x328 (§9.1).
    pub compressed_len: u32,
}

/// Parsed page directory: arena_index → list of pages — see §9 (Page Directory).
///
/// The key is the arena index (`signal_id >> 11`, §3/§7). Each arena covers
/// 0x800 Signal IDs; the value is the ordered list of [`PageRef`]s for that
/// arena, one entry per page emitted during simulation.
pub type PageDirectory = HashMap<u64, Vec<PageRef>>;

/// Parse the page directory from the arena table and the arena records — see §9, §9.1.
///
/// The page directory is located immediately after the `Xilinx DBG` directory
/// entry (entry_offset + 48).  Arena record `i` is at
/// `page_dir_base + 0x4C0 * i`.
pub fn parse_page_directory<R: Read + Seek>(
    r: &mut R,
    arena_table: &[u64],
    page_dir_base: u64,
) -> Result<PageDirectory> {
    let mut dir: PageDirectory = HashMap::new();

    for (arena_idx, &slot) in arena_table.iter().enumerate() {
        if slot == 0 {
            continue;
        }
        // `slot` is the file offset of the arena record.
        let pages = read_arena_record(r, slot)?;
        if !pages.is_empty() {
            dir.insert(arena_idx as u64, pages);
        }
    }

    Ok(dir)
}

/// Read a (possibly chained) arena record and return all page refs — see §9.1.
///
/// Each arena record is exactly 0x4C0 bytes with the layout (§9.1):
/// - +0x000: `uint64` continuation pointer (0 if no chain)
/// - +0x008: 100 × `uint64` page file offsets
/// - +0x328: 100 × `uint32` compressed page lengths
/// - +0x4B8: `uint64` number of pages listed in this record
///
/// If word 0 ≠ 0 the arena has more than 100 pages; follow the continuation
/// chain until word 0 == 0 (§9.1).
fn read_arena_record<R: Read + Seek>(r: &mut R, record_offset: u64) -> Result<Vec<PageRef>> {
    const MAX_PAGES_PER_RECORD: usize = 100;
    let mut all_pages = Vec::new();
    let mut next = record_offset;

    loop {
        seek_to(r, next)?;
        // Continuation word at +0x000 — non-zero means more than 100 pages
        // exist and this word points to the next arena record (§9.1).
        let continuation = read_u64_le(r)?; // +0x000

        // 100 × u64 page file offsets (+0x008) — see §9.1.
        let mut offsets = [0u64; MAX_PAGES_PER_RECORD];
        for o in offsets.iter_mut() {
            *o = read_u64_le(r)?;
        }

        // 100 × u32 compressed lengths (+0x328) — see §9.1.
        let mut lengths = [0u32; MAX_PAGES_PER_RECORD];
        for l in lengths.iter_mut() {
            *l = read_u32_le(r)?;
        }

        // Number of pages listed in this record (+0x4B8) — see §9.1.
        let n_pages = read_u64_le(r)? as usize; // +0x4B8

        for i in 0..n_pages.min(MAX_PAGES_PER_RECORD) {
            if offsets[i] != 0 {
                all_pages.push(PageRef {
                    offset: offsets[i],
                    compressed_len: lengths[i],
                });
            }
        }

        if continuation == 0 {
            break;
        }
        next = continuation;
    }

    Ok(all_pages)
}

// ---------------------------------------------------------------------------
// Value Change Records
// ---------------------------------------------------------------------------

/// One value change record parsed from a decompressed page — see §11.2.
#[derive(Debug, Clone)]
pub struct RawValueChange {
    /// Simulation time of the change (in the file's time unit) — §11.2 offset 0 (uint64).
    pub time: u64,
    /// Local Signal ID within the arena (`signal_id & 0x7FF`) — §11.2 offset 8 (uint32).
    pub local_signal_id: u64,
    /// Raw value bytes; length is the §11.2 offset-12 uint32 value-length field.
    pub value: Vec<u8>,
}

/// Page header — see §11.1.
///
/// Occupies the first 20 bytes of the inflated page. Value change records
/// begin immediately at offset 20 (§11.2).
#[derive(Debug, Clone)]
pub struct PageHeader {
    /// Time of the first value change on this page — §11.1 offset 0 (uint64).
    pub t0: u64,
    /// `last_change_time - t0` — §11.1 offset 8 (uint64).
    pub t1: u64,
    /// Number of value change records on this page — §11.1 offset 16 (uint32).
    pub n: u32,
}

/// Decompress one page and extract its value change records — see §11 (Value Pages).
///
/// Each page inflates to exactly **10240 bytes** (`0x2800`), as stored in the
/// trailer at `+0x40` (§4 / §11). The inflated layout is:
/// - bytes 0–19: page header (`t0` at 0, `t1` at 8, `n` at 16) — §11.1
/// - bytes 20+:  `n` value change records back-to-back, zero-padded to 10240 — §11.2
pub fn read_page<R: Read + Seek + ?Sized>(
    r: &mut R,
    page: &PageRef,
    page_size: usize,
) -> Result<Vec<RawValueChange>> {
    seek_to(r, page.offset)?;
    let compressed = read_bytes(r, page.compressed_len as usize)?;

    // Decompress using zlib (§11 — pages are zlib-compressed streams).
    let mut decompressed = Vec::with_capacity(page_size);
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| WdbError::Decompression(e.to_string()))?;

    if decompressed.len() < 20 {
        return Ok(vec![]);
    }

    // Page header (§11.1): t0 at offset 0, t1 at offset 8, n at offset 16.
    let _t0 = u64_at(&decompressed, 0);
    let _t1 = u64_at(&decompressed, 8);
    let n = u32_at(&decompressed, 16) as usize;

    let mut records = Vec::with_capacity(n);
    // Value change records start at offset 20 (§11.2).
    let mut pos = 20usize;

    for _ in 0..n {
        if pos + 16 > decompressed.len() {
            break;
        }
        // §11.2 record layout: offset 0 = uint64 time, offset 8 = uint32
        // local_signal_id, offset 12 = uint32 value length, offset 16 = value bytes.
        let time = u64_at(&decompressed, pos);
        let local_signal_id = u32_at(&decompressed, pos + 8) as u64;
        let value_len = u32_at(&decompressed, pos + 12) as usize;
        pos += 16;
        if pos + value_len > decompressed.len() {
            break;
        }
        let value = decompressed[pos..pos + value_len].to_vec();
        pos += value_len;
        records.push(RawValueChange {
            time,
            local_signal_id,
            value,
        });
    }

    Ok(records)
}

// ---------------------------------------------------------------------------
// High-Level: Load All Signal Changes
// ---------------------------------------------------------------------------

/// All value changes for one signal, keyed by global Signal ID.
pub type SignalChanges = Vec<(u64, Vec<u8>)>; // (time, raw_bytes)

/// Load all signal changes from the page directory — see §9 and §11.
///
/// Returns a map from global Signal ID → sorted list of (time, raw_value).
///
/// The global Signal ID is reconstructed as `(arena_idx << 11) | local_signal_id`
/// (§3/§7 Signal ID structure: `arena_idx = signal_id >> 11`,
/// `local_signal_id = signal_id & 0x7FF`).
pub fn load_all_signal_changes<R: Read + Seek>(
    r: &mut R,
    page_dir: &PageDirectory,
    page_size: usize,
) -> Result<HashMap<u64, SignalChanges>> {
    let mut changes: HashMap<u64, SignalChanges> = HashMap::new();

    for (&arena_idx, pages) in page_dir {
        for page_ref in pages {
            let records = read_page(r, page_ref, page_size)?;
            for rec in records {
                // Global Signal ID = (arena_idx << 11) | local_signal_id
                // (§3/§7: arena index = signal_id >> 11, local = signal_id & 0x7FF).
                let global_id = (arena_idx << 11) | rec.local_signal_id;
                changes
                    .entry(global_id)
                    .or_default()
                    .push((rec.time, rec.value));
            }
        }
    }

    // Sort each signal's changes by time.
    for ch in changes.values_mut() {
        ch.sort_by_key(|(t, _)| *t);
    }

    Ok(changes)
}

// ---------------------------------------------------------------------------
// Value Decoding
// ---------------------------------------------------------------------------

/// A decoded signal value — see §12 (Value Encodings).
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedValue {
    /// VHDL binary blob (raw bytes in their WDB encoding) — §12.1.
    Binary(Vec<u8>),
    /// Verilog 4-state logic vector: one `LogicBit` per bit, LSB first — §12.2.
    Logic(Vec<LogicBit>),
    /// Real / float64 value (`real`/`shortreal`) — §12.2 / §12.3 special Verilog types.
    Real(f64),
}

/// A single 4-state logic bit — see §12.2.
///
/// Encoded on disk as interleaved `(val_bit, xz_bit)` pairs per §12.2:
/// - `(0, 0)` → `0`
/// - `(1, 0)` → `1`
/// - `(0, 1)` → `Z`
/// - `(1, 1)` → `X`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicBit {
    Zero,
    One,
    Z,
    X,
}

impl LogicBit {
    /// Construct a `LogicBit` from the `(val_bit, xz_bit)` on-disk encoding — §12.2.
    pub fn from_val_xz(val: bool, xz: bool) -> Self {
        match (val, xz) {
            (false, false) => LogicBit::Zero,
            (true, false) => LogicBit::One,
            (false, true) => LogicBit::Z,
            (true, true) => LogicBit::X,
        }
    }

    pub fn to_char(self) -> char {
        match self {
            LogicBit::Zero => '0',
            LogicBit::One => '1',
            LogicBit::Z => 'z',
            LogicBit::X => 'x',
        }
    }
}

/// Decode a Verilog value from an 8-byte (or 16/32/64-byte) raw record — see §12.2.
///
/// Layout: interleaved `(val_u32, xz_u32)` pairs, LSW first (§12.2).
/// `bit_width` is the declared signal width in bits.
///
/// Record size by declared width (§12.2 table):
/// - 1–32 bits   →  8 bytes  (1 `(val, xz)` pair)
/// - 33–64 bits  → 16 bytes  (2 pairs)
/// - 65–128 bits → 32 bytes  (4 pairs)
/// - 129+ bits   → 64+ bytes (one pair per 32 bits; unconfirmed above 128 bits)
pub fn decode_verilog_value(raw: &[u8], bit_width: u32) -> DecodedValue {
    let mut bits = Vec::with_capacity(bit_width as usize);
    let n_pairs = raw.len() / 8;

    for pair_idx in 0..n_pairs {
        let base = pair_idx * 8;
        if base + 8 > raw.len() {
            break;
        }
        let val_word = u32::from_le_bytes(raw[base..base + 4].try_into().unwrap());
        let xz_word = u32::from_le_bytes(raw[base + 4..base + 8].try_into().unwrap());
        let bit_start = pair_idx * 32;
        for bit_in_pair in 0..32u32 {
            let global_bit = bit_start + bit_in_pair as usize;
            if global_bit >= bit_width as usize {
                break;
            }
            let val_bit = (val_word >> bit_in_pair) & 1 == 1;
            let xz_bit = (xz_word >> bit_in_pair) & 1 == 1;
            bits.push(LogicBit::from_val_xz(val_bit, xz_bit));
        }
    }

    DecodedValue::Logic(bits)
}

/// Decode a real (f64) from an 8-byte raw record — see §12.2 / §12.3.
///
/// `real`/`shortreal` values are stored as `float64` IEEE 754 little-endian (§12.3).
pub fn decode_real(raw: &[u8]) -> DecodedValue {
    if raw.len() >= 8 {
        let v = f64::from_le_bytes(raw[0..8].try_into().unwrap());
        DecodedValue::Real(v)
    } else {
        DecodedValue::Binary(raw.to_vec())
    }
}

/// Format a `DecodedValue` as a human-readable string (similar to wdbcvt output).
///
/// - VHDL enumeration literals are looked up via `enum_literals` (§12.1 — 1-byte
///   literal index for enumerations with ≤256 literals).
/// - Verilog logic vectors are rendered MSB-first (§12.2).
pub fn format_value(v: &DecodedValue, is_vhdl: bool, enum_literals: Option<&[String]>) -> String {
    match v {
        DecodedValue::Binary(bytes) => {
            if bytes.len() == 1 {
                // §12.1 VHDL enumeration: 1-byte literal index into enum_literals.
                if let Some(lits) = enum_literals {
                    let idx = bytes[0] as usize;
                    if idx < lits.len() {
                        return lits[idx].clone();
                    }
                }
                return format!("{}", bytes[0]);
            }
            if bytes.len() == 4 {
                let v = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
                return format!("{}", v);
            }
            if bytes.len() == 8 {
                let v = i64::from_le_bytes(bytes[0..8].try_into().unwrap());
                return format!("{}", v);
            }
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ")
        }
        DecodedValue::Logic(bits) => {
            // §12.2 Verilog logic vector.
            if bits.len() == 1 {
                bits[0].to_char().to_string()
            } else {
                // MSB-first display.
                bits.iter().rev().map(|b| b.to_char()).collect()
            }
        }
        DecodedValue::Real(f) => format!("{}", f),
    }
}

// ---------------------------------------------------------------------------
// VHDL std_ulogic literal indices
// ---------------------------------------------------------------------------

/// VHDL `std_ulogic` enumeration literal index table — see §12.1.
///
/// Literal indices per §12.1: `U=0 X=1 0=2 1=3 Z=4 W=5 L=6 H=7 -=8`.
/// A VHDL enumeration value of kind `std_ulogic` is encoded as a 1-byte index
/// into this array (§12.1 "std_ulogic literal indices").
pub const STD_ULOGIC_LITERALS: &[&str] = &["U", "X", "0", "1", "Z", "W", "L", "H", "-"];
