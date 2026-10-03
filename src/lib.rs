//! WDB waveform database parser.
//!
//! Parses Xilinx/AMD Vivado `.wdb` files produced by `xsim`.
//!
//! # Quick start
//!
//! ```no_run
//! use wdb_parser::WdbFile;
//!
//! let wdb = WdbFile::open("sim.wdb").unwrap();
//! let hier = wdb.hierarchy();
//!
//! // Walk the scope tree from the root.
//! visit_scope(&hier, hier.root(), 0);
//!
//! fn visit_scope(hier: &wdb_parser::Hierarchy, scope: wdb_parser::ScopeRef, depth: usize) {
//!     let info = hier.scope(scope);
//!     println!("{}{}", "  ".repeat(depth), info.name);
//!     for &child in hier.child_scopes(scope) {
//!         visit_scope(hier, child, depth + 1);
//!     }
//!     for &var_ref in hier.scope_vars(scope) {
//!         let v = hier.var(var_ref);
//!         println!("{}  {} (signal_id={:?})", "  ".repeat(depth), v.name, v.signal_id);
//!     }
//! }
//! ```
//!
//! After opening, load signal changes:
//!
//! ```no_run
//! # use wdb_parser::WdbFile;
//! let mut wdb = WdbFile::open("sim.wdb").unwrap();
//! wdb.load_all_signals().unwrap();
//!
//! let hier = wdb.hierarchy();
//! for &var_ref in hier.scope_vars(hier.root()) {
//!     let v = hier.var(var_ref);
//!     if let Some(sid) = v.signal_id {
//!         if let Some(changes) = wdb.signal_changes(sid) {
//!             for (time, raw) in changes {
//!                 println!("t={} val={:?}", time, raw);
//!             }
//!         }
//!     }
//! }
//! ```

pub mod error;
pub mod waveform;
mod io;
mod header;
mod type_table;
mod hierarchy;

use std::collections::HashMap;
use std::io::{BufReader, Read, Seek};
use std::path::Path;
use std::fs::File;

pub use error::{Result, WdbError};
pub use type_table::{TypeEntry, TypeKind, TypeOrigin, RangeTriple};
pub use hierarchy::{
    ScopeKind, ScopeRecord, ScopeDefRecord, VarDefRecord, VarDefKind, VarRecord,
    PortMode, RangeRecord, FileEntry, NetlistHierarchy,
};
pub use waveform::{DecodedValue, LogicBit, PageDirectory, decode_verilog_value, decode_real};

// ---------------------------------------------------------------------------
// Public identifier types
// ---------------------------------------------------------------------------

/// An opaque reference to a scope in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeRef(pub usize);

/// An opaque reference to a variable in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarRef(pub usize);

/// A signal identifier that indexes into the waveform data.
///
/// `signal_id >> 11` = arena index, `signal_id & 0x7FF` = local signal ID.
///
/// (§3 — Arena definition; §8 — Signal ID structure)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignalId(pub u64);

impl SignalId {
    /// Arena index: `signal_id >> 11` — see §3 (Arena definition) and §8 (Signal ID structure).
    pub fn arena_index(self) -> u64 { self.0 >> 11 }
    /// Local Signal ID within the arena: `signal_id & 0x7FF` — see §3 (Arena definition) and §8 (Signal ID structure).
    pub fn local_signal_id(self) -> u64 { self.0 & 0x7FF }
}

// ---------------------------------------------------------------------------
// Scope and Var info structs (public-facing, richer than raw records)
// ---------------------------------------------------------------------------

/// Information about one scope in the elaborated hierarchy.
///
/// Combines §7.5 (Scope Records) and §7.6 (Scope Definition Records).
#[derive(Debug, Clone)]
pub struct ScopeInfo {
    /// Scope instance name — see §7.5 word 0 (name offset in scope string table).
    pub name: String,
    /// Full hierarchical path (dot-separated from root).
    pub path: String,
    /// Scope kind code — see §7.6 word 2.
    pub kind: ScopeKind,
    /// Entity/module name (empty if none) — see §7.6 word 0 (entity/module name offset).
    pub entity_name: String,
    /// Architecture/body name (empty if none) — see §7.6 word 1 (architecture name offset).
    pub arch_name: String,
    /// Source file path — see §7.5 words 6–7 (file index) and §7.9 (File Table).
    pub source_file: Option<String>,
    /// Source line number — see §7.5 word 7.
    pub source_line: u32,
    /// Internal index into the raw scope records array.
    pub raw_index: usize,
}

