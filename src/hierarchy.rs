/// Netlist hierarchy section parser — §7 + §8 of the WDB format spec.
///
/// Parses scope records, scope definition records, variable definition records,
/// range records, string tables, file table, and variable records.
use std::io::{Read, Seek};

use crate::error::{Result, WdbError};
use crate::io::*;

// ---------------------------------------------------------------------------
// Public types — Scope Definition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    VerilogModule,
    SvInterface,
    SvModport,
    VerilogTask,
    VerilogFunction,
    NamedBlock,
    VerilogProcess,
    SvPackage,
    VhdlEntity,
    VhdlPackage,
    VhdlGenerate,
    VhdlGenerateOrBlock,
    VhdlProcess,
    VhdlFunction,
    VhdlProcedure,
    Root,
    Unknown(u32),
}

impl ScopeKind {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0x00 => ScopeKind::VerilogModule,
            0x01 => ScopeKind::SvInterface,
            0x02 => ScopeKind::SvModport,
            0x03 => ScopeKind::VerilogTask,
            0x04 => ScopeKind::VerilogFunction,
            0x05 => ScopeKind::NamedBlock,
            0x07 => ScopeKind::VerilogProcess,
            0x08 => ScopeKind::SvPackage,
            0x09 => ScopeKind::VhdlEntity,
            0x0A => ScopeKind::VhdlPackage,
            0x0B => ScopeKind::VhdlGenerate,
            0x0C => ScopeKind::VhdlGenerateOrBlock,
            0x0D => ScopeKind::VhdlProcess,
            0x11 => ScopeKind::VhdlFunction,
            0x12 => ScopeKind::VhdlProcedure,
            0x13 => ScopeKind::Root,
            other => ScopeKind::Unknown(other),
        }
    }
}

/// Raw scope record from Region 0 (9 × u32).
#[derive(Debug, Clone)]
pub struct ScopeRecord {
    pub name_offset: u32,
    pub parent_index: i32, // -1 for root
    pub num_children: u32,
    pub first_child_index: i32, // -1 if none
    pub first_var_id: i32,      // -1 if none
    pub file_index: u32,
    pub source_line: u32,
    pub scope_def_index: u32,
}

/// Scope definition record from Region 1 (9 × u32).
#[derive(Debug, Clone)]
pub struct ScopeDefRecord {
    pub entity_name_offset: i32, // -1 if none
    pub arch_name_offset: i32,   // -1 if none
    pub kind: ScopeKind,
    pub num_var_defs: u32,
    pub arch_file_index: u32,
    pub arch_source_line: u32,
    pub entity_file_index: u32,
    pub entity_source_line: u32,
}

/// Variable definition kind codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarDefKind {
    VerilogVar,          // 0x00
    VerilogParameter,    // 0x01
    VerilogNet,          // 0x03
    VerilogNetWand,      // 0x04
    VerilogNetWor,       // 0x05
    VerilogNetTri,       // 0x06
    VerilogNetTriand,    // 0x07
    VerilogNetTrior,     // 0x08
    VerilogNetTri0,      // 0x09
    VerilogNetTri1,      // 0x0A
    VerilogNetTrireg,    // 0x0B
    VerilogNetSupply0,   // 0x0C
    VerilogNetSupply1,   // 0x0D
    VhdlSignal,          // 0x0E
    VhdlVariable,        // 0x0F
    VhdlGeneric,         // 0x12
    VhdlConstant,        // 0x13
    VhdlSubprogramParam, // 0x14
    VhdlSignalParam,     // 0x15
    Unknown(u32),
}

