# mfa Architectural Decisions

This document records the major design decisions behind `mfa` (mac file analyzer).
It is intended for contributors and technical readers who want to understand not just
what was built, but why the structure looks the way it does.

Where a decision is an inference from project structure and behavior rather than
directly documented intent, it is noted as a "likely rationale" or "inferred motivation".

---

## 1. Purpose and Scope

`mfa` is a command-line disk usage analyzer for macOS. It reports how much disk
space is consumed by files and directories, with correct accounting for hardlinks,
sparse files, and (optionally) resource forks. It supports multiple output formats
to fit into shell pipelines and reporting workflows.

### Why macOS-only

macOS presents specific challenges that generic cross-platform tools handle poorly
or not at all: resource forks, the `getattrlist` bulk attribute syscall, APFS
clone semantics, and deployment target constraints for arm64 vs x86_64. Targeting
macOS exclusively means the implementation can use native APIs without portability
abstraction layers, and accuracy guarantees can be stated precisely.

`du(1)` is not designed around the same accuracy and scripting goals. It does not expose resource fork sizes cleanly, and it does not provide structured output.

---

## 2. Core Architectural Principles

**Correctness before convenience.** The default behavior -- block-based sizes,
hardlink deduplication, and skipped symlinks, reflects what is actually on disk, not
what might be convenient to display.

**Actual disk usage as the default metric.** `st_blocks * 512` is the default size
metric. Logical byte count (`st_size`) is available as `--apparent` but is not
the default because it overstates disk consumption for sparse files and does not
reflect the actual cost of holding data on an APFS volume.

**macOS-native where it is justified.** The `getattrlist` path exists because it can retrieve multiple file attributes in a single syscall, including size, inode, device, and resource fork length.

**Scriptable output, not interactive UI.** All output modes (tree, flat, by-ext,
JSON, CSV) are designed for stdout. The tool does not require a terminal, does not
use cursor control, and produces clean output that can be piped to `jq`, imported
to a spreadsheet, or passed to other tools.

**Separation of concerns across pipeline stages.** Traversal, stat collection,
aggregation, formatting, and serialization are in separate modules. Each module has a clearly bounded responsibility. This makes it straightforward to test individual
stages and to extend or replace them independently.

---

## 3. Key Decisions and Rationale

### 3.1 Language: Rust

**Decision:** Implement in Rust.

**Rationale:** The problem requires direct use of macOS syscalls (via `libc`),
precise control over memory allocation in the hot path, and high-throughput
parallel iteration. Rust provides these without a garbage collector, with a
powerful type system that catches correctness errors at compile time.

**Tradeoffs:** Higher barrier to contribution compared to Python or Go. Build
times are longer. These are acceptable given the performance and correctness goals.

---

### 3.2 Three-Walker Strategy

**Decision:** Implement three distinct walkers (`sequential`, `rayon parallel`,
`getattrlist parallel`) rather than a single traversal path.

**Rationale:** Likely motivation is to allow benchmarking and comparison across
strategies, and to have a fallback when `getattrlist` returns incomplete attribute
sets (for example, on non-APFS volumes or mounted network filesystems). The
sequential walker serves as the correctness baseline. The parallel walker uses
`rayon` work-stealing for CPU-bound workloads. The `getattrlist` walker issues bulk
syscalls to reduce kernel round-trips.

**On `fts_open`/`fts_read`:** The original design plan listed `fts_open`/`fts_read`
as a candidate traversal strategy (POSIX file-tree walk with kernel-assisted ordering).
This was evaluated but not adopted. The `getattrlist` parallel walker achieves the
same kernel-efficiency goal by batching attribute retrieval into a single syscall,
which is a more direct macOS-native optimization than `fts_open` provides.
`fts_open` was therefore not superseded by an equivalent approach -- `getattrlist`
surpasses it for the macOS use case and was chosen instead.