/// Information about one variable (signal, port, generic, etc.).
///
/// Combines §7.7 (Variable Definition Records) and §8 (Variable Records).
#[derive(Debug, Clone)]
pub struct VarInfo {
    pub name: String,
    /// Full hierarchical path including name.
    pub path: String,
    /// Variable kind code — see §7.7 word 8.
    pub kind: VarDefKind,
    /// Port direction mode — see §7.7 word 9.
    pub port_mode: PortMode,
    /// Bit width (Verilog) or byte size (VHDL) — see §7.7 word 4.
    pub value_size: u32,
    /// Index into the type table — see §7.7 word 5.
    pub type_index: u32,
    /// Primary Signal ID, if this variable has waveform data — see §8 offset 0.
    pub signal_id: Option<SignalId>,
    /// Storage class (0=signal/net, 1=language-boundary port, 2=constant/generic, …) — see §8 offset 28.
    pub storage_class: u32,
    /// Bit offset (Verilog) or byte offset (VHDL) for ports bound to a slice of a wider
    /// signal.  Zero for non-slice ports and all non-port variables — see §8 offset 20.
    pub slice_offset: u32,
    pub source_file: Option<String>,
    pub source_line: u32,
    pub ranges: Vec<RangeRecord>,
}

impl VarInfo {
    /// True if this variable's kind comes from a VHDL source.
    pub fn is_vhdl(&self) -> bool {
        self.kind.is_vhdl()
    }
}

// ---------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------

/// The elaborated design hierarchy — scope tree and variable list.
pub struct Hierarchy {
    scopes: Vec<ScopeInfo>,
    vars: Vec<VarInfo>,
    /// Root scope index (always 0 — the `_top` scope).
    root_idx: usize,
    /// scope_idx → list of child scope indices.
    children: Vec<Vec<ScopeRef>>,
    /// scope_idx → list of variable refs.
    scope_vars: Vec<Vec<VarRef>>,
    /// Reverse map: signal_id.0 → VarRef (for fast lookup during signal loading).
    sig_id_to_var: HashMap<u64, VarRef>,
    /// Raw netlist data (kept for access to string tables etc.).
    pub raw: NetlistHierarchy,
    /// Type entries.
    pub types: Vec<TypeEntry>,
}

impl Hierarchy {
    /// The root scope (`_top`).
    pub fn root(&self) -> ScopeRef {
        ScopeRef(self.root_idx)
    }

    /// Get scope information.
    pub fn scope(&self, r: ScopeRef) -> &ScopeInfo {
        &self.scopes[r.0]
    }

    /// Get variable information.
    pub fn var(&self, r: VarRef) -> &VarInfo {
        &self.vars[r.0]
    }

    /// Child scopes of a given scope.
    pub fn child_scopes(&self, r: ScopeRef) -> &[ScopeRef] {
        &self.children[r.0]
    }

    /// Variables declared directly in a given scope.
    pub fn scope_vars(&self, r: ScopeRef) -> &[VarRef] {
        &self.scope_vars[r.0]
    }

    /// All scopes as a flat slice (index = ScopeRef.0).
    pub fn all_scopes(&self) -> &[ScopeInfo] {
        &self.scopes
    }

    /// All variables as a flat slice (index = VarRef.0).
    pub fn all_vars(&self) -> &[VarInfo] {
        &self.vars
    }

    /// Look up a type entry by index.
    pub fn type_entry(&self, idx: usize) -> Option<&TypeEntry> {
        self.types.get(idx)
    }

    /// Look up the variable that owns a given `SignalId`.
    ///
    /// Returns `None` if the signal ID is not present in this design.
    ///
    /// (§8 — Variable Records / Signal ID; §3 — Arena definition)
    pub fn var_by_signal_id(&self, id: SignalId) -> Option<VarRef> {
        self.sig_id_to_var.get(&id.0).copied()
    }