impl VarDefKind {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0x00 => VarDefKind::VerilogVar,
            0x01 => VarDefKind::VerilogParameter,
            0x03 => VarDefKind::VerilogNet,
            0x04 => VarDefKind::VerilogNetWand,
            0x05 => VarDefKind::VerilogNetWor,
            0x06 => VarDefKind::VerilogNetTri,
            0x07 => VarDefKind::VerilogNetTriand,
            0x08 => VarDefKind::VerilogNetTrior,
            0x09 => VarDefKind::VerilogNetTri0,
            0x0A => VarDefKind::VerilogNetTri1,
            0x0B => VarDefKind::VerilogNetTrireg,
            0x0C => VarDefKind::VerilogNetSupply0,
            0x0D => VarDefKind::VerilogNetSupply1,
            0x0E => VarDefKind::VhdlSignal,
            0x0F => VarDefKind::VhdlVariable,
            0x12 => VarDefKind::VhdlGeneric,
            0x13 => VarDefKind::VhdlConstant,
            0x14 => VarDefKind::VhdlSubprogramParam,
            0x15 => VarDefKind::VhdlSignalParam,
            other => VarDefKind::Unknown(other),
        }
    }

    pub fn is_vhdl(self) -> bool {
        matches!(
            self,
            VarDefKind::VhdlSignal
                | VarDefKind::VhdlVariable
                | VarDefKind::VhdlGeneric
                | VarDefKind::VhdlConstant
                | VarDefKind::VhdlSubprogramParam
                | VarDefKind::VhdlSignalParam
        )
    }
}

/// Port mode (word 9 of variable definition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMode {
    Inout,
    In,
    Out,
    Buffer,
    Linkage,
    NotAPort,
}

impl PortMode {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => PortMode::Inout,
            1 => PortMode::In,
            2 => PortMode::Out,
            3 => PortMode::Buffer,
            4 => PortMode::Linkage,
            _ => PortMode::NotAPort,
        }
    }
}

/// Range record from Region 4 (6 × u32).
#[derive(Debug, Clone, Copy)]
pub struct RangeRecord {
    pub left: i64,
    pub right: i64,
    /// `1` = `to`, `-1` = `downto`.
    pub dir: i32,
    /// Reported distance between bounds (may be inaccurate for null ranges).
    pub distance: u32,
}

impl RangeRecord {
    pub fn element_count(self) -> usize {
        match self.dir {
            1 => self.right.saturating_sub(self.left).max(0) as usize + 1,
            -1 => self.left.saturating_sub(self.right).max(0) as usize + 1,
            _ => 0,
        }
    }
}

/// Variable definition record from Region 3 (11 × u32, padded to 8-byte boundary).
#[derive(Debug, Clone)]
pub struct VarDefRecord {
    /// Offset into variable definition string table.
    pub name_offset: u32,
    /// Index into value class entry table (Region 17).
    pub value_class_index: u32,
    pub file_index: u32,
    pub source_line: u32,
    /// Byte size for VHDL, bit size for Verilog.
    pub value_size: u32,
    /// Type index into the type table.
    pub type_index: u32,
    /// Number of range records.
    pub num_ranges: u32,
    /// Index of first range record (-1 if none).
    pub first_range_index: i32,
    pub kind: VarDefKind,
    pub port_mode: PortMode,
}

/// Variable record (§8) — 56 bytes each, bridges hierarchy to waveform data.
#[derive(Debug, Clone)]
pub struct VarRecord {
    /// Primary Signal ID.
    pub signal_id: u64,
    /// Second Signal ID (0 for generics/constants/variables).
    pub signal_id2: u64,
    /// Scope index.
    pub scope_index: u32,
    /// Byte/bit offset for port-slice binding (0 otherwise).
    pub slice_offset: u32,
    /// Storage class.
    pub storage_class: u32,
    /// Variable definition index.
    pub var_def_index: u64,
    /// Position in Verilog port list (0 for VHDL/non-ports).
    pub port_list_pos: u32,
}

impl VarRecord {
    pub fn arena_index(&self) -> u64 {
        self.signal_id >> 11
    }

    pub fn local_signal_id(&self) -> u64 {
        self.signal_id & 0x7FF
    }

    pub fn is_logged(&self) -> bool {
        // Storage class 2 = generic/constant/variable → no value change data.
        self.storage_class != 2 && self.signal_id != 0
    }
}

/// File table entry from Region 13.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub compiled_path: String,
    pub local_path: Option<String>,
}

