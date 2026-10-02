# WDB Format Specification

**Xilinx Waveform Database** — Binary format produced by Vivado xsim (the Xilinx/AMD FPGA simulator). Written by `xsim` when you include `log_wave -r /` in the Tcl batch file; without that call the file contains only design metadata and no waveform data.

- **Extension**: `.wdb`
- **Endianness**: little-endian throughout
- **Integer types**: `uint32` / `uint64` / `int32` unless noted otherwise
- **Magic**: `Xilinx WAVE DATABASE 01\0` at offset 0x00

---

## Conceptual Structure

Like all waveform dump formats, a WDB file has three logical layers:

**Metadata** — who produced it, when, and where everything is:
- Magic string and tool name identifying the producer (`Xilinx Simulator`)
- Unix timestamp recording when the file was written
- Directory entry pointers locating the three major sections
- Trailer fields: simulation end time, page size, arena slot count, marker offset

**Netlist Hierarchy** — the complete elaborated design structure:
- Type table: all signal types (enumerations, integers, reals, arrays, records, Verilog vectors, etc.)
- Netlist hierarchy section: the scope tree (scopes, scope definitions, source locations), variable definitions, port modes, and source file references
- Variable records: one per elaborated variable, binding each variable definition to a Signal ID

**Value Change Data** — the actual waveform:
- Arena table and page directory: the index mapping variables to their compressed value pages
- Marker: which variables have any recorded values (the logged ranges)
- Value pages: zlib-compressed streams of `(time, local_signal_id, value)` value change records

### Key design property: hierarchy and waveform data are fully decoupled

The netlist hierarchy is self-contained and complete regardless of whether any signals were logged. A parser can read the full scope tree, all type information, and every variable definition without touching a single value page. Conversely, value pages can be decoded knowing only the arena/Signal ID mapping — no type information is required to extract raw bytes.

The **variable records** are the bridge between the two layers. Each variable record holds the Signal ID that links a variable definition in the hierarchy to a position in the arena/page system. The hierarchy tells you what a signal is; the variable record tells you where its values live; the value pages tell you what values it took over time.

---

## Table of Contents

