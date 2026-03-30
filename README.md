# mfa — mac file analyzer

A fast, accurate command-line tool for analysing disk usage on macOS.

## Features

- **Three walkers** with increasing performance: single-threaded baseline → rayon parallel → `getattrlist` bulk syscall parallel
- **Accurate sizes**: tracks actual disk blocks (`st_blocks × 512`) by default; `--apparent` for logical byte counts
- **Correct hardlink handling**: every `(dev, ino)` pair is counted exactly once
- **Sparse file awareness**: disk usage reflects real storage, not logical file length
- **Resource fork support** (`--include-rsrc`): adds macOS resource fork bytes via `ATTR_FILE_RSRCLENGTH`
- **Symlink safety**: skipped by default; `--follow-symlinks` follows with cycle detection
- **Five output formats**: tree, flat list, by-extension breakdown, JSON, CSV
- **Filtering**: by extension (`-t`), minimum size (`--min-size`), depth (`-d`), and top-N (`-n`)

## Requirements

| Requirement | Minimum version |
|---|---|
| macOS | 10.12 Sierra (Intel) · 11.0 Big Sur (Apple Silicon) |
| Rust toolchain | 1.70 |

## Installation

### From source

```
git clone https://github.com/TimYagan/mac-file-analyzer
cd mac-file-analyzer
cargo install --path .
```

The binary is installed as `mfa` in `~/.cargo/bin/`.

### Development build

```
cargo build
./target/debug/mfa [OPTIONS] [PATH]
```

### Release build (recommended for performance)

```
cargo build --release
./target/release/mfa [OPTIONS] [PATH]
```

## Usage

```
mfa [OPTIONS] [PATH]
```

`PATH` defaults to the current directory if omitted.

### Options

| Flag | Short | Default | Description |
|---|---|---|---|
| `--depth <N>` | `-d` | unlimited | Maximum directory depth to descend |
| `--top <N>` | `-n` | `20` | Show top N largest entries (flat/JSON/CSV modes) |
| `--format <FORMAT>` | `-f` | `tree` | Output format: `tree`, `flat`, `json`, `csv` |
| `--sort <ORDER>` | `-s` | `size` | Sort order: `size` (largest first) or `name` (A–Z) |
| `--type <EXT>` | `-t` | — | Only count files with this extension (e.g. `mp4`) |
| `--min-size <SIZE>` | — | — | Only show entries at or above this size |
| `--apparent` | — | off | Use logical file sizes (`st_size`) instead of disk blocks |
| `--follow-symlinks` | — | off | Follow symlinks (cycle-safe) |
| `--include-rsrc` | — | off | Add macOS resource fork bytes to each file's size |
| `--by-ext` | — | off | Aggregate by extension instead of by path |
| `--dirs-only` | — | off | Show directories only, each ranked by their total content size. Lists every subdirectory (not the scan root itself) with its rolled-up size. Compatible with `--min-size`, `--top N`, `--sort`, and the `flat`, `json`, and `csv` output formats. Has no effect with `--format tree` (a note is printed to stderr). Cannot be combined with `--by-ext`. |
| `--no-progress` | — | off | Suppress the progress spinner |
| `--stats` | — | off | Print total size and elapsed time after output |
| `--quiet` | — | off | Suppress non-critical warnings to stderr |

### `--min-size` format

Accepts an integer or decimal number with an optional unit suffix.
A space between the number and unit is allowed.

| Input | Interpreted as |
|---|---|
| `500` | 500 bytes |
| `10KB` or `10KiB` | 10 × 1 024 B = 10 240 B |
| `100MB` or `100MiB` | 100 × 1 048 576 B |
| `1.5GiB` | 1 610 612 736 B |
| `2TB` | 2 × 1 099 511 627 776 B |

## Output modes

### Tree (default)

Hierarchical view sorted largest-first. Analogous to `ncdu`.

```
$ mfa ~/Downloads
  42.3 GiB  /Users/you/Downloads
  38.1 GiB    Videos
  38.1 GiB      lecture-archive.mp4
   3.8 GiB    ISOs
   3.8 GiB      ubuntu-24.04.iso
 421.0 MiB    archives
 ...
```

Control depth with `-d`:

```
$ mfa -d 2 ~/Projects
```

Sort alphabetically instead of by size:

```
$ mfa -s name ~/Projects
```

### Flat list

Individual files ranked by size, largest first — no directory rollup entries. Use `-n` to limit results.
Each line shows the human-readable size followed by the full path.