/// The complete parsed netlist hierarchy section.
#[derive(Debug)]
pub struct NetlistHierarchy {
    /// Time unit exponent (power of 10 in seconds, e.g. -12 for ps).
    pub time_unit_exponent: i32,
    pub scopes: Vec<ScopeRecord>,
    pub scope_defs: Vec<ScopeDefRecord>,
    pub var_defs: Vec<VarDefRecord>,
    pub ranges: Vec<RangeRecord>,
    pub scope_strings: Vec<u8>,
    pub var_def_strings: Vec<u8>,
    pub files: Vec<FileEntry>,
    /// Variable records (§8) — one per elaborated variable.
    pub variables: Vec<VarRecord>,
    /// Logged variable ID ranges from the marker (§10).
    pub logged_ranges: Vec<(u64, u64)>,
}

impl NetlistHierarchy {
    /// Return the name string for a scope record.
    pub fn scope_name(&self, rec: &ScopeRecord) -> &str {
        read_cstr(&self.scope_strings, rec.name_offset as usize)
    }

    /// Return the entity name for a scope definition (empty string if absent).
    pub fn scope_def_entity_name(&self, rec: &ScopeDefRecord) -> &str {
        if rec.entity_name_offset < 0 {
            return "";
        }
        read_cstr(&self.scope_strings, rec.entity_name_offset as usize)
    }

    /// Return the variable definition name.
    pub fn var_def_name(&self, rec: &VarDefRecord) -> &str {
        read_cstr(&self.var_def_strings, rec.name_offset as usize)
    }

    /// Return range records for a variable definition.
    pub fn var_def_ranges(&self, rec: &VarDefRecord) -> &[RangeRecord] {
        if rec.first_range_index < 0 || rec.num_ranges == 0 {
            return &[];
        }
        let start = rec.first_range_index as usize;
        let end = start + rec.num_ranges as usize;
        &self.ranges[start.min(self.ranges.len())..end.min(self.ranges.len())]
    }

    /// Return variables belonging to a scope (contiguous slice starting at
    /// `first_var_id` for `num_vars` entries — the caller must know the count).
    ///
    /// The count is derived from the scope definition's `num_var_defs`.
    pub fn scope_variables(&self, scope_idx: usize) -> &[VarRecord] {
        let scope = &self.scopes[scope_idx];
        if scope.first_var_id < 0 {
            return &[];
        }
        let start = scope.first_var_id as usize;
        let def = &self.scope_defs[scope.scope_def_index as usize];
        let count = def.num_var_defs as usize;
        let end = (start + count).min(self.variables.len());
        &self.variables[start..end]
    }