    /// Walk the hierarchy depth-first, calling `f` for each scope.
    pub fn walk_scopes<F: FnMut(ScopeRef, &ScopeInfo)>(&self, mut f: F) {
        let mut stack = vec![ScopeRef(self.root_idx)];
        while let Some(sr) = stack.pop() {
            f(sr, self.scope(sr));
            for &child in self.child_scopes(sr).iter().rev() {
                stack.push(child);
            }
        }
    }

    /// Find a scope by its full dot-separated path (e.g. `"_top.tb.dut"`).
    pub fn find_scope(&self, path: &str) -> Option<ScopeRef> {
        self.scopes
            .iter()
            .enumerate()
            .find(|(_, s)| s.path == path)
            .map(|(i, _)| ScopeRef(i))
    }

    /// Find a variable by its full dot-separated path.
    pub fn find_var(&self, path: &str) -> Option<VarRef> {
        self.vars
            .iter()
            .enumerate()
            .find(|(_, v)| v.path == path)
            .map(|(i, _)| VarRef(i))
    }

    /// Return the scopes directly inside `scope`.
    ///
    /// If `scope` is `None`, returns the immediate children of the synthetic
    /// `_top` root — i.e. the top-level design units visible to the user.
    pub fn scopes_in(&self, scope: Option<ScopeRef>) -> &[ScopeRef] {
        let sr = scope.unwrap_or(ScopeRef(self.root_idx));
        self.child_scopes(sr)
    }

    /// Return the variables declared directly inside `scope`.
    ///
    /// If `scope` is `None`, returns the variables of the synthetic `_top`
    /// root scope (typically empty, but present for completeness).
    pub fn vars_in(&self, scope: Option<ScopeRef>) -> &[VarRef] {
        let sr = scope.unwrap_or(ScopeRef(self.root_idx));
        self.scope_vars(sr)
    }
}

// ---------------------------------------------------------------------------
// Time information
// ---------------------------------------------------------------------------

/// Time unit of the simulation, encoded as a power of 10 in seconds.
///
/// (§7.1 offset 24 — `int32` time unit exponent; Appendix A — timescale encoding)
#[derive(Debug, Clone, Copy)]
pub struct TimeUnit {
    /// Exponent: -12 = picoseconds, -9 = nanoseconds, etc. — see §7.1 offset 24 and Appendix A.
    pub exponent: i32,
}

impl TimeUnit {
    pub fn as_str(&self) -> &'static str {
        match self.exponent {
            -15 => "fs",
            -12 => "ps",
            -9 => "ns",
            -6 => "us",
            -3 => "ms",
            0 => "s",
            _ => "?",
        }
    }
}

impl std::fmt::Display for TimeUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "10^{} s ({})", self.exponent, self.as_str())
    }
}

// ---------------------------------------------------------------------------
// WdbFile — main entry point
// ---------------------------------------------------------------------------
/// A parsed WDB waveform database file.
///
/// The type parameter `R` is the underlying seekable reader (e.g.
/// `BufReader<File>` or `Cursor<Vec<u8>>`).  Use [`WdbFile::open`] to open a
/// file on disk, [`WdbFile::from_bytes`] for an in-memory buffer, or
/// [`WdbFile::from_reader`] for any type that implements [`Read`] + [`Seek`].
///
/// Signal waveform data is loaded on demand via [`WdbFile::load_signals`].
/// The underlying reader is kept open and seeked as needed; the entire file is
/// **never** read into memory at once.
pub struct WdbFile<R: Read + Seek> {
    hier: Hierarchy,
    time_unit: TimeUnit,
    end_time: u64,
    /// Live seekable handle to the source.
    reader: R,
    /// Page directory (arena_idx → page refs).  Small — fits in memory.
    page_dir: PageDirectory,
    /// Inflated page size in bytes (always 10240 per spec).
    page_size: usize,
    /// Loaded signal changes: global_signal_id → sorted [(time, raw_bytes)].
    signal_changes: HashMap<u64, Vec<(u64, Vec<u8>)>>,
}

