/// WDB fixed header, trailer, arena table, and directory entries.
///
/// Parsed from §2–§5 of the WDB format spec.
/// File layout overview: §1. Fixed header (`0x00`–`0xC7`): §2.
/// Arena table (`0xC8`–trailer): §3. Trailer (0x48 bytes): §4.
/// Directory entries (48-byte structs): §5.
use std::io::{Read, Seek};

use crate::error::Result;
use crate::io::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WdbHeader {
    /// `Xilinx WAVE DATABASE 01` — magic string at file offset `0x00` (§2, offset `0x00`).
    pub magic: String,
    /// `Xilinx Simulator` — tool identification string at file offset `0x18` (§2, offset `0x18`).
    pub tool_string: String,
    /// Unix timestamp (seconds since epoch) recording when the file was written — see §2, offset `0x38`.
    pub file_write_time: u32,

    /// File offset of the `WDB.Event` directory entry (locates the trailer) — see §2, offset `0x48`.
    pub trailer_dir_offset: u64,
    /// File offset of the `Xilinx RTTI` directory entry (locates the type table) — see §2, offset `0x50`.
    pub type_table_dir_offset: u64,
    /// File offset of the `Xilinx DBG` directory entry (locates the netlist hierarchy) — see §2, offset `0x58`.
    pub hierarchy_dir_offset: u64,
}

#[derive(Debug, Clone)]
pub struct WdbTrailer {
    /// Simulation end time in the file's time unit (§4, trailer offset `+0x00`).
    pub end_time: u64,
    /// Number of arena table slots (§4, trailer offset `+0x0C`).
    pub arena_table_slots: u32,
    /// Signal IDs per arena; constant `0x800` = 2048 (§4, trailer offset `+0x10`; §3 arena definition).
    pub arena_span: u64,
    /// Total Signal ID address space allocated; equals `arena_table_slots × 0x800` (§4, trailer offset `+0x18`; §3).
    pub signal_id_space: u64,
    /// Number of logged ranges at the marker (`0` if nothing logged) (§4, trailer offset `+0x30`; §10).
    pub num_logged_ranges: u64,
    /// File offset of the marker (`0` if nothing logged) (§4, trailer offset `+0x38`; §10).
    pub marker_file_offset: u64,
    /// Inflated page size in bytes; constant `0x2800` = 10240 (§4, trailer offset `+0x40`; §11).
    pub page_size: u32,
}

/// A 48-byte directory entry pointing to one of the three major file sections (§5).
///
/// Layout: 24-byte NUL-terminated name, `uint64` count, `uint64` section offset, `uint64` section length.
/// The three entries are `WDB.Event` (trailer), `Xilinx RTTI` (type table), and `Xilinx DBG` (hierarchy).
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    /// Entry name, NUL-terminated, 24 bytes on disk (§5, entry byte offset 0).
    pub name: String,
    /// Entry count; usually `1` (§5, entry byte offset 24).
    pub count: u64,
    /// File offset of the section start (§5, entry byte offset 32).
    pub section_offset: u64,
    /// Section length in bytes (§5, entry byte offset 40).
    pub section_length: u64,
}

