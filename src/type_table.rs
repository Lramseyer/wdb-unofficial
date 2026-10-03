/// Type table parser — §6 of the WDB format spec.
///
/// The type table starts with the magic `Xilinx ISim TYPE FILE 001\0` — §6.1.
/// Contains one entry per distinct signal type used in the design;
/// see §6.1 for the full framing (magic, timestamp, `n_types`,
/// entries-end offset, back-to-back entries, and trailing offset list).
use std::io::{Read, Seek};

use crate::error::{Result, WdbError};
use crate::io::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Source language indicator for a type — see §6.2 for all origin word values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeOrigin {
    /// VHDL type (not TIME) — §6.2 origin `0x02`.
    VhdlType,
    /// VHDL TIME type — §6.2 origin `0x0A`.
    VhdlTime,
    /// Verilog unnamed/user type (vector, struct, enum, typedef) — §6.2 origin `0x01`.
    VerilogUser,
    /// Verilog predefined scalar (`logic`, `bit`, `real`, etc.) — §6.2 origin `0x05`.
    VerilogPredefined,
    /// Verilog `time` type — §6.2 origin `0x0D`.
    VerilogTime,
    Unknown(u32),
}

impl TypeOrigin {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0x02 => TypeOrigin::VhdlType,
            0x0A => TypeOrigin::VhdlTime,
            0x01 => TypeOrigin::VerilogUser,
            0x05 => TypeOrigin::VerilogPredefined,
            0x0D => TypeOrigin::VerilogTime,
            other => TypeOrigin::Unknown(other),
        }
    }

    pub fn is_vhdl(self) -> bool {
        matches!(self, TypeOrigin::VhdlType | TypeOrigin::VhdlTime)
    }
}

/// A range triple `[left, right, dir]` — see §6.4.
///
/// Each triple is `[i32 left][i32 right][i32 dir]`. The terminator after
/// the last triple in array and record entries is `−99` (`0xFFFFFF9D`) — §6.4.
#[derive(Debug, Clone, Copy)]
pub struct RangeTriple {
    pub left: i32,
    pub right: i32,
    /// `1` = `to`, `-1` = `downto`, `-2` = unconstrained marker — §6.4.
    pub dir: i32,
}

impl RangeTriple {
    pub fn is_unconstrained(self) -> bool {
        self.dir == -2
    }

    /// Element count for this range (0 for unconstrained or null ranges).
    pub fn element_count(self) -> usize {
        if self.is_unconstrained() {
            return 0;
        }
        match self.dir {
            1 => self.right.saturating_sub(self.left).max(0) as usize + 1,
            -1 => self.left.saturating_sub(self.right).max(0) as usize + 1,
            _ => 0,
        }
    }
}