```
$ mfa -f flat -n 5 ~/Downloads
    38.1 GiB	/Users/you/Downloads/Videos/lecture-archive.mp4
     3.8 GiB	/Users/you/Downloads/ISOs/ubuntu-24.04.iso
   421.0 MiB	/Users/you/Downloads/archives/backup.tar.gz
   210.3 MiB	/Users/you/Downloads/Installers/XcodeCommandLineTools.dmg
   105.7 MiB	/Users/you/Downloads/fonts/all-google-fonts.zip
```

Find every file over 1 GiB anywhere on the Mac (requires sudo for full access):

```
$ sudo mfa -f flat --apparent --min-size 1GiB --no-progress /
```

Filter to files over 200 MB and sort alphabetically:

```
$ mfa -f flat --min-size 200MB -s name ~/
```

### By-extension breakdown

```
$ mfa --by-ext ~/Projects
    1.2 GiB  .mp4     (42 files)
  312.3 MiB  .gz      (8 files)
  205.1 MiB  .rs      (1843 files)
  (no ext)            (12 files)   ← files without an extension
```

Combine with `-s name` to list extensions alphabetically:

```
$ mfa --by-ext -s name /
```

### JSON

Flat list as a JSON array. Each element has `path`, `bytes`, and `human` fields.

```
$ mfa -f json -n 5 ~/Downloads | jq '.[].path'
```

By-extension as JSON:

```
$ mfa --by-ext -f json ~/Downloads
[
  { "extension": "mp4", "bytes": 40937100288, "human": "38.1 GiB", "file_count": 1 },
  ...
]
```

### CSV

Flat list as CSV with header `path,bytes,human_size`:

```
$ mfa -f csv -n 50 ~/Downloads > report.csv
```

By-extension as CSV with header `extension,bytes,human_size,file_count`:

```
$ mfa --by-ext -f csv ~/Downloads > by_ext.csv
```

## Common recipes

### Reclaim disk space — find large files

```bash
# Find all files over 200 MB across the entire Mac (needs sudo for full access)
sudo mfa -f flat --apparent --min-size 200MB --no-progress --quiet /

# Same, but limited to your home directory (no sudo needed)
mfa -f flat --apparent --min-size 200MB --no-progress --quiet ~/

# Top 50 largest files anywhere, no size filter
sudo mfa -f flat -n 50 --no-progress --quiet /

# Find large log files
mfa -f flat -t log --min-size 50MB --quiet /

# Find large video files in Downloads
mfa -f flat -t mp4 --min-size 100MB --quiet ~/Downloads

# Find large ZIP / TAR archives
mfa -f flat -t zip --min-size 200MB --quiet ~/
mfa -f flat -t gz  --min-size 200MB ~/ --quiet
```

### Developer artifact cleanup

```bash
# See what's taking space in your projects folder (3 levels deep)
mfa -d 3 ~/projects

# Find large Rust/C static libraries (.a files) in all project targets
mfa -f flat -t a --min-size 100MB ~/projects

# Inspect Xcode DerivedData usage
mfa ~/Library/Developer/Xcode/DerivedData

# Find huge compile databases (.db files)
mfa -f flat -t db --min-size 500MB ~/Library

# Android SDK / NDK toolchain sizes
mfa -d 3 ~/Library/Android/sdk
```

### macOS Library & cache analysis

```bash
# Survey the biggest items in Library caches
mfa -d 2 ~/Library/Caches

# Check Application Support
mfa -d 2 ~/Library/Application\ Support

# Find large items across the whole Library folder
mfa -f flat --min-size 500MB ~/Library
```

### Overview & reporting

```bash
# Top 20 largest files/dirs in the current directory (default)
mfa

# Home folder usage, 3 levels deep
mfa -d 3 ~

# Extension breakdown to see what file types dominate
mfa --by-ext ~/Downloads

# Extension breakdown sorted alphabetically
mfa --by-ext -s name ~/Media

# Export top 200 files to CSV for a spreadsheet
mfa -f csv -n 200 ~ > ~/disk-report.csv

# Pipe JSON to jq — extract paths of files over 1 GiB
mfa -f json --min-size 1GiB ~ | jq '.[].path'

# Print total size and elapsed time
mfa --stats --no-progress ~/Projects
```

### Find large directories

```bash
# Top 20 largest subdirectories in the current directory
mfa --dirs-only

# Find subdirectories over 1 GiB
mfa --dirs-only --min-size 1GiB

# Top 10 largest directories in Downloads, sorted alphabetically
mfa --dirs-only -n 10 -s name ~/Downloads

# Export directory sizes to CSV
mfa --dirs-only -f csv ~/Projects > dir-sizes.csv

# JSON output for directories over 500 MB
mfa --dirs-only -f json --min-size 500MB ~/Library
```

