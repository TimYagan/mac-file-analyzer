# mfa Implementation Plan and Evolution

This document began as the implementation plan for `mfa` (mac file analyzer) and
now serves a dual purpose: it records the original design intent and documents
how the project evolved from that plan into the shipped implementation. It is
intended to be useful to future contributors who want to understand the project's
history and design direction.

---

## Original Goals

The project was conceived as a fast, accurate disk usage analyzer for macOS,
built in Rust. The headline goals were:

- Parallel directory traversal using work-stealing (`rayon`)
- Accurate size reporting: real disk blocks by default, logical size as opt-in
- Correct hardlink handling by deduplicating on `(dev, ino)` pairs
- macOS-native optimization via `getattrlist` bulk attribute syscall
- Resource fork support via `ATTR_FILE_RSRCLENGTH`
- Multiple output modes: tree, flat list, by-extension breakdown, JSON, CSV
- A complete CLI with depth, top-N, sort, extension filter, and size filter controls
- An accuracy test suite covering hardlinks, sparse files, and symlinks
- Criterion benchmarks to measure and compare walker strategies

---

## Architecture Evolution

The original plan described a single-phase traversal with a pluggable stat layer
and a straightforward formatter. What shipped differs in several meaningful ways:

**Three walkers instead of one.** The plan listed `rayon` parallel traversal and
`getattrlist` as strategies. The implementation separates these into three
distinct, runnable walkers: a single-threaded baseline, a `rayon` parallel walker,
and a `getattrlist` parallel walker. This makes each strategy independently
benchmarkable and provides a correctness baseline for the optimized paths.

**`lib.rs` added.** The original structure had no `lib.rs`. A library crate entry
point was added so that integration tests can reference internal types and
functions directly rather than only through the binary.

**Output module formalized.** The original plan listed `json.rs` and `csv.rs`
under `src/output/` but did not include a `mod.rs`. The final structure includes
`src/output/mod.rs` as a proper Rust submodule entry point.

**`--json` / `--csv` consolidated into `--format`.** The original CLI specified
`--json` and `--csv` as separate boolean flags. The implemented CLI uses
`--format <tree|flat|json|csv>` as a single enum, eliminating ambiguity when
selecting output format.

**New flags not in the original plan.** `--dirs-only`, `--include-rsrc`,
`--stats`, and `--quiet` all appear in the shipped CLI but were not in the
original plan. These reflect real operational needs discovered during implementation.

**Benchmark naming change.** The original plan named the benchmark file
`perf_bench.rs`. The final file is `benches/walk_bench.rs`.

---

## What Shipped

### Traversal Strategies

Three walkers in `src/walker.rs`:

| Walker | Mechanism | Use |
|---|---|---|
| Sequential | Single-threaded `walkdir`-style traversal | Correctness baseline |
| Parallel | `rayon` work-stealing across all CPU cores | General-purpose fast path |
| getattrlist parallel | Bulk `getattrlist` syscall with `rayon` | macOS-optimized fast path |

All three walkers produce the same output contract and are verified against
each other in the accuracy test suite.

### Accuracy Model

| Problem | Solution | Status |
|---|---|---|
| Hardlinks counted multiple times | `(dev, ino)` `HashSet` dedup across all walkers | Shipped |
| Apparent size vs disk usage | Default: `st_blocks * 512`; opt-in `--apparent` for `st_size` | Shipped |
| Sparse files | Block-based default reflects real storage | Shipped |
| APFS clones | Block-based per-inode count is honest | Shipped (with noted limitation) |
| Symlinks | Skipped by default; `--follow-symlinks` with cycle detection | Shipped |
| Resource forks | Excluded by default; `--include-rsrc` adds `ATTR_FILE_RSRCLENGTH` | Shipped |
| Special files | Devices, pipes, sockets skipped | Shipped |
| Permission errors | Warning to stderr; traversal continues | Shipped |

### CLI Surface

```
mfa [OPTIONS] [PATH]

Core options:
  -d, --depth <N>          Max traversal depth (default: unlimited)
  -n, --top <N>            Top N results in flat/json/csv modes (default: 20)
  -f, --format <FORMAT>    Output format: tree, flat, json, csv (default: tree)
  -s, --sort <ORDER>       Sort by size (default) or name
  -t, --type <EXT>         Filter by file extension
      --min-size <SIZE>    Minimum size threshold (supports KB, MB, GiB, etc.)
      --apparent           Logical file sizes instead of disk blocks
      --follow-symlinks    Follow symlinks with cycle detection
      --include-rsrc       Add resource fork bytes via getattrlist
      --by-ext             Aggregate by extension instead of by path
      --dirs-only          Show rolled-up directory totals only
      --no-progress        Suppress progress spinner
      --stats              Print total size and elapsed time after output
      --quiet              Suppress non-critical stderr warnings
```

Format and aggregation combinations:

- `--format tree` produces a hierarchical display (default)
- `--format flat` produces a file-ranked list; no directory rollup entries
- `--format json` / `--format csv` produce machine-readable flat or by-ext output
- `--by-ext` switches aggregation to extension totals; compatible with json/csv
- `--dirs-only` shows directory rollups; incompatible with `--by-ext`

### Output Modes

Five output modes covering interactive exploration and pipeline use:

1. **Tree**: hierarchical, sorted largest-first, human-readable sizes
2. **Flat list**: files ranked by size, largest first, with full paths
3. **By-extension**: aggregated totals per extension with file counts
4. **JSON**: flat or by-ext data as a JSON array for `jq` and downstream tools
5. **CSV**: flat or by-ext data with a header row for spreadsheet import
### Testing and Benchmarks

- 67 library unit tests (aggregator, formatter, walker, stat, output modules)
- 12 main module tests
- 41 accuracy integration tests (hardlinks, symlinks, sparse files via `tempfile` fixtures)
- 40 CLI integration tests (binary invocation, flag combinations, output format validation)
- Total: 160 tests, all passing

Criterion benchmarks in `benches/walk_bench.rs`:
- Walker throughput benchmarks for each of the three strategies
- Output renderer benchmarks for tree and flat rendering paths
- Synthetic directory trees built during bench setup for stable wall-clock results

### Project Layout

```
mac-file-analyzer/
├── .cargo/
│   └── config.toml          # macOS deployment targets (10.12 x86_64 / 11.0 aarch64)
├── src/
│   ├── lib.rs               # Public module declarations
│   ├── main.rs              # CLI entrypoint (clap)
│   ├── walker.rs            # Three walkers: sequential, parallel, getattrlist
│   ├── stat.rs              # macOS getattrlist wrapper and inode dedup
│   ├── aggregator.rs        # Size rollup, DirNode tree, sort and filter
│   ├── formatter.rs         # Human-readable output: tree, flat, by-ext
│   └── output/
│       ├── mod.rs           # Output submodule declarations
│       ├── json.rs          # JSON serialization
│       └── csv.rs           # CSV serialization
├── tests/
│   ├── accuracy_tests.rs    # Filesystem-level correctness tests
│   └── cli_tests.rs         # Binary integration tests
├── benches/
│   └── walk_bench.rs        # Criterion benchmarks
├── docs/
│   ├── plan.md              # This file
│   └── architectural-decisions.md  # Design decisions and rationale
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── CONTRIBUTING.md
├── CODE_OF_CONDUCT.md
└── README.md
```

---

## Original Plan vs Final Implementation

| Area | Original plan | Final implementation | Notes |
|---|---|---|---|
| Walker count | Implied single + getattrlist | Three distinct walkers | Sequential added as correctness baseline |
| `--json` / `--csv` flags | Separate boolean flags | `--format <json\|csv\|tree\|flat>` | Consolidated to eliminate ambiguity |
| `--dirs-only` | Not planned | Shipped | Operational use case |
| `--stats` / `--quiet` | Not planned | Shipped | Operational convenience |
| `--include-rsrc` | Implicit in getattrlist section | Explicit opt-in flag | Excluded by default |
| `lib.rs` | Not in original structure | Added | Required for integration test access |
| `output/mod.rs` | Not listed | Added | Standard Rust submodule structure |
| Benchmark filename | `perf_bench.rs` | `benches/walk_bench.rs` | Renamed and moved to `benches/` |
| Deployment targets | Not specified | `.cargo/config.toml` sets 10.12/11.0 | Added for distribution compatibility |
| `fts_open` traversal | Listed as a design option | Not foregrounded in final implementation | getattrlist path covers the same optimization goal |

---

## Lessons from Implementation

**Correctness rules needed to be first-class.** The accuracy test suite was
conceived as a final step in the original plan but its scope expanded because
hardlink dedup and sparse file accounting have subtle failure modes. In practice, correctness testing became a first-class implementation concern rather than a final validation step.

**Output shape benefited from consolidation.** The original `--json` / `--csv`
flag design would have complicated flag parsing and produced ambiguous states.
Consolidating into `--format` improved the CLI contract.

**macOS-specific optimizations were worth isolating.** Keeping `getattrlist`
entirely inside `stat.rs` meant that when the validation logic (checking the
returned attribute bitmask) needed to be added for non-APFS volumes, it was
a contained change.

**Operational flags add real value.** `--dirs-only`, `--stats`, and `--quiet`
were not in the original plan but are among the most practical flags for common
workflows. Design documentation benefits from leaving room for these additions
rather than treating the CLI surface as frozen at plan time.

---

## Remaining Opportunities

- **Homebrew formula or binary release.** Currently requires Rust toolchain to
  install. A formula or a GitHub Releases binary would lower the barrier for
  non-Rust users.

- **Published benchmark numbers.** The benchmark infrastructure exists but
  representative results (walker throughput at various file counts, renderer
  latency) are not yet documented. This would help contributors evaluate changes.

- **Contributor notes on the aggregator data model.** The `DirNode` tree and
  the flat sort internals in `aggregator.rs` are the most complex part of the
  codebase. Inline documentation or an expanded section in
  `docs/architectural-decisions.md` would help new contributors.

- **Scan result comparison.** A before/after comparison workflow (e.g., diff
  two JSON outputs) is a plausible future addition but is not part of the
  current design and would require careful output schema design.

---

## Conclusion

The project shipped substantially as planned. The core accuracy model, the three
walker strategies, and the five output modes all reflect the original intent.
The main evolution was in the CLI surface (flag consolidation, new operational
flags) and in the project structure (library crate, output submodule, deployment
target configuration). The accuracy test suite proved to be the most important
investment: it provides confidence that the correctness guarantees hold across
walker implementations and across edge cases that are hard to reason about
statically.