impl WdbFile<BufReader<File>> {
    /// Open and parse a `.wdb` file from disk.
    ///
    /// Reads only the header, type table, hierarchy, and page directory.
    /// Value pages are not read until [`load_signals`] is called.
    ///
    /// Not available on WebAssembly targets (no filesystem access).
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let f = File::open(path)?;
        WdbFile::from_reader(BufReader::new(f))
    }
}

impl WdbFile<std::io::Cursor<Vec<u8>>> {
    /// Parse a WDB file from an in-memory byte buffer.
    ///
    /// Useful for tests or when the caller has already loaded the file.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        WdbFile::from_reader(std::io::Cursor::new(data))
    }
}

impl<R: Read + Seek> WdbFile<R> {
    /// Parse a WDB file from any [`Read`] + [`Seek`] source.
    ///
    /// This is the primary constructor for WebAssembly and other environments
    /// without direct filesystem access; see §7.1 (Section Framing) for the
    /// on-disk layout consumed here.  Pass any type that implements both
    /// traits — for example:
    ///
    /// ```no_run
    /// # use std::io::{BufReader, Cursor};
    /// # use wdb_parser::WdbFile;
    /// // From a file (buffered):
    /// let f = std::fs::File::open("sim.wdb").unwrap();
    /// let wdb = WdbFile::from_reader(BufReader::new(f)).unwrap();
    ///
    /// // From a byte slice already in memory:
    /// let bytes: Vec<u8> = std::fs::read("sim.wdb").unwrap();
    /// let wdb = WdbFile::from_reader(Cursor::new(bytes)).unwrap();
    /// ```
    pub fn from_reader(mut reader: R) -> Result<Self> {
        // Parse file metadata (header, trailer, arena table, directory entries).
        let meta = header::parse_file_metadata(&mut reader)?;

        // Parse type table.
        let types = type_table::parse_type_table(
            &mut reader,
            meta.type_table_dir.section_offset,
        )?;

        // Parse netlist hierarchy + variable records + marker.
        let raw_hier = hierarchy::parse_hierarchy(
            &mut reader,
            meta.hierarchy_dir.section_offset,
            meta.hierarchy_dir.section_length,
            meta.trailer.marker_file_offset,
            meta.trailer.num_logged_ranges,
        )?;

        // Page directory base = hierarchy dir entry offset + 48.
        let page_dir_base = meta.header.hierarchy_dir_offset + 48;
        let page_dir = waveform::parse_page_directory(
            &mut reader,
            &meta.arena_table,
            page_dir_base,
        )?;

        let time_unit = TimeUnit {
            exponent: raw_hier.time_unit_exponent,
        };
        let end_time = meta.trailer.end_time;
        let page_size = meta.trailer.page_size as usize;

        // Build the public Hierarchy from the raw records.
        let hier = build_hierarchy(raw_hier, types)?;

        Ok(WdbFile {
            hier,
            time_unit,
            end_time,
            reader,
            page_dir,
            page_size,
            signal_changes: HashMap::new(),
        })
    }

    /// The simulation time unit.
    pub fn time_unit(&self) -> TimeUnit {
        self.time_unit
    }

    /// The simulation end time (in the file's time unit).
    pub fn end_time(&self) -> u64 {
        self.end_time
    }

    /// The elaborated design hierarchy.
    pub fn hierarchy(&self) -> &Hierarchy {
        &self.hier
    }