    /// Check whether variable ID `var_id` is in the logged ranges.
    pub fn is_logged_var(&self, var_id: u64) -> bool {
        for &(first, last) in &self.logged_ranges {
            if var_id >= first && var_id <= last {
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse_hierarchy<R: Read + Seek>(
    r: &mut R,
    section_offset: u64,
    section_length: u64,
    marker_offset: u64,
    num_logged_ranges: u64,
) -> Result<NetlistHierarchy> {
    seek_to(r, section_offset)?;

    // --- Section framing ---
    // Magic: "Xilinx ISim DBG 006\0" (20 bytes)
    let mut magic_buf = [0u8; 20];
    r.read_exact(&mut magic_buf)?;
    check_magic(&magic_buf, "Xilinx ISim DBG 006")?;

    let _timestamp = read_u32_le(r)?; // offset 20
    let time_unit_exponent = read_i32_le(r)?; // offset 24

    // Region offsets (18 × u32) at offset 28.
    let mut region_offsets = [0u32; 18];
    for o in region_offsets.iter_mut() {
        *o = read_u32_le(r)?;
    }

    // Counts at offset 100 (4 × u32).
    let n_scopes = read_u32_le(r)? as usize;
    let _n_scope_defs = read_u32_le(r)? as usize; // == n_scopes
    let n_variables = read_u32_le(r)? as usize;
    let n_var_defs = read_u32_le(r)? as usize;

    // Header words at offset 116 (17 × u32).
    let mut header_words = [0u32; 17];
    for w in header_words.iter_mut() {
        *w = read_u32_le(r)?;
    }

    let n_ranges = header_words[0] as usize;
    let scope_strtab_len = header_words[5] as usize;
    let var_def_strtab_len = header_words[6] as usize;
    let file_strtab_len = header_words[7] as usize;
    let n_files = header_words[9] as usize;

    // Regions start at `section_offset + 184` (offset 184 in section).
    // Region offsets are relative to section start.
    let reg_base = section_offset;

    // Read scope records (Region 0) — 9 × u32 each.
    let scopes = read_scope_records(r, reg_base, &region_offsets, n_scopes)?;

    // Read scope definition records (Region 1) — 9 × u32 each.
    let scope_defs = read_scope_def_records(r, reg_base, &region_offsets, n_scopes)?;

    // Read variable definition records (Region 3) — 11 × u32 each.
    let var_defs = read_var_def_records(r, reg_base, &region_offsets, n_var_defs)?;

    // Read range records (Region 4) — 6 × u32 each.
    let ranges = read_range_records(r, reg_base, &region_offsets, n_ranges)?;

    // Read scope string table (Region 9).
    let scope_strings = read_strtab(r, reg_base, &region_offsets, 9, scope_strtab_len)?;

    // Read variable definition string table (Region 10).
    let var_def_strings = read_strtab(r, reg_base, &region_offsets, 10, var_def_strtab_len)?;

    // Read file string table (Region 11).
    let file_strings = read_strtab(r, reg_base, &region_offsets, 11, file_strtab_len)?;

    // Read file table (Region 13) — 2 × u32 per file.
    let files = read_file_table(r, reg_base, &region_offsets, n_files, &file_strings)?;

    // Variable records start at region_offsets[2] relative to section start.
    // (Region 2 is the end of the section proper; variable records begin there.)
    let var_records_offset = section_offset + region_offsets[2] as u64;
    let variables = read_var_records(r, var_records_offset, n_variables)?;

    // Marker — logged ranges.
    let logged_ranges = if marker_offset > 0 && num_logged_ranges > 0 {
        read_logged_ranges(r, marker_offset, num_logged_ranges)?
    } else {
        vec![]
    };

    Ok(NetlistHierarchy {
        time_unit_exponent,
        scopes,
        scope_defs,
        var_defs,
        ranges,
        scope_strings,
        var_def_strings,
        files,
        variables,
        logged_ranges,
    })
}

fn read_scope_records<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    n: usize,
) -> Result<Vec<ScopeRecord>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, base + offsets[0] as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let name_offset = read_u32_le(r)?;
        let parent_index = read_i32_le(r)?;
        let _zero = read_u32_le(r)?;
        let num_children = read_u32_le(r)?;
        let first_child_index = read_i32_le(r)?;
        let first_var_id = read_i32_le(r)?;
        let file_index = read_u32_le(r)?;
        let source_line = read_u32_le(r)?;
        let scope_def_index = read_u32_le(r)?;
        out.push(ScopeRecord {
            name_offset,
            parent_index,
            num_children,
            first_child_index,
            first_var_id,
            file_index,
            source_line,
            scope_def_index,
        });
    }
    Ok(out)
}

fn read_scope_def_records<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    n: usize,
) -> Result<Vec<ScopeDefRecord>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, base + offsets[1] as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let entity_name_offset = read_i32_le(r)?;
        let arch_name_offset = read_i32_le(r)?;
        let kind_raw = read_u32_le(r)?;
        let num_var_defs = read_u32_le(r)?;
        let _zero = read_u32_le(r)?;
        let arch_file_index = read_u32_le(r)?;
        let arch_source_line = read_u32_le(r)?;
        let entity_file_index = read_u32_le(r)?;
        let entity_source_line = read_u32_le(r)?;
        out.push(ScopeDefRecord {
            entity_name_offset,
            arch_name_offset,
            kind: ScopeKind::from_u32(kind_raw),
            num_var_defs,
            arch_file_index,
            arch_source_line,
            entity_file_index,
            entity_source_line,
        });
    }
    Ok(out)
}

