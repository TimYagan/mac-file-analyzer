# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [0.1.3] - 2026-04-01

### Fixed
- Bump version to 0.1.3
- Fix a minor typo in the README

[0.1.3]: https://github.com/TimYagan/mac-file-analyzer/releases/tag/v0.1.3 


## [0.1.2] - 2026-04-01

### Fixed
- Bump version to 0.1.2

[0.1.2]: https://github.com/TimYagan/mac-file-analyzer/releases/tag/v0.1.2 

## [0.1.1] - 2026-03-31

### Fixed
- Reject non-finite values such as `NaN` and `Infinity` in `--min-size`
- Prevent CSV formula injection by prefixing dangerous paths with a tab in CSV output
- Improve error messages for invalid `--min-size` values

### Added
- Add `run-tests.sh` for convenient local test execution
- Complete functional validation of CLI feature behavior

[0.1.1]: https://github.com/TimYagan/mac-file-analyzer/releases/tag/v0.1.1 

## [0.1.0] — 2026-03-31

### Added
- Three-walker strategy: sequential baseline, rayon parallel, `getattrlist` bulk-syscall parallel
- Accurate disk usage via `st_blocks × 512` by default; `--apparent` for logical sizes
- Hardlink deduplication via `(dev, ino)` seen-set
- Sparse file awareness
- Resource fork support via `--include-rsrc` (`ATTR_FILE_RSRCLENGTH`)
- Symlink skipping by default; `--follow-symlinks` with cycle detection
- Five output formats: `tree`, `flat`, `json`, `csv`, by-extension (`--by-ext`)
- Filtering flags: `--type`, `--min-size`, `--depth`, `--top`
- Display flags: `--dirs-only`, `--stats`, `--quiet`, `--no-progress`
- `getattrlist` attribute validation with `lstat` fallback for non-APFS volumes
- macOS deployment targets: 10.12 Sierra (x86_64), 11.0 Big Sur (aarch64)
- MIT license
- 160-test suite: unit, integration (accuracy fixtures), and CLI black-box tests
- Criterion benchmarks in `benches/walk_bench.rs`

[0.1.0]: https://github.com/TimYagan/mac-file-analyzer/releases/tag/v0.1.0