/// Kind codes from the type table.
#[derive(Debug, Clone)]
pub enum TypeKind {
    /// Kind `0x03` — Enumeration (VHDL, Verilog) — §6.3 Kind 0x03.
    Enumeration {
        origin: TypeOrigin,
        variant: u32,
        class: u32,
        /// Literal names.
        literals: Vec<String>,
        /// Byte size per value: `1` for ≤256 literals, `4` for ≥257, `0` for Verilog — §6.3 Kind 0x03.
        size: u32,
    },
    /// Kind `0x04` — Named values (SystemVerilog enum) — §6.3 Kind 0x04.
    NamedValues {
        origin: TypeOrigin,
        base_type_index: u32,
        entries: Vec<(String, u64)>,
        ranges: Vec<RangeTriple>,
    },
    /// Kind `0x05` — Integer (VHDL or Verilog packed integer) — §6.3 Kind 0x05.
    Integer {
        origin: TypeOrigin,
        low: i32,
        high: i32,
    },
    /// Kind `0x06` — Real (IEEE 754 float) — §6.3 Kind 0x06.
    Real {
        origin: TypeOrigin,
        /// VHDL only; Verilog has no bounds.
        bounds: Option<(f64, f64)>,
    },
    /// Kind `0x07` — Alias (typedef) — §6.3 Kind 0x07.
    Alias {
        origin: TypeOrigin,
        target_index: u32,
        ranges: Vec<RangeTriple>,
    },
    /// Kind `0x08` — Access (pointer type, VHDL) — §6.3 Kind 0x08.
    Access {
        origin: TypeOrigin,
        designated_type_index: u32,
    },
    /// Kind `0x0C` — File type (VHDL) — §6.3 Kind 0x0C.
    File {
        origin: TypeOrigin,
        element_type_index: u32,
    },
    /// Kind `0x0D` — Physical (VHDL time/user physical) — §6.3 Kind 0x0D.
    Physical {
        origin: TypeOrigin,
        /// (unit_name, scale_in_base_units)
        units: Vec<(String, u64)>,
    },
    /// Kind `0x10` — Array — §6.3 Kind 0x10.
    Array {
        origin: TypeOrigin,
        /// Layout: `1`=VHDL, `2`=unpacked, `3`=packed, `6`=SV packed union — §6.3 Kind 0x10.
        layout: u16,
        element_type_index: u32,
        index_type_indices: Vec<u32>,
        ranges: Vec<RangeTriple>,
    },
    /// Kind `0x11` — Record / struct — §6.3 Kind 0x11.
    Record {
        origin: TypeOrigin,
        layout: u16,
        fields: Vec<RecordField>,
    },
    /// Kind `0x13` — Dynamic array (SV, `-debug all`) — §6.3 Kinds 0x13–0x18.
    DynamicArray {
        origin: TypeOrigin,
        element_type_index: u32,
    },
    /// Kind `0x14` — Queue (SV, `-debug all`) — §6.3 Kinds 0x13–0x18.
    Queue {
        origin: TypeOrigin,
        element_type_index: u32,
    },
    /// Kind `0x15` — Associative array (SV, `-debug all`) — §6.3 Kinds 0x13–0x18.
    AssocArray {
        origin: TypeOrigin,
        element_type_index: u32,
        key_type_index: u32,
    },
    /// Kind `0x17` — Class (SV, `-debug all`) — §6.3 Kinds 0x13–0x18.
    Class {
        origin: TypeOrigin,
        parent_class_index: Option<u32>,
        fields: Vec<RecordField>,
    },
    /// Kind `0x18` — String (SV, `-debug all`) — §6.3 Kinds 0x13–0x18.
    StringType { origin: TypeOrigin },
    /// Unknown kind — stored raw for forward compatibility.
    Unknown { kind: u8, data: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: String,
    pub type_index: u32,
    pub ranges: Vec<RangeTriple>,
}

#[derive(Debug, Clone)]
pub struct TypeEntry {
    pub name: String,
    pub kind: TypeKind,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse the type table section starting at `section_offset` — see §6.1 for framing.
pub fn parse_type_table<R: Read + Seek>(r: &mut R, section_offset: u64) -> Result<Vec<TypeEntry>> {
    seek_to(r, section_offset)?;

    // Magic: "Xilinx ISim TYPE FILE 001\0" (26 bytes) — §6.1
    let mut magic_buf = [0u8; 26];
    r.read_exact(&mut magic_buf)?;
    check_magic(&magic_buf, "Xilinx ISim TYPE FILE 001")?;

    // 2 bytes noise, 4 bytes timestamp (noise)
    skip(r, 6)?;

    // n_types and offset-from-magic where entries end — §6.1
    let n_types = read_u32_le(r)? as usize;
    let _entries_end = read_u32_le(r)?;

    // Read all entry data into memory for easier parsing.
    // Entries are back-to-back starting at offset 40 from section_offset.
    // After entries: 8 * n_types bytes of u64 offsets (from magic).
    // Read from current position through the offset list.
    // We compute: each offset is relative to magic (section_offset).

    // Read remaining section data.
    // We'll read the entire section blob and parse entries from it.
    // Actually easier: use the offset list to find each entry.
    // Current position is 40 bytes into the section (magic 26 + noise 2 + ts 4 + n 4 + end 4).
    // entries_end is the offset from magic where entries end; offset list starts there.

    // Re-read the end offset — already read it above.
    // Read entries sequentially and also collect offset table.
    // For simplicity, read the entries sequentially since they're back-to-back.
    let mut entries = Vec::with_capacity(n_types);
    for _ in 0..n_types {
        let entry = parse_one_type_entry(r)?;
        entries.push(entry);
    }

    Ok(entries)
}

/// Parse one type entry from the current stream position — see §6.1.
///
/// Each entry: `[u32 length][u32 tag] name\0 body`.
/// The low byte of `tag` is the kind code; the high bytes are always `0xA0` — §6.1.
fn parse_one_type_entry<R: Read + Seek>(r: &mut R) -> Result<TypeEntry> {
    let entry_start = r.seek(std::io::SeekFrom::Current(0))?;
    let length = read_u32_le(r)? as u64;
    let tag = read_u32_le(r)?;
    // Low byte = kind code; high bytes always 0xA0 — §6.1
    let kind_code = (tag & 0xFF) as u8;

    // Read name (NUL-terminated).
    let name = read_nul_string(r)?;

    // Read body (entry_start + length - current_position bytes).
    let current_pos = r.seek(std::io::SeekFrom::Current(0))?;
    let body_len = (entry_start + length).saturating_sub(current_pos) as usize;
    let body = read_bytes(r, body_len)?;

    let kind = parse_type_body(kind_code, &body)?;

    Ok(TypeEntry { name, kind })
}

fn read_nul_string<R: Read>(r: &mut R) -> Result<String> {
    let mut s = String::new();
    loop {
        let b = read_u8(r)?;
        if b == 0 {
            break;
        }
        s.push(b as char);
    }
    Ok(s)
}

fn read_nul_string_from(data: &[u8], offset: &mut usize) -> String {
    let start = *offset;
    while *offset < data.len() && data[*offset] != 0 {
        *offset += 1;
    }
    let s = std::str::from_utf8(&data[start..*offset]).unwrap_or("").to_string();
    if *offset < data.len() {
        *offset += 1; // skip NUL
    }
    s
}

fn read_range_triples(data: &[u8], offset: &mut usize, n: usize) -> Vec<RangeTriple> {
    let mut triples = Vec::with_capacity(n);
    for _ in 0..n {
        if *offset + 12 > data.len() {
            break;
        }
        let left = i32_at(data, *offset);
        let right = i32_at(data, *offset + 4);
        let dir = i32_at(data, *offset + 8);
        *offset += 12;
        triples.push(RangeTriple { left, right, dir });
    }
    triples
}

fn parse_type_body(kind_code: u8, data: &[u8]) -> Result<TypeKind> {
    if data.len() < 4 {
        return Ok(TypeKind::Unknown {
            kind: kind_code,
            data: data.to_vec(),
        });
    }

    let origin = TypeOrigin::from_u32(u32_at(data, 0));
    let mut pos = 4usize;

    match kind_code {
        // Kind 0x03 — Enumeration
        0x03 => {
            if data.len() < 16 {
                return Ok(TypeKind::Unknown { kind: kind_code, data: data.to_vec() });
            }
            let variant = u32_at(data, pos); pos += 4;
            let class = u32_at(data, pos); pos += 4;
            let n = u32_at(data, pos) as usize; pos += 4;
            let mut literals = Vec::with_capacity(n);
            for _ in 0..n {
                literals.push(read_nul_string_from(data, &mut pos));
            }
            let size = if pos + 4 <= data.len() { u32_at(data, pos) } else { 0 };
            Ok(TypeKind::Enumeration { origin, variant, class, literals, size })
        }

        // Kind 0x04 — Named values (SV enum)
        0x04 => {
            let base_type_index = u32_at(data, pos); pos += 4;
            let n = u32_at(data, pos) as usize; pos += 4;
            pos += 4; // constant 8
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let name = read_nul_string_from(data, &mut pos);
                let value = if pos + 8 <= data.len() { u64_at(data, pos) } else { 0 };
                pos += 8;
                entries.push((name, value));
            }
            let nranges = if pos + 4 <= data.len() { u32_at(data, pos) as usize } else { 0 };
            pos += 4;
            let ranges = read_range_triples(data, &mut pos, nranges);
            Ok(TypeKind::NamedValues { origin, base_type_index, entries, ranges })
        }

        // Kind 0x05 — Integer
        0x05 => {
            let low = i32_at(data, pos); pos += 4;
            let high = i32_at(data, pos);
            Ok(TypeKind::Integer { origin, low, high })
        }

        // Kind 0x06 — Real
        0x06 => {
            if origin.is_vhdl() && data.len() >= pos + 20 {
                pos += 4; // variant
                let low = f64::from_le_bytes(data[pos..pos+8].try_into().unwrap()); pos += 8;
                let high = f64::from_le_bytes(data[pos..pos+8].try_into().unwrap());
                Ok(TypeKind::Real { origin, bounds: Some((low, high)) })
            } else {
                Ok(TypeKind::Real { origin, bounds: None })
            }
        }

        // Kind 0x07 — Alias
        0x07 => {
            let target_index = u32_at(data, pos); pos += 4;
            let nranges = u32_at(data, pos) as usize; pos += 4;
            let ranges = read_range_triples(data, &mut pos, nranges);
            Ok(TypeKind::Alias { origin, target_index, ranges })
        }

        // Kind 0x08 — Access
        0x08 => {
            let designated_type_index = u32_at(data, pos);
            Ok(TypeKind::Access { origin, designated_type_index })
        }

        // Kind 0x0C — File
        0x0C => {
            let element_type_index = u32_at(data, pos);
            Ok(TypeKind::File { origin, element_type_index })
        }

        // Kind 0x0D — Physical
        0x0D => {
            let n = u32_at(data, pos) as usize; pos += 4;
            let mut units = Vec::with_capacity(n);
            for _ in 0..n {
                let name = read_nul_string_from(data, &mut pos);
                let scale = if pos + 8 <= data.len() { u64_at(data, pos) } else { 0 };
                pos += 8;
                units.push((name, scale));
            }
            Ok(TypeKind::Physical { origin, units })
        }

        // Kind 0x10 — Array
        0x10 => {
            if data.len() < pos + 8 {
                return Ok(TypeKind::Unknown { kind: kind_code, data: data.to_vec() });
            }
            let layout = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap()); pos += 2;
            pos += 2; // constant 0xA0
            let element_type_index = u32_at(data, pos); pos += 4;
            let dims = u32_at(data, pos) as usize; pos += 4;
            let mut index_type_indices = Vec::with_capacity(dims);
            for _ in 0..dims {
                if pos + 4 > data.len() { break; }
                index_type_indices.push(u32_at(data, pos)); pos += 4;
            }
            let nranges = if pos + 4 <= data.len() { u32_at(data, pos) as usize } else { 0 };
            pos += 4;
            let ranges = read_range_triples(data, &mut pos, nranges);
            // Skip terminator (-99 or small non-negative)
            Ok(TypeKind::Array { origin, layout, element_type_index, index_type_indices, ranges })
        }