### Accuracy & special cases

```bash
# Logical (apparent) size instead of actual disk blocks
mfa --apparent ~/Downloads

# Include macOS resource forks (relevant for Classic Mac data)
mfa --include-rsrc ~/Library

# Follow symlinks (cycle-safe)
mfa --follow-symlinks /usr/local

# Sort alphabetically instead of by size
mfa -s name ~/Projects
```

## Accuracy notes

| Scenario | Behaviour |
|---|---|
| Hardlinks | Each `(device, inode)` pair is counted exactly once across all three walkers |
| Sparse files | `--apparent` reports logical size; default reports real disk blocks (`st_blocks × 512`) |
| APFS clones | Block-based count is honest — shared extents between clones are only counted once per inode |
| Symlinks | Skipped by default; `--follow-symlinks` counts the target inode (cycle-safe) |
| Resource forks | Excluded by default; `--include-rsrc` adds the fork length via `getattrlist(ATTR_FILE_RSRCLENGTH)` |
| Special files | Devices, pipes, and sockets are always skipped |
| Permission errors | Reported to stderr as warnings; the rest of the tree continues |

## Running tests

```bash
# All unit tests (aggregator, formatter, walker, stat, output)
cargo test --lib

# Integration tests (accuracy fixtures: hardlinks, symlinks, sparse files, …)
cargo test --test accuracy_tests

# Integration tests (flags, output formats, edge cases)
cargo test --test cli_tests

# All tests
cargo test
```

Expected: **67 library unit tests + 12 main tests + 41 accuracy tests + 40 CLI tests = 160 tests total**, all passing.

## Running benchmarks

Benchmarks use [Criterion](https://bheisler.github.io/criterion.rs/book/) and build synthetic
directory trees during setup so wall-clock measurements are clean.

Benchmark infrastructure is included via Criterion, but representative public benchmark results are not yet documented because results vary significantly by hardware, filesystem, and macOS version.

```bash
# Run all benchmarks and print results to stdout
cargo bench

# Run only the Phase 3 getattrlist benchmarks
cargo bench -- walk_5k_files_phase3

# Run only the output-rendering benchmarks
cargo bench -- render_

# Generate an HTML report (opens automatically in browser)
cargo bench
open target/criterion/report/index.html
```

## Project structure

```
mac-file-analyzer/
├── .cargo/
│   └── config.toml          # macOS deployment target configuration
├── src/
│   ├── lib.rs               # Public module declarations
│   ├── main.rs              # CLI entrypoint (clap)
│   ├── walker.rs            # Three walkers: sequential, parallel, getattrlist
│   ├── stat.rs              # macOS getattrlist wrapper, inode dedup
│   ├── aggregator.rs        # Size rollup, tree building, sort/filter
│   ├── formatter.rs         # Human-readable output: tree, flat, by-ext
│   └── output/
│       ├── mod.rs           # Output module declarations
│       ├── json.rs          # JSON serialization
│       └── csv.rs           # CSV serialization
├── tests/
│   ├── accuracy_tests.rs    # Integration tests (hardlinks, symlinks, sparse, ...)
│   └── cli_tests.rs         # Binary integration tests (CLI flags, output formats)
├── benches/
│   └── walk_bench.rs        # Criterion benchmarks (walkers + renderers)
├── docs/
│   └── plan.md              # Original design plan
├── .gitignore               # Git ignore patterns
├── CODE_OF_CONDUCT.md       # Contributor Covenant code of conduct
├── CONTRIBUTING.md          # Contribution guidelines
├── LICENSE                  # MIT license
├── Cargo.lock               # Dependency lock file
├── Cargo.toml               # Package metadata and dependencies
└── README.md                # This file
```

## Build profiles

The `[profile.release]` section in `Cargo.toml` enables:

- `opt-level = 3` — full optimisation
- `lto = "thin"` — link-time optimisation for cross-crate inlining
- `codegen-units = 1` — single codegen unit for maximum optimisation
- `strip = true` — strip debug symbols from the binary

## Contributing

Bug reports and feature requests are welcome via
[GitHub Issues](https://github.com/TimYagan/mac-file-analyzer/issues).

For code contributions, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Code of Conduct

This project has adopted the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). For more information, see the [Code of Conduct](CODE_OF_CONDUCT.md) or open an issue in this repository with any questions or concerns.

## License

MIT — see [LICENSE](LICENSE).