**On the aggregation model:** The original plan described a channel-based aggregation
topology: worker threads push file records into a channel, a single dedicated thread
consumes the channel and builds the aggregated tree. The implementation uses rayon's
work-stealing parallel iterator instead. In the rayon model there is no explicit
channel; parallel traversal and local stat collection are orchestrated by rayon's
scheduler, and results are collected into a shared data structure protected by the
borrow checker. This was a deliberate architectural shift: rayon's work-stealing is
simpler to reason about, eliminates the channel backpressure problem, and integrates
naturally with Rust's ownership model.

**Tradeoffs:** More code to maintain. Three walkers must produce identical results
for the same input, which the accuracy test suite verifies. The benefit is that
the performance-optimized path can be validated against the baseline, and
degradation on unusual filesystems does not break the tool.

---

### 3.3 Block-Based Disk Usage as Default

**Decision:** Report `st_blocks * 512` by default. Expose `st_size` as `--apparent`.

**Rationale:** Sparse files can have a logical size of, say, 1 GiB but consume
only a few kilobytes of actual blocks. Reporting `st_size` for sparse files would
be misleading to anyone trying to understand actual storage consumption. APFS
similarly distinguishes between logical and physical usage. The default should
answer "how much space would I reclaim" rather than "how large are these files
in theory."

**Tradeoffs:** Users familiar with `ls -l` or `du -sh` may be surprised by
differences when resource forks or sparse files are involved. The `--apparent`
flag and the accuracy notes in the README address this explicitly.

---

### 3.4 Inode Deduplication via (dev, ino)

**Decision:** Track every seen `(device, inode)` pair in a `HashSet` and count
each unique inode exactly once.

**Rationale:** Hardlinks share an inode. Counting size per directory entry would
count the same data multiple times for any filesystem layout that uses hardlinks --
including macOS system directories that use hardlinks heavily for Time Machine
and system integrity purposes.

**Tradeoffs:** The `HashSet` grows with the number of unique inodes seen. For
very large scans (millions of files), memory usage increases proportionally.
This is an accepted cost for correct output.

---

### 3.5 Symlinks Skipped by Default

**Decision:** Skip symlink targets by default. Follow with `--follow-symlinks`,
which includes cycle detection using the same `(dev, ino)` seen-set.

**Rationale:** Following symlinks by default risks double-counting directory
trees or producing infinite loops on circular symlinks. Skipping is the safe
default. Users who explicitly need symlink targets traversed can opt in.

**Tradeoffs:** A directory containing mostly symlinks to large files will appear
nearly empty by default. This is technically correct but may be unexpected.
The accuracy notes in the README document this.

---

### 3.6 Resource Forks Excluded by Default

**Decision:** Resource forks (macOS-specific secondary file streams) are not
included in file sizes unless `--include-rsrc` is passed.

**Rationale:** For nearly all modern files on a modern macOS system, resource
forks are empty or negligibly small. Including them by default would complicate
output for the common case. For users with Classic Mac data, media metadata, or
extended-attribute-heavy files, the option is available.

**Implementation note:** When enabled, `getattrlist` is called with
`ATTR_FILE_RSRCLENGTH` to retrieve the fork size in the same syscall that
retrieves other attributes, at negligible additional cost.

**Tradeoffs:** The default may underreport for a small class of files. The
option exists and is documented.

---

### 3.7 Output Modeled as Tree / Flat / By-Extension / JSON / CSV

**Decision:** Five output modes covering human-readable hierarchical display,
ranked flat list, extension aggregation, and two machine-readable formats.

**Rationale:** Different use cases need different output shapes. Tree view fits
interactive exploration. Flat list ranked by size fits "find the biggest files"
workflows. By-extension fits "what kinds of files are consuming the most space"
analysis. JSON and CSV fit downstream processing with `jq`, spreadsheets, and
custom scripts.

**Tradeoffs:** More formatter and serializer code to maintain. Each mode must
handle all combinations of filtering flags.

---

### 3.8 `--format` Abstraction Instead of Per-Format Flags