    /// Load value change data for the given signals.
    ///
    /// Only pages from arenas that contain at least one requested signal are
    /// decompressed.  Within each such page, only records whose local signal
    /// ID falls within the signal's allocated slot range are retained.
    ///
    /// Calling this multiple times with different signal sets is cumulative —
    /// previously loaded data is kept.  Call [`clear_signals`] first if you
    /// need to release memory from a prior load.
    ///
    /// (§9 — Page Directory; §11 — Value Pages; §14 — Wide Values/Chunking)
    ///
    /// # Example
    /// ```no_run
    /// # use wdb_parser::{WdbFile, SignalId};
    /// let mut wdb = WdbFile::open("sim.wdb").unwrap();
    /// let hier = wdb.hierarchy();
    /// let signals: Vec<SignalId> = hier
    ///     .vars_in(None)          // top-level vars
    ///     .iter()
    ///     .filter_map(|&vr| hier.var(vr).signal_id)
    ///     .collect();
    /// wdb.load_signals(&signals).unwrap();
    /// ```
    pub fn load_signals(&mut self, signals: &[SignalId]) -> Result<()> {
        // ---------------------------------------------------------------
        // Step 1: group requested signals by arena, computing the local
        // signal ID range each occupies.
        //
        // A signal's records always land in the range
        //   [local_base, local_base + slot_span)
        // where slot_span is:
        //   VHDL   → round_up_to_8(value_size_bytes)
        //   Verilog → ceil(bit_width / 32) * 8   (the on-disk record size)  §12.2
        //
        // For wide signals (≥ 275 bytes) that are split into chunks, each
        // chunk offset is also within this range, so the same filter works. §14.1
        // ---------------------------------------------------------------
        let mut arena_to_ranges: HashMap<u64, Vec<(u64, u64)>> = HashMap::new();

        for &sig in signals {
            let local_base = sig.local_signal_id();
            let slot_span = self.signal_slot_span(sig);
            let arena = sig.arena_index();
            let end = local_base + slot_span;

            if end <= 0x800 {
                // Simple case: all within one arena.
                arena_to_ranges
                    .entry(arena)
                    .or_default()
                    .push((local_base, end));
            } else {
                // Signal crosses the arena boundary at 0x800. §14.3
                // Part 1: [local_base, 0x800) in the primary arena.
                // Part 2: [0, end - 0x800) in the next arena.
                arena_to_ranges
                    .entry(arena)
                    .or_default()
                    .push((local_base, 0x800));
                arena_to_ranges
                    .entry(arena + 1)
                    .or_default()
                    .push((0, end - 0x800));
            }
        }

        // ---------------------------------------------------------------
        // Step 2: for each arena of interest, read and filter its pages.
        // ---------------------------------------------------------------
        for (arena_idx, ranges) in &arena_to_ranges {
            let pages = match self.page_dir.get(arena_idx) {
                Some(p) => p.clone(), // PageRef is cheap to clone (two u64s)
                None => continue,
            };
            for page_ref in &pages {
                let records =
                    waveform::read_page(&mut self.reader, page_ref, self.page_size)?;
                for rec in records {
                    let lid = rec.local_signal_id;
                    if ranges.iter().any(|&(lo, hi)| lid >= lo && lid < hi) {
                        let global_id = (arena_idx << 11) | lid;
                        self.signal_changes
                            .entry(global_id)
                            .or_default()
                            .push((rec.time, rec.value));
                    }
                }
            }
        }

        // ---------------------------------------------------------------
        // Step 3: sort newly added changes by time (dedup on time is NOT
        // done — the caller may need multiple records at the same time for
        // delta-cycle or multi-driver signals).
        // ---------------------------------------------------------------
        for changes in self.signal_changes.values_mut() {
            changes.sort_by_key(|(t, _)| *t);
        }

        Ok(())
    }

    /// Convenience: load every logged signal in the file.
    ///
    /// For large files this can use significant memory and time.  Prefer
    /// [`load_signals`] with a specific list when only some signals are needed.
    pub fn load_all_signals(&mut self) -> Result<()> {
        let signals: Vec<SignalId> = self
            .hier
            .vars
            .iter()
            .filter_map(|v| v.signal_id)
            .collect();
        self.load_signals(&signals)
    }

    /// Drop all loaded value change data, freeing the associated memory.
    pub fn clear_signals(&mut self) {
        self.signal_changes.clear();
    }

    /// Return the raw (time, value_bytes) changes for a signal by its Signal ID.
    ///
    /// Returns `None` if this signal has not been loaded yet or has no data.
    ///
    /// (§11.2 — Value Change Records)
    pub fn signal_changes(&self, id: SignalId) -> Option<&[(u64, Vec<u8>)]> {
        self.signal_changes.get(&id.0).map(|v| v.as_slice())
    }