/// All file-level metadata parsed from the header region.
#[derive(Debug)]
pub struct WdbFileMetadata {
    pub header: WdbHeader,
    pub trailer: WdbTrailer,
    /// One `u64` file offset per arena slot; `0` means no data for that arena (§3).
    ///
    /// Runs from file offset `0xC8` to the start of the trailer. Slot count =
    /// `(trailer_offset - 0xC8) / 8`. A non-zero slot points to the arena record
    /// in the page directory (§9.1).
    pub arena_table: Vec<u64>,
    /// Directory entry for the trailer section (`WDB.Event`) — see §5.
    pub trailer_dir: DirectoryEntry,
    /// Directory entry for the type table (`Xilinx RTTI`) — see §5.
    pub type_table_dir: DirectoryEntry,
    /// Directory entry for the netlist hierarchy (`Xilinx DBG`) — see §5.
    pub hierarchy_dir: DirectoryEntry,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse all file-level metadata: fixed header (§2), arena table (§3),
/// trailer (§4), and the three directory entries (§5).
///
/// Entry point: reads the fixed header at `0x00`–`0xC7` (§2), uses the
/// pointer at `0x48` to locate the `WDB.Event` directory entry and thereby
/// the trailer (`trailer_offset = wdb_event_pointer - 0x48`, §2), then reads
/// the arena table at `0xC8` (§3) and the trailer (§4).
pub fn parse_file_metadata<R: Read + Seek>(r: &mut R) -> Result<WdbFileMetadata> {
    // --- §2 Fixed Header ---
    seek_to(r, 0x00)?;
    let magic = read_fixed_string(r, 24)?;        // §2 offset 0x00: magic
    let tool_string = read_fixed_string(r, 24)?;  // §2 offset 0x18: tool string
    let _header_size = read_u64_le(r)?;           // §2 offset 0x30: constant 0x40
    let file_write_time = read_u32_le(r)?;        // §2 offset 0x38: Unix timestamp
    skip(r, 4)?;  // 0x3C: unknown
    skip(r, 8)?;  // 0x40: reserved (always 0)

    let trailer_dir_offset = read_u64_le(r)?;     // §2 offset 0x48: WDB.Event pointer
    let type_table_dir_offset = read_u64_le(r)?;  // §2 offset 0x50: Xilinx RTTI pointer
    let hierarchy_dir_offset = read_u64_le(r)?;   // §2 offset 0x58: Xilinx DBG pointer

    // 0x60–0x97: unknown (always 0)                      §2 offset 0x60, 56 bytes
    // 0x98–0xA3: 3 × u32 constants 0x30                  §2 offset 0x98
    // 0xC0: u32 constant 3 (number of sections)          §2 offset 0xC0
    // 0xC4: u32 noise                                     §2 offset 0xC4
    // Total: 56 + 12 + 4 + 4 = 76 bytes to skip
    skip(r, 76)?;

    // --- §3 Arena Table ---
    // The pointer at 0x48 points to the WDB.Event entry, which sits immediately
    // *after* the trailer (trailer_offset + 0x48).  So:
    //   trailer_start = trailer_dir_offset - 0x48
    //   arena_table_length_bytes = trailer_start - 0xC8
    //   slot_count = arena_table_length_bytes / 8
    let trailer_start = trailer_dir_offset - 0x48;
    let arena_table_bytes = trailer_start - 0xC8;
    let slot_count = (arena_table_bytes / 8) as usize;

    seek_to(r, 0xC8)?; // §3: arena table begins at 0xC8
    let mut arena_table = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        arena_table.push(read_u64_le(r)?);
    }

    // --- §4 Trailer ---
    seek_to(r, trailer_start)?;
    let end_time = read_u64_le(r)?;           // §4 +0x00: simulation end time
    skip(r, 4)?;                              // §4 +0x08: unknown constant 0x3E9
    let arena_table_slots = read_u32_le(r)?;  // §4 +0x0C: number of arena slots
    let arena_span = read_u64_le(r)?;         // §4 +0x10: constant 0x800
    let signal_id_space = read_u64_le(r)?;    // §4 +0x18: total Signal ID space
    skip(r, 4)?;                              // §4 +0x20: constant 0xC8
    skip(r, 4)?;                              // §4 +0x24: constant 0
    skip(r, 8)?;                              // §4 +0x28: constant 0
    let num_logged_ranges = read_u64_le(r)?;  // §4 +0x30: logged range count
    let marker_file_offset = read_u64_le(r)?; // §4 +0x38: marker file offset
    let page_size = read_u32_le(r)?;          // §4 +0x40: constant 0x2800
    skip(r, 4)?;                              // §4 +0x44: constant 0x64

    // --- §5 Directory Entries ---
    let trailer_dir = read_dir_entry(r, trailer_dir_offset)?;
    let type_table_dir = read_dir_entry(r, type_table_dir_offset)?;
    let hierarchy_dir = read_dir_entry(r, hierarchy_dir_offset)?;

    Ok(WdbFileMetadata {
        header: WdbHeader {
            magic,
            tool_string,
            file_write_time,
            trailer_dir_offset,
            type_table_dir_offset,
            hierarchy_dir_offset,
        },
        trailer: WdbTrailer {
            end_time,
            arena_table_slots,
            arena_span,
            signal_id_space,
            num_logged_ranges,
            marker_file_offset,
            page_size,
        },
        arena_table,
        trailer_dir,
        type_table_dir,
        hierarchy_dir,
    })
}

/// Parse one 48-byte directory entry from the given file offset (§5).
///
/// Layout: 24-byte name | `uint64` count | `uint64` section_offset | `uint64` section_length.
fn read_dir_entry<R: Read + Seek>(r: &mut R, offset: u64) -> Result<DirectoryEntry> {
    seek_to(r, offset)?;
    let name = read_fixed_string(r, 24)?;         // §5 entry byte offset 0
    let count = read_u64_le(r)?;                  // §5 entry byte offset 24
    let section_offset = read_u64_le(r)?;         // §5 entry byte offset 32
    let section_length = read_u64_le(r)?;         // §5 entry byte offset 40
    Ok(DirectoryEntry {
        name,
        count,
        section_offset,
        section_length,
    })
}