**Decision:** Use a single `--format <tree|flat|json|csv>` flag rather than
separate `--json` and `--csv` flags.

**Rationale (noted as a plan evolution):** The original plan specified `--json`
and `--csv` as independent flags. The implemented CLI consolidates format
selection into `--format`. This avoids ambiguity when both `--json` and `--csv`
might theoretically be passed simultaneously, and it models format selection
as a single enum choice rather than as a boolean combination.

**Tradeoffs:** Users familiar with tools that use `--json` as a flag need to
adjust. The `--by-ext` flag remains separate because it changes the aggregation
model, not just the serialization format.

---

### 3.9 Separate Output Serializers Under `src/output/`

**Decision:** JSON and CSV serialization live in `src/output/json.rs` and
`src/output/csv.rs`, behind a module boundary.

**Rationale (inferred):** Separating format-specific serialization from the
aggregation and formatting logic means that `aggregator.rs` and `formatter.rs`
do not need to know about serde or CSV encoding. The output module takes
already-computed data structures and renders them. This makes the serializers
easy to test and replace.

**Tradeoffs:** An additional module boundary to navigate. Minor overhead in
clarity for small codebases; this pays off as the number of supported formats
grows.

---

### 3.10 Traversal Continues on Permission Errors

**Decision:** When a directory or file cannot be stat'd or opened due to a
permission error, log a warning to stderr and continue traversal.

**Rationale:** Users running without root will regularly encounter restricted
system directories. Halting the entire scan on the first permission error would
make the tool useless for home-directory scans that touch even a single
restricted subtree. The useful behavior is to report what is visible and note
what was skipped.

**Tradeoffs:** Output is incomplete in the presence of permission errors.
The stderr warnings document this. `--quiet` suppresses the warnings when
the caller knows this is expected.

---

### 3.11 CLI-First, No GUI or TUI

**Decision:** The tool is a Unix filter: reads from the filesystem, writes
structured text to stdout.

**Rationale:** A TUI such as `ncdu` is optimized for interactive exploration rather than for use in scripts or CI pipelines. A GUI is out of scope for a Rust CLI project of
this nature. Scriptability and composability are higher-value properties for the
target user.

**Non-goals:** See Section 6.

---

### 3.12 Benchmark-Driven Optimization Path

**Decision:** Criterion benchmarks targeting each walker strategy and each
output renderer are included in `benches/walk_bench.rs`.

**Rationale:** The three-walker strategy only makes sense if the performance
difference between walkers can be measured and communicated. Benchmarks also
make regressions detectable. Criterion builds synthetic directory trees during
bench setup so that wall-clock results are stable and portable.

**Tradeoffs:** Benchmark maintenance burden. Setup fixtures must reflect
realistic file distributions to be useful.

---

### 3.13 Test Coverage for Correctness Edge Cases

**Decision:** A dedicated `tests/accuracy_tests.rs` suite creates controlled
fixtures with known hardlinks, symlinks, and sparse files, then verifies that
reported sizes match expected values.

**Rationale:** The accuracy guarantees (hardlink dedup, sparse file accounting,
symlink skip) are not obviously correct from reading the code. They must be
verified against known inputs. Unit tests alone cannot cover filesystem-level
behavior; integration tests with real filesystem fixtures are necessary.

**Tradeoffs:** Test fixtures require filesystem operations and temporary
directories. `tempfile` is used to isolate test state.

---

## 4. Module Structure Rationale

**`walker.rs`** -- Contains all three traversal strategies. Walkers produce a
stream of file-level records (path, size, inode, device). They do not aggregate
or format; they only collect. Keeping all walker variants in one file makes it
easy to compare implementations and ensures they share the same output type
contract.

**`stat.rs`** -- Wraps macOS syscalls (`lstat`, `getattrlist`) and owns the
inode deduplication logic. Isolating this here means the rest of the codebase
does not call libc directly, and the `getattrlist` attribute validation (checking
the returned bitmask) is in one place.