        // Kind 0x11 — Record / struct
        0x11 => {
            if data.len() < pos + 8 {
                return Ok(TypeKind::Unknown { kind: kind_code, data: data.to_vec() });
            }
            let layout = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap()); pos += 2;
            pos += 2; // 0x0B
            let n = u32_at(data, pos) as usize; pos += 4;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                let name = read_nul_string_from(data, &mut pos);
                if pos + 8 > data.len() { break; }
                let type_index = u32_at(data, pos); pos += 4;
                let nranges = u32_at(data, pos) as usize; pos += 4;
                let ranges = read_range_triples(data, &mut pos, nranges);
                fields.push(RecordField { name, type_index, ranges });
            }
            Ok(TypeKind::Record { origin, layout, fields })
        }

        // Kind 0x13 — Dynamic array
        0x13 => {
            let element_type_index = u32_at(data, pos);
            Ok(TypeKind::DynamicArray { origin, element_type_index })
        }

        // Kind 0x14 — Queue
        0x14 => {
            let element_type_index = u32_at(data, pos);
            Ok(TypeKind::Queue { origin, element_type_index })
        }

        // Kind 0x15 — Associative array
        0x15 => {
            let element_type_index = u32_at(data, pos); pos += 4;
            pos += 4; // number
            let key_type_index = if pos + 4 <= data.len() { u32_at(data, pos) } else { 0 };
            Ok(TypeKind::AssocArray { origin, element_type_index, key_type_index })
        }

        // Kind 0x17 — Class
        0x17 => {
            let parent_raw = i32_at(data, pos); pos += 4;
            let parent_class_index = if parent_raw < 0 { None } else { Some(parent_raw as u32) };
            pos += 4; // number
            let n_fields = u32_at(data, pos) as usize; pos += 4;
            let mut fields = Vec::with_capacity(n_fields);
            for _ in 0..n_fields {
                let name = read_nul_string_from(data, &mut pos);
                if pos + 8 > data.len() { break; }
                let type_index = u32_at(data, pos); pos += 4;
                let nranges = u32_at(data, pos) as usize; pos += 4;
                let ranges = read_range_triples(data, &mut pos, nranges);
                pos += 4; // trailing 0
                fields.push(RecordField { name, type_index, ranges });
            }
            Ok(TypeKind::Class { origin, parent_class_index, fields })
        }

        // Kind 0x18 — String
        0x18 => Ok(TypeKind::StringType { origin }),

        _ => Ok(TypeKind::Unknown { kind: kind_code, data: data.to_vec() }),
    }
}