1. [High-Level File Layout](#1-high-level-file-layout)
2. [Fixed Header (0x00–0xC7)](#2-fixed-header)
3. [Arena Table (0xC8–trailer)](#3-arena-table)
4. [Trailer (0x48 bytes)](#4-trailer)
5. [Directory Entries](#5-directory-entries)
6. [Type Table — `Xilinx ISim TYPE FILE 001`](#6-type-table)
7. [Netlist Hierarchy Section — `Xilinx ISim DBG 006`](#7-netlist-hierarchy-section)
8. [Variable Records](#8-variable-records)
9. [Page Directory](#9-page-directory)
10. [Marker — Logged Ranges](#10-marker--logged-ranges)
11. [Value Pages](#11-value-pages)
12. [Value Encodings](#12-value-encodings)
13. [Verilog / SystemVerilog Specifics](#13-verilog--systemverilog-specifics)
14. [Wide Values and Chunking](#14-wide-values-and-chunking)
15. [VHDL Partial Writes](#15-vhdl-partial-writes)
16. [Variable Value Change Summary](#16-variable-value-change-summary)

---

## 1. High-Level File Layout

A WDB file is a single flat binary. The sections appear in this order:

| Layer | Section | Location | Contents |
|:---|:---|:---|:---|
| Metadata | Fixed header | `0x00`–`0xC7` | Magic, tool string, timestamps, directory pointers |
| Metadata | Arena table | `0xC8`–trailer | `uint64` file offsets of arena page-directory records |
| Metadata | Trailer | 0x48 bytes | End time, marker offset, page size, slot count |
| Metadata | Directory entries | After trailer | Three 48-byte entries: `WDB.Event`, `Xilinx RTTI`, `Xilinx DBG` |
| Hierarchy | Type table | Starts at `Xilinx ISim TYPE FILE 001` | Signal type definitions |
| Hierarchy | Netlist hierarchy section | Starts at `Xilinx ISim DBG 006` | Scopes, scope definitions, variable definitions, source locations |
| Hierarchy | Variable records | Immediately after netlist hierarchy section | One 56-byte record per variable; holds the Signal ID |
| Waveform | Page directory | After `Xilinx DBG` directory entry | Arena records listing page offsets and lengths |
| Waveform | Marker | 16 bytes | Logged range count + file offset |
| Waveform | Value pages | zlib streams | Compressed waveform data |

The marker sits between the page directory and the first value page in a small simulation. When a page is flushed before the simulation ends, that page appears before the marker.

**To decode the file without the `xsim.dir` tree:** all needed data is self-contained in the `.wdb` file.

---

## 2. Fixed Header

All offsets are from the start of the file.

| Offset | Len    | Type      | Contents |
|:-------|--------|:----------|:---------|
| `0x00` | 24     | ASCII     | Magic: `Xilinx WAVE DATABASE 01\0` (NUL-terminated) |
| `0x18` | 24     | ASCII     | Tool string: `Xilinx Simulator\0` (zero-padded) |
| `0x30` | 8      | uint64    | Constant `0x40` (header size / start of extended header) |
| `0x38` | 4      | uint32    | Unix timestamp (seconds since epoch) — file write time |
| `0x3C` | 4      | bytes     | Unknown |
| `0x40` | 8      |     —     | Reserved, always `0` |
| `0x48` | 8      | uint64    | File offset of directory entry [§5](#5-directory-entries) `WDB.Event` (Trailer) — that entry describes the trailer and is used to locate it |
| `0x50` | 8      | uint64    | File offset of directory entry [§5](#5-directory-entries) `Xilinx RTTI` (Type Table) — points to the 48-byte struct describing the RTTI section |
| `0x58` | 8      | uint64    | File offset of directory entry [§5](#5-directory-entries) `Xilinx DBG` (Hierarchy) — points to the 48-byte struct describing the netlist hierarchy section |
| `0x60` | 56     |     —     | Unknown; always `0` in all observed cases |
| `0x98` | 12     | 3× uint32 | Constant `0x30 0x30 0x30` |
| `0xC0` | 4      | uint32    | Constant `3` (number of sections) |
| `0xC4` | 4      | uint32    | Per-run duration value — noise |
| `0xC8` | varies | uint64[]  | Arena table (continues until trailer) |

The pointer at `0x48` points to the `WDB.Event` directory entry. That entry sits immediately *after* the trailer — at `trailer_offset + 0x48` — because a directory entry always follows the section it describes, and the `WDB.Event` entry describes the trailer (`length = 0x48`). So the trailer offset is:

```
trailer_offset = wdb_event_pointer - 0x48
```

And the arena table, which runs from `0xC8` to the start of the trailer, has:

```
arena_table_length_bytes = trailer_offset - 0xC8
slot_count               = arena_table_length_bytes / 8
```

---

## 3. Arena Table

Beginning at `0xC8`, the file holds a sequence of `uint64` slots. Each slot is either:
- A non-zero file offset pointing to the corresponding arena record in the page directory, or
- `0` if no variables were logged in that arena.

**Arena definition:** An arena covers a window of `0x800` (2048) Signal IDs. Given a Signal ID `s`:
- Arena index = `s >> 11`
- Local Signal ID within the arena = `s & 0x7FF`

The slot count equals `ceil(signal_id_space / 0x800)`, where `signal_id_space` is the trailer field at `+0x18`.

Slots can be `0` in the middle of the table (e.g., if only some arenas were written). Slot order does not match the order the arena records appear in the page directory — records appear in the order arenas were first written.

---

## 4. Trailer

The trailer is `0x48` bytes long and follows the arena table. Offsets are relative to the start of the trailer.

| Offset  | Len | Type   | Contents |
|:--------|---:|:---------|:---|
| `+0x00` | 8 | `uint64` | Simulation end time in the file's time unit |
| `+0x08` | 4 | `uint32` | Unknown - Constant `0x3E9` |
| `+0x0C` | 4 | `uint32` | Number of arena table slots |
| `+0x10` | 8 | `uint64` | Constant `0x800` (arena span — Signal IDs per arena) |
| `+0x18` | 8 | `uint64` | Signal ID space: total Signal ID address space allocated |
| `+0x20` | 4 | `uint32` | Constant `0xC8` (arena table start offset) |
| `+0x24` | 4 | `uint32` | Unknown - Constant `0` |
| `+0x28` | 8 | `uint64` | Unknown - Constant `0` |
| `+0x30` | 8 | `uint64` | Number of logged ranges at the marker (`0` if nothing logged) |
| `+0x38` | 8 | `uint64` | File offset of the marker (`0` if nothing logged) |
| `+0x40` | 4 | `uint32` | Constant `0x2800` — inflated page size (10240 bytes) |
| `+0x44` | 4 | `uint32` | Constant `0x64` |

---

## 5. Directory Entries

Each pointer at header offset `0x48` points to a 48-byte entry of the form:

> Mind the 0x48 offset and 48 byte struct. Those are 2 different things!

| Offset | Len |  Type  | Contents |
|:-------|-----|:-------|:---------|
| `0`    | 24  | ASCII  | Entry name (NUL-terminated) |
| `24`   | 8   | uint64 | Count (usually `1`) |
| `32`   | 8   | uint64 | File offset of section start |
| `40`   | 8   | uint64 | Section length in bytes |

> [!IMPORTANT]  
> Some sources claim that the order of these entries in the table is not guaranteed. Use the entry name to identify the section.

|   Entry name  | Count | Section           | Length |
|:--------------|------:|:------------------|:-------|
| `WDB.Event`   |   1   | Trailer           | `0x48` — despite its name, this entry describes only the trailer; it exists solely to allow a parser to locate and validate the trailer |
| `Xilinx RTTI` |   1   | Type table        | Length through the type table's offset list |
| `Xilinx DBG`  |   1   | Netlist Hierarchy | Netlist hierarchy section + variable records |

Each entry sits at `offset + length` — directly after the section it describes. The page directory immediately follows the `Xilinx DBG` entry.

---

## 6. Type Table

**Magic:** `Xilinx ISim TYPE FILE 001\0` (26 bytes)

### 6.1 Framing

| Offset | Len | Contents |
|:---|----|:---|
| `0` | 26 | Magic string (NUL-terminated) |
| `26` | 2 | `uint16` noise |
| `28` | 4 | `uint32` Unix timestamp (noise) |
| `32` | 4 | `uint32` number of type entries (`n_types`) |
| `36` | 4 | `uint32` offset from magic where entries end |
| `40` | varies | Entries back-to-back |
| end | 8 × `n_types` | `uint64` offsets of each entry (from the magic) |

Each entry has the form:

```
[uint32 length][uint32 tag] name NUL body
```

- `length` covers the entire entry.
- The low byte of `tag` is the **kind code** (see below). The high bytes are always `0xA0`.
- The first word of `body` is the **origin** (source language indicator).
- The offset list at the end is sorted; a parser should verify that it names every entry.

### 6.2 Origin Word Values

| Value | Meaning |
|---:|:---|
| `0x02` | VHDL type (not `TIME`) |
| `0x0A` | VHDL `TIME` |
| `0x01` | Verilog unnamed/user type: vector, struct, enum, typedef |
| `0x05` | Verilog predefined scalar: `logic`, `bit`, `real`, `scalar_int`, `integer`, `int`, `byte`, `longint` |
| `0x0D` | Verilog `time` |

### 6.3 Kind Codes and Entry Bodies

| Kind | Name | Languages | Notes |
|:---|:---|:---|:---|
| `0x03` | [Enumeration](#kind-0x03--enumeration) | VHDL, Verilog | `BIT`, `BOOLEAN`, `STD_ULOGIC`, `logic`, `bit` |
| `0x04` | [Named values (SV enum)](#kind-0x04--named-values-systemverilog-enum) | SystemVerilog | Named integer enum with explicit values |
| `0x05` | [Integer](#kind-0x05--integer) | VHDL, Verilog | Bounded integer or Verilog packed integer type |
| `0x06` | [Real](#kind-0x06--real) | VHDL, Verilog | IEEE 754 float; Verilog has no bounds |
| `0x07` | [Alias (typedef)](#kind-0x07--alias-typedef) | VHDL, Verilog | Names a base type; may carry array bounds |
| `0x08` | [Access (pointer)](#kind-0x08--access-pointer-type) | VHDL | Declares 48 bytes; no value change records |
| `0x0C` | [File](#kind-0x0c--file-type) | VHDL | Declares 0 bytes; no value change records |
| `0x0D` | [Physical](#kind-0x0d--physical-vhdl-timeuser-physical) | VHDL | Time or user-defined physical type with named units |
| `0x10` | [Array](#kind-0x10--array) | VHDL, Verilog | Packed or unpacked; may be unconstrained |
| `0x11` | [Record / struct](#kind-0x11--record--struct) | VHDL, Verilog | Named fields; VHDL record or SV struct |
| `0x13` | [Dynamic array](#kinds-0x130x18--dynamic-types-under--debug-all-only) | SystemVerilog | `int d[]`; `-debug all` only |
| `0x14` | [Queue](#kinds-0x130x18--dynamic-types-under--debug-all-only) | SystemVerilog | `int q[$]`; `-debug all` only |
| `0x15` | [Associative array](#kinds-0x130x18--dynamic-types-under--debug-all-only) | SystemVerilog | `int a[key]`; `-debug all` only |
| `0x17` | [Class](#kinds-0x130x18--dynamic-types-under--debug-all-only) | SystemVerilog | `-debug all` only |
| `0x18` | [String](#kinds-0x130x18--dynamic-types-under--debug-all-only) | SystemVerilog | `-debug all` only |

#### Kind `0x03` — Enumeration

```
[u32 origin][u32 variant][u32 class][u32 n]  <n NUL-terminated literal names>  [u32 size]
```

- `size`: byte size of an encoded value (`1` for ≤256 literals, `4` for ≥257 literals, `0` for Verilog).
- VHDL `variant` is always `2`. Verilog `logic` has variant `0`, `bit` has variant `1`.
- `class` encodes the literal shape (not the type name):
  - `2` — `'0'` and `'1'` literals (like `BIT`)
  - `3` — nine `STD_ULOGIC` literals
  - `4` — any set with a character literal
  - `5` — identifiers only (like `BOOLEAN`)

#### Kind `0x04` — Named Values (SystemVerilog enum)

```
[u32 origin][u32 base_type_index][u32 n][u32 8]
<n × (name NUL [u64 value])>
[u32 nranges]  <nranges × range triple>
```

No trailing `-99`. The `typedef` alias entry (kind `0x07`) points to this entry.

#### Kind `0x05` — Integer

```
[u32 origin][i32 low][i32 high][u32 1]
```

#### Kind `0x06` — Real

VHDL: `[u32 origin][u32 variant][f64 low][f64 high][u32 1]`  
Verilog: `[u32 origin][u32 0]`

#### Kind `0x07` — Alias (typedef)

```
[u32 origin][u32 target_index][u32 nranges]  <nranges × range triple>
```

Used to name Verilog structs and enums. The target is the unnamed base type entry. A parser should follow aliases before reading a type. An alias with `nranges > 0` carries bounds (e.g., `typedef logic [7:0] byte_t` gets one triple `(7, 0, -1)`); when the alias has no range, take bounds from the innermost alias that has any.

#### Kind `0x08` — Access (pointer type)

```
[u32 origin][u32 designated_type_index][u32 8][u32 48]
```

Variables of access types declare 48 bytes and have no record.

#### Kind `0x0C` — File type

```
[u32 origin][u32 element_type_index][u32 8][u32 40]
```

Variables of file types declare 0 bytes and have no record.

#### Kind `0x0D` — Physical (VHDL time/user physical)

```
[u32 origin][u32 n]  <n × (name NUL [u64 scale])>
```

`scale` gives each unit in the base unit (e.g., `TIME` uses picoseconds as the base).

#### Kind `0x10` — Array

```
[u32 origin][u16 layout][u16 0xA0][u32 element_type_index][u32 dims]
<dims × u32 index_type_index>
[u32 nranges]  <nranges × range triple>
<terminator: -99 or a small non-negative number under -debug all>
```

**Layout values:**
- `1` — VHDL array
- `2` — Verilog/SV unpacked array
- `3` — Verilog/SV packed array
- `6` — SystemVerilog packed union

An unconstrained type has one triple `(0, 0, -2)`. A constrained type has one triple per dimension of bounds. A constrained element adds its bounds after the type's own dimension triples.

#### Kind `0x11` — Record / struct

```
[u32 origin][u16 layout][u16 0x0B][u32 n]
<n × field: name NUL [u32 type_index][u32 nranges] <nranges × range triple>>
<terminator: -99>
```

Field `nranges > 0` only when the field is of a vector or record type that has an array field somewhere in it. The triples are the inner field's bounds in field declaration order (see §6.5).

#### Kinds `0x13–0x18` — Dynamic types (under `-debug all` only)

| Kind | Type |
|:---|:---|
| `0x13` | Dynamic array (`int d[]`) |
| `0x14` | Queue (`int q[$]`) |
| `0x15` | Associative array (`int a[key]`) |
| `0x17` | Class |
| `0x18` | String |

```
0x13: [u32 origin][u32 element_type_index][u32 number]
0x14: [u32 origin][u32 element_type_index][u32 number]
0x15: [u32 origin][u32 element_type_index][u32 number][u32 key_type_index]
0x17: [u32 origin][i32 parent_class_index (-1 if none)][u32 number][u32 n_fields]
      <n_fields × (name NUL [u32 type_index][u32 nranges] <triples> [u32 0])>
0x18: [u32 origin]
```

### 6.4 Range Triples

A range triple is `[i32 left][i32 right][i32 dir]` where `dir` is `1` for `to` and `-1` for `downto`. The terminator after the last triple in array and record entries is `0xFFFFFF9D` (`-99` as a signed 32-bit int).

### 6.5 Record Field Constraint Decoding

For VHDL records, a parser must combine the type-table field triples with the variable definition's range list:
- The variable definition's range list holds one triple per array dimension in field order, regardless of where the bounds are written in the source.
- An unconstrained field has `(0, 0, -2)` in the type table.
- Use the variable definition's ranges when available (they take precedence); fall back to the type table field triples only when the variable definition provides none.
- A `real` field contributes no triple to an outer field's list.

---

## 7. Netlist Hierarchy Section

**Magic:** `Xilinx ISim DBG 006\0` (20 bytes)

The section named by the `Xilinx DBG` directory entry. Despite being named "DBG" internally, this section contains the complete elaborated netlist hierarchy — scopes, scope definitions, variable definitions, and source locations — not just debug instrumentation.

### 7.0 Concepts: Scope Definitions, Scopes, Variable Definitions, and Variables

WDB uses four distinct concepts to represent the netlist hierarchy. They map to two pairs of standard concepts: **scope definition / scope instance** and **variable definition / variable instance**.

- **Scope Definition** (called `Unit` internally) — the module body, entity/architecture, or process body that a scope was elaborated from. Carries the list of variable definitions for that scope kind. One scope definition per scope, at the same index. No direct equivalent in VCD; safe to ignore if only signal names and values are needed.
- **Scope** (scope instance) — one node in the elaborated hierarchy tree (a module instance, entity instance, process, generate iteration, or the root). Equivalent to a `$scope` block in VCD. Referenced by array index; no numeric ID.
- **Variable Definition** (called `Decl` internally) — a signal, port, generic, constant, or variable as written in source. Belongs to a scope definition. Carries name, type, size, kind, and port mode, but no Signal ID or reference to value data. Referenced by array index; no numeric ID.
- **Variable** (variable record) — a variable definition elaborated in a specific scope. Carries a **Signal ID** (the WDB equivalent of a VCD identifier code), a scope index, a variable definition index, and a slice offset for ports bound to a slice of another signal. The Signal ID encodes the arena (`signal_id >> 11`) and the Local Signal ID within that arena (`signal_id & 0x7FF`).

#### The Full Chain

- **Scope** (hierarchy node)
  - has zero or more **child Scopes**
  - contains zero or more **Variables** (variable records — no children)
    - has a **variable definition index** → **Variable Definition** (name, type, size, kind)
    - has a **scope definition index** → **Scope Definition** (entity/architecture or module body)
    - has a **Signal ID** → **Arena + Local Signal ID** → **Value pages** (waveform data)
    - has a **slice offset** (non-zero only for ports bound to a slice of another signal)

Reading a signal's waveform end-to-end:
1. Find the scope by navigating the scope tree.
2. Find the variable record for that scope's variable (scope word 5 gives the first Variable ID; variables of a scope are contiguous).
3. Read the variable definition (via the variable's variable definition index) to get the signal's name, type, and size.
4. Use the Signal ID (from the variable record) to compute the arena index and Local Signal ID, then look up the value pages in the page directory.

### 7.1 Section Framing

Offsets are relative to the start of the netlist hierarchy section.

| Offset | Len | Type | Contents |
|----|----|:---|:---|
| `0` | 20 | `u8[20]` | Magic `Xilinx ISim DBG 006\0` |
| `20` | 4 | `uint32` | Timestamp (noise) |
| `24` | 4 | `int32` | Time unit exponent: power of 10 of the simulation precision in seconds (e.g., `-12` for picoseconds, `-9` for nanoseconds) |
| `28` | 72 | `uint32[18]` | Region offsets, relative to section start — see [§7.2 Regions](#72-regions) |
| `100` | 16 | `uint32[4]` | Number of scope, scope definition, variable, and variable definition records |
| `116` | 68 | `uint32[17]` | Header words — see [§7.4 Header Words](#74-header-words-17-words-at-offset-116) |
| `184` | varies | — | Regions |

All count fields are full 32-bit words. In large designs these can be substantial — a 70000-iteration for-generate produces 140004 scopes, 140004 scope definitions, 140000 variables, and 140000 variable definitions.

### 7.2 Regions

Region `i` runs from `offset[i]` to the next larger offset. Equal consecutive offsets mean an empty region. Region 2 is special: it is the end of the section proper; variable records begin there.

| offset | Region | Contents | Record size |
|---:|---:|:---|:---|
| `28` | 0 | Scope records — see [§7.5](#75-x -records-region-0) | 9 × `uint32` |
| `32` | 1 | Scope definition records — see [§7.6](#76-scope-definition-records-region-1) | 9 × `uint32` |
| `40` | 3 | Variable definition records — see [§7.7](#77-variable-definition-records-region-3) | 11 × `uint32`, padded to 8-byte boundary at end |
| `44` | 4 | Range records — see [§7.8](#78-range-records-region-4) | 6 × `uint32` |
| `48`–`60` | 5–8 | Empty | — |
| `+4` | 9 | Scope string table | NUL-terminated strings, padded to 8 bytes |
| `68` | 10 | Variable definition string table | NUL-terminated strings, padded to 8 bytes |
| `72` | 11 | File string table | NUL-terminated strings, padded to 8 bytes |
| `76` | 12 | Empty | — |
| `80` | 13 | File table — see [§7.9](#79-file-table-region-13) | 2 × `uint32` per file |
| `84` | 14 | Statement index | 2 × `uint32` per file |
| `88` | 15 | Statement lines | 1 × `uint32` per executable statement, padded to 8 bytes |
| `92` | 16 | Empty | — |
| `96` | 17 | Value class entries — see [§7.10](#710-value-class-entries-region-17) | 3 × `uint32` per distinct class, padded to 8 bytes |

### 7.3 Counts

| Offset | Len | Type | Count |
|:---|---:|:---|:---|
| `100` | 4 | `uint32` | Scope records. Always equals the scope definition count — there is one scope definition per scope. See [§7.5](#75-scope-records-region-0). |
| `104` | 4 | `uint32` | Scope definition records. Always equals the scope count. See [§7.6](#76-scope-definition-records-region-1). |
| `108` | 4 | `uint32` | Variable records. See [§8](#8-variable-records). |
| `112` | 4 | `uint32` | Variable definition records. See [§7.7](#77-variable-definition-records-region-3). |

### 7.4 Header Words (17 words at offset 116)

| Offset | Word | Contents |
|---:|---:|:---|
| `116` | 0 | Number of range records (region 4) |
| `120`–`132` | 1–4 | Counts of empty regions 5–8 (always 0) |
| `136` | 5 | Length of scope string table before padding |
| `140` | 6 | Length of variable definition string table before padding |
| `144` | 7 | Length of file string table before padding |
| `148` | 8 | Count of empty region 12 (always 0) |
| `152` | 9 | Number of files (entries in region 13) |
| `156` | 10 | Number of files again (entries in region 14) |
| `160` | 11 | Number of words in region 15 (`0` without `-debug line`) |
| `164` | 12 | Count of empty region 16 (always 0) |
| `168` | 13 | Number of value class entries (region 17) |
| `172` | 14 | Debug flags: byte0=1, byte1=`drivers`, byte2=`readers` |
| `176` | 15 | Debug flags: byte0=1, byte1=`line`/`subprogram`, byte2=`line`/`subprogram` |
| `180` | 16 | Constant `0x10000` |

Word `i` (0–13) counts region `i + 4`. Record regions are counted in records; string tables in bytes up to and including the last NUL; region 15 in words.

### 7.5 Scope Records (Region 0)

Each scope is one node of the elaborated hierarchy: the root, an entity instance, a generate iteration, or a process.

| Word | Contents |
|---:|:---|
| 0 | Name: offset into scope string table |
| 1 | Parent scope index (`-1` for root) |
| 2 | `0` |
| 3 | Number of child scopes |
| 4 | Index of first child scope (`-1` if none) |
| 5 | Variable ID of the first variable in this scope (`-1` if none) |
| 6 | File index of the scope's source (`0` for root) |
| 7 | Source line of the scope (`0` for root) |
| 8 | Scope definition index |

- The root scope is named `_top`.
- Children of one parent are contiguous, starting at the index in word 4.
- Variables of a scope are contiguous in the variable record list.
- The root may have multiple top-level children (multiple tops).
- A for-generate iteration scope uses an extended identifier: `\g(0)\` — backslashes are present in the string table.

### 7.6 Scope Definition Records (Region 1)

A scope definition is what a scope was elaborated from — one scope definition per scope in every case.

| Word | Contents |
|---:|:---|
| 0 | Entity/module name: offset into scope string table (`-1` if none) |
| 1 | Architecture name: same pool (`-1` if none) |
| 2 | Kind code (see below) |
| 3 | Number of variable definitions |
| 4 | `0` |
| 5 | File index of the architecture/body |
| 6 | Line of the architecture/body |
| 7 | File index of the entity/module (`0` for a process) |
| 8 | Line of the entity/module (`0` for a process) |

The variable definitions of a scope definition are the next `count` records after the previous scope definition's last record, in scope definition order.

**Scope definition kind codes:**

| Kind | Description |
|---:|:---|
| `0x00` | Verilog module |
| `0x01` | SV interface |
| `0x02` | SV modport |
| `0x03` | Verilog task |
| `0x04` | Verilog function |
| `0x05` | Named block (`begin : name`) |
| `0x07` | Verilog process (`initial`, `always`, etc.) |
| `0x08` | SV package |
| `0x09` | VHDL entity |
| `0x0A` | VHDL package |
| `0x0B` | VHDL `generate` or `block` |
| `0x0C` | VHDL `generate` or `block` (used consistently for for/if/case generate and block statements) |
| `0x0D` | VHDL process |
| `0x11` | VHDL function |
| `0x12` | VHDL procedure |
| `0x13` | Root |

Within a scope definition, signals come first in source order, followed by generics/constants/variables in source order.

**Multiple scope definitions for one entity:** If two instances have different generic values, they each get their own scope definition record (both named `child(sim)`). If they have the same generic values, they share one. This is a VHDL rule; Verilog modules always share one scope definition regardless of parameter differences.

### 7.7 Variable Definition Records (Region 3)

A variable definition is one signal, port, generic, constant, or variable before elaboration.

| Word | Contents |
|---:|:---|
| 0 | Name: offset into variable definition string table |
| 1 | Index into value class entry table (region 17) |
| 2 | File index |
| 3 | Source line |
| 4 | Value size: bytes for VHDL, bits for Verilog |
| 5 | Type index into the type table |
| 6 | Number of range records (region 4) |
| 7 | Index of first range record (`-1` if none) |
| 8 | Kind code (see below) |
| 9 | Port mode (see below) |
| 10 | Noise word for signals; `0` for variables |

**Variable definition kind codes:**

| Kind | Description |
|---:|:---|
| `0x0E` | VHDL signal (including ports) |
| `0x0F` | Variable declared in a process |
| `0x12` | Generic |
| `0x13` | Constant: architecture constant, loop index, or generate index |
| `0x14` | Subprogram parameter or variable (under `-debug subprogram`) |
| `0x15` | Signal parameter of a subprogram (under `-debug subprogram`) |
| `0x00` | Verilog variable: `reg`, `integer`, `real`, `time`, SV `logic`, `int`, struct, enum |
| `0x01` | Verilog `parameter` / `localparam` |
| `0x03` | Verilog net: `wire`, `uwire`, and every port |
| `0x04`–`0x0D` | Other Verilog net types: `wand`, `wor`, `tri`, `triand`, `trior`, `tri0`, `tri1`, (0x0B=unseen `trireg`), `supply0`, `supply1` |

**Port mode (word 9):**

| Value | Mode |
|---:|:---|
| `0` | `inout` |
| `1` | `in` |
| `2` | `out` |
| `3` | `buffer` |
| `4` | `linkage` |
| `5` | Not a port |

### 7.8 Range Records (Region 4)

A range record describes one dimension bound:

| Word | Contents |
|---:|:---|
| 0 | Left bound (low word of signed 64-bit pair) |
| 1 | Left bound (high word — sign extension) |
| 2 | Right bound (low word) |
| 3 | Right bound (high word) |
| 4 | Direction: `1` for `to`, `-1` for `downto` |
| 5 | Distance between bounds plus one (not the element count for null ranges) |

Actual element count = `max(0, (right - left + 1)` if `to`, `(left - right + 1)` if `downto`). Recompute from bounds and direction — the last word is unreliable for null ranges.

### 7.9 File Table (Region 13)

Two `uint32` per file:
1. Offset into file string table for the compiled path.
2. Offset into file string table for a local path, or `-1` if absent.

Files 0 and 1 are never referenced in the corpus. File 2 is the testbench. Subsequent files are library sources.

### 7.10 Value Class Entries (Region 17)

One 3-word entry per distinct Verilog value class among the variables in the file. The first word is the class code; the other two are always `0`. Entries appear in the order their class first appears in the variable list.

**Class codes (Verilog/SV):**

| Code | Description |
|---:|:---|
| `0` | Every VHDL variable; `real`/`realtime`; packed type with no initializer, or initializer running as an implicit process from a real/time literal; net; enum from a literal; unpacked struct/array; packed struct from `'{}`; real/realtime parameter |
| `1` | Packed type from a sized literal (`1'b0`, `8'h00`, etc.); fill literal (`'0`, `'1`, `'x`, `'z`); concatenation/replication/conditional/function call/comparison; expression with a sized operand; packed type parameter; untyped parameter from a sized literal/expression/string |
| `3` | Every `shortint`, `int`, `integer`, `longint`, `int unsigned`, `longint unsigned`; signed packed type from an unsized literal; untyped/`integer`/`int`/enum parameter; hidden variable of most casts; loop index; return variable of `function int` |
| `4` | Every `time`; unsigned packed type from an unsized literal or expression; time parameter |
| `6` | Packed type or untyped parameter from a string literal or string concatenation |

### 7.11 Signal Definition Records (Region 2 / Sub-section 2)

The netlist hierarchy section also embeds a signal definition table in the region between the scope/scope definition/variable definition regions and the variable records. These 56-byte records hold the `sig_id` and `sig_end` fields used during Verilog waveform decoding.

| Offset | Len | Field | Description |
|:---|---:|:---|:---|
| `0x00` | 8 | `sig_id` | Unique event-data identifier (equals the variable's primary Signal ID) |
| `0x08` | 8 | `sig_end` | `sig_id + data_size` |
| `0x10` | 4 | `scope_idx` | Scope table index |
| `0x14` | 4 | (unknown) | Usually `0` |
| `0x18` | 4 | (unknown) | Usually `0` |
| `0x1C` | 4 | (unknown) | Usually `0` |
| `0x20` | 8 | `name_idx` | Index into signal name string table |
| `0x28` | 8 | (flags) | Observed: `0x7FFE` variants |
| `0x30` | 8 | (padding) | `0` |

**`data_size`** = `sig_end - sig_id`. This equals the record `length` field in value pages (see §12.2). Consecutive `sig_id` values are spaced `0xB8` (184) bytes apart. The first signal's `sig_id` is typically `0x0708`.

Records are detected by the pattern: `sig_id > 0 && sig_end > sig_id && (sig_end - sig_id) ∈ {8, 16, 32} && sig_id < 0x100000`.

### 7.12 Signal Width/Type Table (Sub-section 3)

An array of 44-byte records, one per signal, located in the region immediately following the signal definition records.

| Offset | Len | Field | Description |
|:---|---:|:---|:---|
| `+0x00` | 8 | (padding) | 2 × `uint32`, always `0` |
| `+0x08` | 4 | marker | Always `2` — use to locate records |
| `+0x0C` | 4 | (unknown) | |
| `+0x10` | 4 | `bit_width` | Signal width in bits |
| `+0x14` | 4 | `type_ref` | Index into the type table |
| `+0x18` | 24 | (remaining) | Undocumented |

### 7.13 Verilog Scope Hierarchy Decoding (Verilog/SV only)

The scope records in region 0 give the elaborated hierarchy directly for VHDL designs. For Verilog/SV designs the scope table must be built from the module string table via a more involved process, because multiple instances of the same module share a single string and the binary scope definition records use `0xFFFFFFFF` as delimiters rather than a flat array.

#### 7.13.1 Module String Table Layout

Strings in the scope string table (region 9) fall into three categories:

1. **Module type definitions** — module names like `"mid"`, `"leaf"`. These are **skipped** when building the scope index table.
2. **Instance names** — `"u"`, `"v"`, `"u0"`, `"b0"`. These become scope entries.
3. **Process names** — match the pattern `{Prefix}{EID}_{N}` where `Prefix ∈ {Initial, Block, Always, NetRegassign, GenForLoop}` and `N` is a digit sequence. These become scope entries but typically have no signal children.

Strings are ordered:
- **Single-instance pattern**: alternating `(instance, TYPE, [processes...])`
- **Multi-instance pattern**: `(first_instance, TYPE, [processes...], instance₂, instance₃, ...)`

#### 7.13.2 Scope Index Mapping

Each signal's `scope_idx` field indexes into a **scope table** derived from the module string table. The scope table is built by iterating the strings and skipping entries that are module type definitions:

```
scope_to_str = [all non-type string indices in order]
scope_idx N  → str_idx scope_to_str[N] → string name
```

Example: `['_top', 'top', 'u', 'mid', 'Initial_1', 'v', 'leaf', ...]` with `mid` and `leaf` as types at indices 3 and 6:
```
scope_to_str = [0, 1, 2, 4, 5, 7, ...]
scope_idx 4  →  str_idx 5  →  "v"
```

#### 7.13.3 Two-Pass Type Identification

Types (module type strings) must be distinguished from instances before building the scope table. Use a two-pass algorithm:

**Pass 1 — region 0 binary scan:**

Region 0 uses `0xFFFFFFFF` (FF) as record delimiters. Scan **FF,FF records** (two consecutive `0xFFFFFFFF` words followed by data words):
- 7-word record where `w4 == w0` → `w2` is a **type definition** string index
- 7-word record where `w4` is a known type → `w2` is a **confirmed instance**
- 7-word record where `w4` is a non-type, non-self reference → both `w4` AND `w2` are added to **instance references** (handles multi-instance child patterns)
- Short records (3–4 words at the end of the region) are **ambiguous** — do not use for type classification

**Pass 2 — string table state machine** with three states:
- `ExpectInstance`: after a TYPE string → the next non-process string is an instance
- `ExpectTypeOrMulti`: after an instance → check whether the next string is a type (returning to `ExpectInstance`) or a confirmed multi-instance of the preceding type (entering `MultiInstance`)
- `MultiInstance`: all subsequent non-process strings are instances of the same type until a new type string appears

**End-of-walk reclassification**: If the walk ends in `ExpectTypeOrMulti` and all remaining non-type strings are processes, the last "first instance" is actually a type. This handles same-name-depth patterns.

#### 7.13.4 Region 0 — Scope Definition Records

Region 0 is a variable-length binary structure using `0xFFFFFFFF` as delimiters encoding module hierarchy, parent-child relationships, and type definitions.

**Single-FF records** (one `0xFFFFFFFF` then 7 data words):

| Word | Field | Description |
|:---|:---|:---|
| w0 | context | Parent scope string index |
| w1 | eid | Entity ID (unique per definition) |
| w2 | str_idx | Scope/instance string index |
| w3 | line | Source line number |
| w4 | parent | Parent/type string index |
| w5 | (zero) | Always `0` |
| w6 | ext_count | `0` = not extended; `>0` = inline child data follows |

**FF,FF records** (two `0xFFFFFFFF` words then 3–7 data words):

| Variant | Condition | Meaning |
|:---|:---|:---|
| Type definition | 7 words, `w4 == w0` | `w2` = module type string index |
| Instantiation | 7 words, `w4` ∈ type_set | `w2` = instance string index |
| Short record | 3–4 words | Context-dependent; may define a type or instance |

**Inline children** (appended after any single-FF record where `w6 > 0`):
- Scan for `(a, 0, c)` triples: sets scope `a` as a child of scope `c`
- A single FF within inline data separates sub-records (skip it; continue scanning)
- A double FF (FF,FF) terminates the inline data block

#### 7.13.5 Parent Relationship Resolution (Priority Order)

1. Binary header words 15–17 and 24–28 (explicit parent relationships in the header region)
2. Inline children from extended single-FF records (§7.12.4)
3. Type-nesting inference: each deeper `(instance, TYPE)` pair in the string table implies the instance is a child of the previous pair's instance
4. Default: unparented scopes → parent = scope 1 (top)

#### 7.13.6 Scope Table Expansion

The initial scope table has a 1:1 mapping from non-type string indices to scope entries. Two patterns require expansion:

**Same-name depth:** When the same instance name (e.g., `"u"`) is used at multiple hierarchy levels — each instantiating a different module type — the string table has only one entry for `"u"`, but signals reference **different** scope indices for each level.

```
mod_strs: ['_top', 'top', 'u', 'lv1', 'Initial_1', 'lv2', 'Initial_2', ...]
types: {0, 3, 5, ...}
```

Signal `scope_idx` values are `2, 4, 6, 8, ...` — even indices correspond to each depth level. The process-name scopes (at odd indices after type skipping) serve as **placeholders** that get renamed to the instance name (`"u"`) and re-parented to form a chain: `top → u → u → u → ...`

**Multi-instance parents:** When multiple instances of the same module type exist at the same level (e.g., `u0`, `u1` of type `mid`), child scopes must be **duplicated** for each parent instance. Signal `scope_idx` values beyond the initial table size reference these expanded entries.

```
mod_strs: ['_top', 'top', 'u0', 'mid', 'u1', 'Initial_1', 'v', 'leaf', ...]
scope_idx: 6 → v under u0
scope_idx: 7 → v under u1  (BEYOND initial table — expanded entry)
```

Expansion creates new scope entries named after the child instance (`"v"`) with parent set to the corresponding parent instance (`u0` or `u1`), assigning `scope_idx` values sequentially beyond the initial table for each parent's copy.

---

## 8. Variable Records

Immediately following the netlist hierarchy section (`offset[2]`), there are `n_variables` variable records of 56 bytes each (where `n_variables` is the third count word at section offset 100).

| Offset | Len | Contents |
|---:|---:|:---|
| `0` | 8 | Signal ID (primary) |
| `8` | 8 | Second Signal ID for signals (`0` for generics, constants, variables) |
| `16` | 4 | Scope index |
| `20` | 4 | Byte offset into value (for ports bound to slices); bits for Verilog. `0` otherwise |
| `24` | 4 | `0` |
| `28` | 4 | Storage class (see below) |
| `32` | 8 | Variable definition index |
| `40` | 4 | Position in Verilog port list (`0` for VHDL, non-ports, and later instances of the same scope definition) |
| `44` | 4 | Unwritten memory content — ignore |
| `48` | 8 | `0` |

**Storage class (word at offset 28):**

| Value | Description |
|---:|:---|
| `0` | Signal, net, or Verilog variable (ports included) |
| `1` | Port at a language boundary (VHDL↔Verilog) |
| `2` | Generic, constant, parameter, process variable, or loop index |
| `3` | Subprogram parameter or variable of scalar/access type |
| `4` | Subprogram parameter or variable of array/string/record type |
| `6` | Signal parameter of a subprogram |

**Signal ID structure:**
- `signal_id >> 11` → arena index
- `signal_id & 0x7FF` → Local Signal ID within the arena

The first Signal ID is always `0x768`. Subsequent Signal IDs are spaced by the value size (rounded up to 8 bytes) plus `0xE8` per signal with one driver, or `0xB8` for signals/ports with no driver. Non-signal variables (generics, constants, variables) get their Signal IDs after all signals.

**Port slice binding:** When a port is bound to a slice (word at offset 20 ≠ 0), the port's value is extracted from the signal's records: `signal_bytes[offset : offset + port_size]` for VHDL, or bits `offset` to `offset + width - 1` for Verilog.

---

## 9. Page Directory

Immediately after the `Xilinx DBG` directory entry. Arena record `i` is at `entry_offset + 48 + 0x4C0 * i`.

### 9.1 Arena Record (0x4C0 bytes)

| Offset | Len | Contents |
|:---|---:|:---|
| `0x000` | 8 | `uint64` file offset of a continuation record (`0` if none) |
| `0x008` | 800 | 100 × `uint64` page file offsets |
| `0x328` | 400 | 100 × `uint32` compressed page lengths |
| `0x4B8` | 8 | `uint64` number of pages listed in this record |

If an arena has more than 100 pages, the continuation record is written right after the 100th page, and the first record has `word 0 ≠ 0` with exactly 100 pages. The continuation record has the same layout.

---

## 10. Marker — Logged Ranges

At the file offset named in the trailer (`+0x38`), there is a list of `N` entries (where `N` is the trailer word at `+0x30`). Each entry is 16 bytes:

```
[uint64 first][uint64 last]
```

This is a **closed range** of Variable IDs (indices into the variable records). Every variable inside the range has at least one value change record; no variable outside has any.

**Notes:**
- A design that logs nothing has `N = 0` and the marker offset `= 0`.
- Multiple ranges appear when there are gaps (e.g., a package signal between two logged signals).
- The marker can appear after a page that was flushed mid-simulation; always follow the directory to find pages rather than scanning for the marker.

---

## 11. Value Pages

Each page is a zlib-compressed stream that inflates to exactly **10240 bytes** (`0x2800`).

### 11.1 Page Header (20 bytes)

| Offset | Len | Contents |
|:---|---:|:---|
| `0` | 8 | `uint64` `t0` — time of the first value change on this page (in the file's time unit) |
| `8` | 8 | `uint64` `t1` — `last_change_time - t0` |
| `16` | 4 | `uint32` `n` — number of value change records on this page |
| `20` | — | `n` value change records, then zero-padding to 10240 bytes |

### 11.2 Value Change Record Format

| Offset | Len | Contents |
|:---|---:|:---|
| `0` | 8 | `uint64` time of the change (in file's time unit) |
| `8` | 4 | `uint32` Local Signal ID (`signal_id & 0x7FF`) |
| `12` | 4 | `uint32` value length in bytes |
| `16` | `len` | Value bytes |

There is no alignment padding between value change records. A 1-byte value produces a 17-byte value change record.

**Value change semantics:**
- Value changes are in simulation order (not necessarily sorted by Local Signal ID within a time).
- Only **changes** get a value change record — writes of the already-held value produce no record.
- **Exception:** Resolved signals with multiple drivers record every transaction (changed or not). Nonblocking assignments in clocked `always` blocks record the first write and every later write that follows an event on an operand. Shared nets (multiple drivers+readers) record every driver evaluation.
- Delta cycles produce one value change record per delta cycle in which the value changes.
- A page flushed during simulation keeps only one value change per (Local Signal ID, time) pair — the last value at that time. The final page written at simulation close is not affected.

**Time unit:** The `int32` at netlist hierarchy section offset `+24` gives the base-10 exponent of the simulation precision in seconds. For example, `-12` means picoseconds. All times in value change records and page headers use this unit.

---

## 12. Value Encodings

### 12.1 VHDL Value Encodings

The `value_size` (variable definition word 4) is in bytes for VHDL.

| Type | Encoding |
|:---|:---|
| Enumeration (≤256 literals) | 1-byte literal index |
| Enumeration (≥257 literals) | 4-byte little-endian `uint32` literal index |
| `integer` and subtypes | `int32` (little-endian) |
| `real` | `float64` IEEE 754 (little-endian) |
| `time` and physical | `int64` counting the base unit (ps for `TIME`, base unit for user types) |
| Array | Elements back-to-back, left index first, row-major for multi-dimensional |
| Record | Fields in declaration order, with alignment; total rounded up to 8 bytes |

**`std_ulogic` literal indices:** `U=0 X=1 0=2 1=3 Z=4 W=5 L=6 H=7 -=8`

**Record field alignment:**
- `integer` fields: align to 4 bytes
- `real` fields: align to 8 bytes
- Record-typed fields: align to 8 bytes
- Record total size: rounded up to next multiple of 8

### 12.2 Verilog Value Encodings

The `value_size` (variable definition word 4) is in **bits** for Verilog. The on-disk value change record size is `8 × ceil(bits / 32)` bytes.

The on-disk size of a Verilog value change record (the `length` field) is determined by the declared bit width:

| Declared width | Record `length` | Storage layout |
|:---|---:|:---|
| 1–32 bits | 8 | `val[31:0]`  `xz[31:0]` |
| 33–64 bits | 16 | `val[31:0]`  `xz[31:0]`  `val[63:32]`  `xz[63:32]` |
| 65–128 bits | 32 | `val[31:0]`  `xz[31:0]`  … `val[127:96]`  `xz[127:96]` |
| 129+ bits | 64+ | Same pattern, one `(val, xz)` pair per 32 bits; unconfirmed above 128 bits |

Values are stored as **interleaved `(val, xz)` uint32 word pairs** — the `val` word followed by the `xz` word for each 32-bit slice, LSW first:

```
length=8:   [val 31:0][xz 31:0]
length=16:  [val 31:0][xz 31:0][val 63:32][xz 63:32]
length=32:  [val 31:0][xz 31:0][val 63:32][xz 63:32][val 95:64][xz 95:64][val 127:96][xz 127:96]
```

Per-bit logic state encoding within each pair (`val_bit`, `xz_bit`):

| `val` bit | `xz` bit | Logic state |
|:----------|:---------|:------------|
| `0`       | `0`      | `0`         |
| `1`       | `0`      | `1`         |
| `0`       | `1`      | `Z`         |
| `1`       | `1`      | `X`         |

Bits above the declared width are `0` in both words. Note: for `length=8` (≤32 bits) the layout is equivalent to a simple value/mask split (`val` in the low 4 bytes, `xz` in the high 4 bytes). The interleaved distinction matters only for wider values.

**Local Signal ID for a partial Verilog value change record:** `signal_id + (8 × first_pair_index)`. A partial write covers only the pairs it touches. The reader overlays each value change record on the accumulated value.

### 12.3 Special Verilog Types

- **`real` / `shortreal`**: One pair holding a `float64` (regardless of declared bit width).
- **`integer`, `int`, `byte`, `longint`, `time`**: Stored as a vector of their width (32, 32, 8, 64, 64 bits).
- **String parameter (untyped, `parameter P = "hello"`)**: Stored as a vector — 8 bits per character, first character at the most-significant bits.
- **Untyped time parameter (`parameter T = 10ns`)**: Stores a `float64` of the value in the time unit. The value change record is 16 bytes; only the first 8 bytes hold the value. Treat specially (see §11.2 "Value change semantics").

---

## 13. Verilog / SystemVerilog Specifics

### 13.1 Process Scope Naming

Verilog processes create implicit scopes named after their kind, line, and a counter:

| Process kind | Name pattern |
|:---|:---|
| `initial` at line N | `InitialN_counter` |
| `always` / `always_ff` / `always_comb` / `always_latch` / `final` at line N | `AlwaysN_counter` |
| `assign` at line N | `NetRegassignN_counter` |
| `fork` branch at line N | `ForkedN_counter` |
| Gate primitive at line N | `ForkedN_counter` |

The counter is per-design, incrementing across all modules in post-order (children before parents). Named blocks create their own scope (`begin : blk`). A `for` loop index declared in the loop creates a `BlockN_counter` scope.

### 13.2 Signal ID Order

Nets get Signal IDs before variables. In a hierarchy, nets come first in pre-order. An `output` port shares the Signal ID of the net it drives in the parent. An `input` port connected to a `wire` shares the wire's Signal ID. An `input` port connected to a `reg` gets its own Signal ID.

The stride between Verilog variables is `0xB8 + record_size`. (Compare with VHDL's `0xE8 + rounded_size` which includes a 0x30 driver slot.)

### 13.3 Value Changes at Time 0

- `.v` file variables with initializers (`reg s = 1'b0`): value changes `X` then the initial value at time 0 (runs as an implicit process).
- `.sv` file variables with initializers (`logic s = 1'b0`): records only the initial value change (taken at declaration).
- Nets with no driver: initial value change is `Z` (not `X`).
- Nets with a driver: initial value change is `X` per variable on the Signal ID, plus one extra when the net has two or more drivers+readers together.

### 13.4 Struct Encoding

- **Packed struct**: contiguous bits, first field at the most significant bits.
- **Unpacked struct**: each field gets its own word-pair slots, last field at the lowest pair. A partial write to one field updates only the pairs that field occupies.

### 13.5 Memory / Unpacked Array

Memory elements are contiguous bits with `m[0]` (first element in ascending index order) at the most significant bits. A partial write to element `m[i]` updates only the pair(s) that element occupies.

---

## 14. Wide Values and Chunking

Values of 275 bytes or more are split into multiple value change records (chunks). Each chunk is a separate value change record with a Local Signal ID equal to `signal_id + byte_offset_of_chunk`. The same chunking applies to both VHDL (byte-based) and Verilog (pair-based, same constants).

### 14.1 Chunk Count Formula

```
size < 275 bytes  →  1 chunk  (whole value in one value change record)
size ≥ 275 bytes  →  n = 2 × ceil((size + 24) / 299) chunks
                      chunk_size = floor(size / n)
                      last chunk takes the remainder
```

The count steps up by 2 for every additional 299 bytes past 275. Equal-sized chunks; the last chunk takes the remaining bytes.

### 14.2 Recursive Chunking of the Remainder

The last chunk may itself be longer than 275 bytes when `n` is large. In that case, apply the same chunking formula recursively to the remainder, treating it as a sub-value starting at `signal_id + n × chunk_size`. A remainder of exactly 275 bytes stays as one chunk (threshold for recursion is > 275, not ≥ 275).

### 14.3 Arena Boundary Splits

A chunk that crosses an arena boundary (`0x800`-byte boundary in the Signal ID space) is split at the boundary. The pieces of the chunk appear as separate value change records in their respective arenas. A parser joins the pieces by address before decoding the value.

### 14.4 Reassembly Algorithm

1. For a variable spanning multiple chunks, collect all value change records at a given timestamp whose Local Signal IDs fall in `[signal_id, signal_id + size)`, from all arenas, in file order within each time.
2. A value change record at the variable's first chunk address with the first-chunk length starts a whole write; subsequent value change records for that time at the predicted chunk addresses complete it.
3. Any other value change record starts a partial write.
4. Join arena-split pieces by address before decoding pairs (Verilog) or bytes (VHDL).

---

## 15. VHDL Partial Writes

An assignment to a field, slice, or element of a VHDL signal writes a value change record shorter than the full value. The value change record's Local Signal ID is `signal_id + byte_offset` of the first byte of the modified portion. The parser overlays the value change record on the accumulated value in file order.

**Offset rules:**
- Record field: its aligned byte offset within the record layout.
- Vector slice `v(hi downto lo)`: byte offset = `(bit_width - 1 - hi)` bytes from the start of the value (i.e., the leftmost written byte).
- Array element `a(i)`: `(i - left_bound) × element_size` bytes.

**Write merging within one delta cycle:**
- Two adjacent modified regions in one delta cycle from one driver → one value change record spanning both.
- Two non-adjacent regions in one delta cycle from one driver → two value change records at one time.
- One whole assignment plus a partial in the same delta → one whole value change record holding the result.
- Value change records from different drivers at one time are separate.
- Value change records are in simulator execution order (not source order).

---

## 16. Variable Value Change Summary

| Variable kind | Value changes written |
|:---|:---|
| Signal | One at time 0 (initial value), then one per change |
| Port connected to signal | One at time 0 on the signal's Signal ID; no further value changes of its own |
| Port left open | Same as a signal, on its own Signal ID |
| Generic / constant | One at time 0 |
| Architecture constant | One at time 0 |
| Process `for` loop index | One at time 0 holding `0` |
| `for generate` loop index | One at time 0 holding the iteration's value |
| Port bound to a literal | Same as a signal, on its own Signal ID |
| Port bound to a slice | One at time 0 on the signal's Signal ID at the slice offset |
| Signal inside a `block` | Same as a signal |
| Variable in a process | None |
| Package constant | None (unless package is explicitly logged) |
| Package signal | None under default `log_wave -recursive *` |
| SV package parameter | None under default script |
| Null range signal | None (marked not logged) |
| `std_logic` with ≥2 drivers | One at time 0, then one per transaction (changed or not) |
| Signal read via external name | Every change written twice |
| Verilog `reg` / variable | `X` at time 0, then one per change |
| Verilog `logic` | Initial value at time 0 (no `X`), then one per change |
| Verilog net (wire) with driver | `X` at time 0 per object on Signal ID; extra `X` when net has ≥2 drivers+readers |
| Verilog net without driver | `Z` at time 0 |
| Clocked nonblocking assignment | First write + every write following an event on any operand |
| Shared net (≥2 drivers+readers) | Every driver evaluation |

---

## Appendix A: Timescale Encoding

The `int32` at netlist hierarchy section offset `+24` encodes the precision as a power of 10 of seconds:

| int32 | Precision |
|------:|:----------|
|   -15 | 1 fs      |
|   -12 | 1 ps      |
|    -9 | 1 ns      |
|    -6 | 1 µs      |
|    -3 | 1 ms      |
|     0 | 1 s       |

Conversion to a VCD `$timescale` string: `exponent % 3` gives the multiplier (`0`→`1`, `-1`/`+2`→`100`, `-2`/`+1`→`10`), and `floor(exponent / 3)` gives the unit (`-5`→`fs`, `-4`→`ps`, `-3`→`ns`, `-2`→`µs`, `-1`→`ms`, `0`→`s`).

---

## Appendix B: Debug Level Flags

Header words 14 and 15 (byte-level flags):

| Word | Byte 0 | Byte 1 | Byte 2 |
|:---|:---|:---|:---|
| 14 | Always `1` | `drivers` mode | `readers` mode |
| 15 | Always `1` | `line`/`subprogram` scope | `line`/`subprogram` variable definitions |

`-debug typical` sets both word 14 byte 1 and word 15 byte 1. `-debug all` sets all bytes. A scope appears for a subprogram when word 15 byte 1 is set; its variable definitions appear when word 15 byte 2 is set. `xlibs` sets no byte.

---

## Appendix C: Parser Implementation Notes

1. **Start from the header pointer at `0x48`** to locate the trailer, then compute the arena table length as `(pointer - 0xC8) / 8`.
2. **Read the trailer** to get: slot count (`+0x0C`), Signal ID space (`+0x18`), time unit (`DBG section +24`), end time (`+0x00`), marker offset/count (`+0x38`/`+0x30`), and page size (`+0x40`).
3. **Follow the directory entry pointers** to locate the type table and netlist hierarchy section. Validate each by checking the magic string.
4. **Parse the netlist hierarchy section** to build the scope tree, scope definition list, variable definition list, and file table. Read the variable records (which start at `offset[2]`) to map variable definition→Signal ID.
5. **Use the arena table** to locate page directory records. For each non-zero slot, read the arena record at the indicated offset to get page locations.
6. **Decompress pages** (zlib), parse value change records, and dispatch by Local Signal ID → variable Signal ID → variable definition.
7. **Handle wide values** by collecting all chunk value change records for a Local Signal ID range before decoding.
8. **Overlay partial writes** (VHDL) or partial pair writes (Verilog) on the accumulated value buffer.
9. **Consult the marker** (logged ranges) to know which variables have any value change records.
10. **Value class entries** in region 17 are Verilog-only metadata; VHDL files have a single class-0 entry.
11. **Ignore the word at variable record offset `+44`** — it is uninitialized memory.
12. **For type-table parsing**: entries form a flat list in dependency order (outer types before inner). Follow alias chains before reading a type. The offset list at the end of the type table is a cross-reference — validate that it names every entry.