    /// Return assembled value changes for a variable, handling cross-arena boundary splits.
    ///
    /// When a VHDL or wide Verilog signal's slot range crosses the arena boundary (i.e.
    /// `local_signal_id + slot_span > 0x800`), the simulator writes part of the value
    /// into the next arena starting at local 0.  This method retrieves both pieces and
    /// concatenates them in byte order to reconstruct the full value.
    ///
    /// For signals that fit within one arena this is equivalent to `signal_changes`.
    /// Returns `None` if the primary data has not been loaded.
    ///
    /// (§14.3 — Arena Boundary Splits; §14.4 — Reassembly Algorithm)
    pub fn assembled_signal_changes(&self, var: &VarInfo) -> Option<Vec<(u64, Vec<u8>)>> {
        let sid = var.signal_id?;
        let local_base = sid.local_signal_id();
        let slot_span = self.signal_slot_span(sid);

        if local_base + slot_span <= 0x800 {
            // Simple: no cross-arena split.
            return self.signal_changes(sid).map(|c| c.to_vec());
        }

        // Cross-arena: primary piece at arena 0 local [local_base, 0x800),
        // overflow piece at arena+1 local 0.
        let primary = self.signal_changes(sid)?;
        if primary.is_empty() {
            return Some(vec![]);
        }

        // Overflow global_id = ((arena + 1) << 11) | 0
        let overflow_gid = SignalId(((sid.arena_index() + 1) << 11) | 0);
        let overflow_changes = self.signal_changes(overflow_gid);

        // Build timestamp → overflow_bytes lookup.
        let mut overflow_map: std::collections::HashMap<u64, Vec<u8>> =
            std::collections::HashMap::new();
        if let Some(changes) = overflow_changes {
            // Keep the last record per timestamp (mirrors the main dedup logic).
            for (t, raw) in changes {
                overflow_map.insert(*t, raw.clone());
            }
        }

        // Assemble.
        let mut result: Vec<(u64, Vec<u8>)> = Vec::with_capacity(primary.len());
        for (time, primary_raw) in primary {
            let mut full = primary_raw.clone();
            if let Some(overflow_raw) = overflow_map.get(time) {
                full.extend_from_slice(overflow_raw);
            }
            result.push((*time, full));
        }
        Some(result)
    }

    /// Decode a raw value change record.
    ///
    /// `is_verilog`: use val/xz pair decoding (Verilog); otherwise treat as raw bytes (VHDL).
    pub fn decode_value(
        &self,
        raw: &[u8],
        is_verilog: bool,
        bit_width: u32,
    ) -> DecodedValue {
        if is_verilog {
            waveform::decode_verilog_value(raw, bit_width)
        } else {
            DecodedValue::Binary(raw.to_vec())
        }
    }