fn read_var_def_records<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    n: usize,
) -> Result<Vec<VarDefRecord>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, base + offsets[3] as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let name_offset = read_u32_le(r)?;
        let value_class_index = read_u32_le(r)?;
        let file_index = read_u32_le(r)?;
        let source_line = read_u32_le(r)?;
        let value_size = read_u32_le(r)?;
        let type_index = read_u32_le(r)?;
        let num_ranges = read_u32_le(r)?;
        let first_range_index = read_i32_le(r)?;
        let kind_raw = read_u32_le(r)?;
        let port_mode_raw = read_u32_le(r)?;
        let _noise = read_u32_le(r)?;
        // 11 × u32 = 44 bytes; pad to 8-byte boundary: no extra padding needed
        // (spec says "padded to 8-byte boundary at end" of region, not per record).
        out.push(VarDefRecord {
            name_offset,
            value_class_index,
            file_index,
            source_line,
            value_size,
            type_index,
            num_ranges,
            first_range_index,
            kind: VarDefKind::from_u32(kind_raw),
            port_mode: PortMode::from_u32(port_mode_raw),
        });
    }
    Ok(out)
}

fn read_range_records<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    n: usize,
) -> Result<Vec<RangeRecord>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, base + offsets[4] as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let left_lo = read_u32_le(r)? as i64;
        let left_hi = read_i32_le(r)? as i64;
        let right_lo = read_u32_le(r)? as i64;
        let right_hi = read_i32_le(r)? as i64;
        let dir = read_i32_le(r)?;
        let distance = read_u32_le(r)?;
        let left = (left_hi << 32) | (left_lo & 0xFFFF_FFFF);
        let right = (right_hi << 32) | (right_lo & 0xFFFF_FFFF);
        out.push(RangeRecord { left, right, dir, distance });
    }
    Ok(out)
}

fn read_strtab<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    region_idx: usize,
    byte_len: usize,
) -> Result<Vec<u8>> {
    if byte_len == 0 { return Ok(vec![]); }
    // Align up to 8 bytes for the full region allocation, but we only need byte_len.
    seek_to(r, base + offsets[region_idx] as u64)?;
    read_bytes(r, byte_len)
}

fn read_file_table<R: Read + Seek>(
    r: &mut R,
    base: u64,
    offsets: &[u32; 18],
    n: usize,
    file_strings: &[u8],
) -> Result<Vec<FileEntry>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, base + offsets[13] as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let compiled_off = read_u32_le(r)? as usize;
        let local_raw = read_u32_le(r)?;
        let local_off = if local_raw == 0xFFFF_FFFF { None } else { Some(local_raw as usize) };
        let compiled_path = read_cstr(file_strings, compiled_off.min(file_strings.len().saturating_sub(1))).to_string();
        let local_path = local_off.map(|o| read_cstr(file_strings, o.min(file_strings.len().saturating_sub(1))).to_string());
        out.push(FileEntry { compiled_path, local_path });
    }
    Ok(out)
}

fn read_var_records<R: Read + Seek>(
    r: &mut R,
    offset: u64,
    n: usize,
) -> Result<Vec<VarRecord>> {
    if n == 0 { return Ok(vec![]); }
    seek_to(r, offset)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let signal_id = read_u64_le(r)?;       // +0
        let signal_id2 = read_u64_le(r)?;      // +8
        let scope_index = read_u32_le(r)?;     // +16
        let slice_offset = read_u32_le(r)?;    // +20
        let _zero = read_u32_le(r)?;           // +24
        let storage_class = read_u32_le(r)?;   // +28
        let var_def_index = read_u64_le(r)?;   // +32
        let port_list_pos = read_u32_le(r)?;   // +40
        let _unwritten = read_u32_le(r)?;      // +44 (ignore)
        let _zero2 = read_u64_le(r)?;          // +48
        out.push(VarRecord {
            signal_id,
            signal_id2,
            scope_index,
            slice_offset,
            storage_class,
            var_def_index,
            port_list_pos,
        });
    }
    Ok(out)
}

fn read_logged_ranges<R: Read + Seek>(
    r: &mut R,
    marker_offset: u64,
    n: u64,
) -> Result<Vec<(u64, u64)>> {
    seek_to(r, marker_offset)?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let first = read_u64_le(r)?;
        let last = read_u64_le(r)?;
        out.push((first, last));
    }
    Ok(out)
}