**`aggregator.rs`** -- Builds the `DirNode` tree from walker output, rolls up
sizes, applies filters, and produces the sorted views consumed by formatters.
This is the most complex module. Separating aggregation from traversal allows
the same aggregated data structure to be fed into multiple output renderers.

**`formatter.rs`** -- Renders the aggregated tree into human-readable text
(tree view, flat list, by-extension summary). Does not handle serialization
to JSON or CSV; those are in `src/output/`.

**`src/output/json.rs` and `src/output/csv.rs`** -- Thin serialization layers
over the aggregated data. They depend on `serde` and `csv` crates respectively
and are isolated from the rest of the pipeline.

**`tests/accuracy_tests.rs`** -- Filesystem-level correctness tests. These are
integration tests that construct real temporary directory trees and verify sizes.

**`tests/cli_tests.rs`** -- Black-box tests that invoke the compiled `mfa`
binary and check stdout/stderr behavior for each flag combination. These catch
CLI surface regressions that unit tests cannot.

**`benches/walk_bench.rs`** -- Criterion benchmarks for walker throughput and
renderer performance. Synthetic trees are built during bench setup.

---

## 5. Notable Changes from the Original Plan

| Area | Original plan | Final implementation |
|---|---|---|
| JSON/CSV flags | `--json`, `--csv` as separate booleans | Unified into `--format <json\|csv\|tree\|flat>` |
| `--dirs-only` | Not in original plan | Added; shows directory rollup totals without file entries |
| `--stats` and `--quiet` | Not in original plan | Added for operational convenience |
| `--include-rsrc` | Implicit in getattrlist discussion | Explicit opt-in flag |
| `fts_open` traversal | Listed as a candidate strategy | Evaluated and superseded; `getattrlist` parallel walker achieves kernel-efficiency more directly for macOS and was chosen instead |
| Aggregation model | Channel-based: workers push, one thread aggregates | Replaced by rayon work-stealing; no explicit channel; results collected via rayon's parallel iterator |
| `lib.rs` | Not in original structure | Added to expose modules as a library for integration testing |
| `output/mod.rs` | Not in original structure | Added as the module entry point for the output submodule |
| `perf_bench.rs` | Named this in original plan | Renamed to `walk_bench.rs` in final project |

---

## 6. Non-Goals

- **Not a general-purpose cross-platform disk analyzer.** The `getattrlist` path, resource fork
  support, and macOS deployment targets are intentionally macOS-specific. Porting
  to Linux or Windows is not a design goal.

- **Not a GUI or TUI application.** There is no interactive display. `ncdu` serves
  that use case.

- **Not a background indexing service.** `mfa` does not run as a daemon, does not
  cache results between invocations, and does not watch for filesystem changes.

- **Not a forensic filesystem tool.** It does not analyze deleted files, examine
  raw disk structures, or provide inode-level provenance beyond what `stat` exposes.

- **Not a clone-aware physical accounting oracle.** APFS deduplication and shared
  extents between clones are accounted for at the inode/block level as reported by
  the OS. `mfa` does not inspect the APFS B-tree or track extent sharing across
  inodes; it relies on the kernel's `st_blocks` value, which is honest per inode
  but does not detect shared extents between two distinct inodes that are APFS
  clones of each other.

---

## 7. Future Evolution

These are plausible next steps, not commitments.

- **Distribution improvements.** A Homebrew formula or a pre-built binary
  release would lower the installation barrier for non-Rust users.

- **Benchmark result documentation.** Publishing representative benchmark numbers
  (walkers at N files, renderers at M nodes) in the docs would let contributors
  assess the impact of changes before merging.

- **Contributor architecture notes.** An expanded version of this document
  covering the aggregator data structures (the `DirNode` tree and flat sort
  internals) would help new contributors understand the core data model.

- **Optional comparison output.** A flag to compare two scan results for before-and-after disk cleanup workflows has been discussed as a future direction but is not part of the current design.