    /// Convenience: get all decoded value changes for a variable.
    pub fn var_changes(&self, var_ref: VarRef) -> Vec<(u64, DecodedValue)> {
        let var = self.hier.var(var_ref);
        let sid = match var.signal_id {
            Some(s) => s,
            None => return vec![],
        };
        let raw_changes = match self.signal_changes(sid) {
            Some(c) => c,
            None => return vec![],
        };
        let is_verilog = !var.is_vhdl();
        raw_changes
            .iter()
            .map(|(t, raw)| {
                let decoded = self.decode_value(raw, is_verilog, var.value_size);
                (*t, decoded)
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Compute the number of local signal ID slots a signal occupies.
    ///
    /// This determines the range `[local_base, local_base + slot_span)` in
    /// which all value change records (including partial-write chunks) for this
    /// signal will appear.
    ///
    /// (§12.2 — Verilog on-disk record size = 8×ceil(bits/32); §14.1 — chunk threshold 275 bytes)
    fn signal_slot_span(&self, sig: SignalId) -> u64 {
        self.hier
            .var_by_signal_id(sig)
            .map(|vr| {
                let v = self.hier.var(vr);
                if v.is_vhdl() {
                    // VHDL: value_size is in bytes.  Slots are 8-byte aligned.
                    ((v.value_size as u64).max(1) + 7) / 8 * 8
                } else {
                    // Verilog: on-disk record size = 8 × ceil(bit_width / 32).
                    ((v.value_size as u64).max(1) + 31) / 32 * 8
                }
            })
            .unwrap_or(8) // unknown signal → assume one 8-byte slot
    }
}

// ---------------------------------------------------------------------------
// Hierarchy builder
// ---------------------------------------------------------------------------

fn build_hierarchy(raw: NetlistHierarchy, types: Vec<TypeEntry>) -> Result<Hierarchy> {
    let n_scopes = raw.scopes.len();
    let n_vars = raw.variables.len();

    // --- Build scope info ---
    let mut scope_infos: Vec<ScopeInfo> = Vec::with_capacity(n_scopes);
    // Compute paths by following parent links (BFS / topological order).
    // Since scope records are numbered 0..n with parent always < child index
    // (the spec says children come after parents in the ordering), we can
    // process them in index order.
    for (i, sc) in raw.scopes.iter().enumerate() {
        let name = raw.scope_name(sc).to_string();
        let path = if sc.parent_index < 0 {
            name.clone()
        } else {
            let parent_path = &scope_infos[sc.parent_index as usize].path;
            format!("{}.{}", parent_path, name)
        };
        let def = &raw.scope_defs[sc.scope_def_index as usize];
        let entity_name = if def.entity_name_offset >= 0 {
            raw_scope_cstr(&raw.scope_strings, def.entity_name_offset as usize).to_string()
        } else {
            String::new()
        };
        let arch_name = if def.arch_name_offset >= 0 {
            raw_scope_cstr(&raw.scope_strings, def.arch_name_offset as usize).to_string()
        } else {
            String::new()
        };
        let source_file = raw
            .files
            .get(sc.file_index as usize)
            .map(|f| f.local_path.clone().unwrap_or(f.compiled_path.clone()));
        scope_infos.push(ScopeInfo {
            name,
            path,
            kind: def.kind,
            entity_name,
            arch_name,
            source_file,
            source_line: sc.source_line,
            raw_index: i,
        });
    }

    // --- Build children map ---
    let mut children: Vec<Vec<ScopeRef>> = vec![vec![]; n_scopes];
    for (i, sc) in raw.scopes.iter().enumerate() {
        if sc.parent_index >= 0 {
            let parent = sc.parent_index as usize;
            if parent < n_scopes {
                children[parent].push(ScopeRef(i));
            }
        }
    }

    // --- Build variable infos + reverse signal_id map ---
    let mut var_infos: Vec<VarInfo> = Vec::with_capacity(n_vars);
    let mut scope_var_map: Vec<Vec<VarRef>> = vec![vec![]; n_scopes];
    let mut sig_id_to_var: HashMap<u64, VarRef> = HashMap::new();

    for (var_idx, var_rec) in raw.variables.iter().enumerate() {
        let def_idx = var_rec.var_def_index as usize;
        let def = raw.var_defs.get(def_idx).ok_or(WdbError::VarIndexOutOfBounds(def_idx))?;

        let name = raw.var_def_name(def).to_string();
        let scope_idx = var_rec.scope_index as usize;
        let scope_path = if scope_idx < scope_infos.len() {
            &scope_infos[scope_idx].path
        } else {
            "_unknown"
        };
        let path = format!("{}.{}", scope_path, name);

        let signal_id = if var_rec.signal_id != 0 {
            Some(SignalId(var_rec.signal_id))
        } else {
            None
        };

        let ranges = raw.var_def_ranges(def).to_vec();
        let source_file = raw
            .files
            .get(def.file_index as usize)
            .map(|f| f.local_path.clone().unwrap_or(f.compiled_path.clone()));

        let vr = VarRef(var_idx);
        if scope_idx < n_scopes {
            scope_var_map[scope_idx].push(vr);
        }
        if let Some(sid) = signal_id {
            sig_id_to_var.insert(sid.0, vr);
        }

        var_infos.push(VarInfo {
            name,
            path,
            kind: def.kind,
            port_mode: def.port_mode,
            value_size: def.value_size,
            type_index: def.type_index,
            signal_id,
            storage_class: var_rec.storage_class,
            slice_offset: var_rec.slice_offset,
            source_file,
            source_line: def.source_line,
            ranges,
        });
    }

    Ok(Hierarchy {
        scopes: scope_infos,
        vars: var_infos,
        root_idx: 0,
        children,
        scope_vars: scope_var_map,
        sig_id_to_var,
        raw,
        types,
    })
}

fn raw_scope_cstr(strings: &[u8], offset: usize) -> &str {
    io::read_cstr(strings, offset.min(strings.len().saturating_sub(1)))
}
