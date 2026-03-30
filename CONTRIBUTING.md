# Contributing to mfa

Thanks for your interest in improving mfa!

## Reporting bugs

Open a [GitHub issue](https://github.com/TimYagan/mac-file-analyzer/issues/new?template=bug_report.md).

Please include:
- macOS version and architecture (Intel or Apple Silicon)
- `mfa --version` output
- the exact command you ran
- what you expected to happen
- what actually happened
- a minimal reproduction, if possible

## Requesting features

Open a [GitHub issue](https://github.com/TimYagan/mac-file-analyzer/issues/new?template=feature_request.md) describing the use case and why the existing flags or output modes do not cover it.

## Development setup

```bash
cargo build
cargo test
```

## Submitting a pull request

1. Search [existing issues](https://github.com/TimYagan/mac-file-analyzer/issues) first — there may
   already be a related discussion or open PR.
2. For non-trivial changes, open an issue to discuss the approach before writing code.
3. Fork the repo, create a focused branch, and make your changes.
4. Run the full test suite before pushing:
   ```bash
   cargo fmt --check
   cargo test
   cargo clippy -- -D warnings
   ```
5. Open a pull request against `main`. Link the related issue in the PR description.

## Code style

- Follow standard `rustfmt` formatting (`cargo fmt`).
- Keep new `unsafe` blocks minimal and document their safety invariants.
- All public functions and significant internal logic should be covered by tests.

## License

By contributing, you agree that your changes will be released under the
[MIT License](LICENSE).

## Versioning and releases

`mfa` uses [Semantic Versioning](https://semver.org/) (`MAJOR.MINOR.PATCH`).

- **PATCH** — bug fixes, doc updates, no API or flag changes
- **MINOR** — new flags or output modes, backward-compatible
- **MAJOR** — breaking changes to CLI flags, output format, or required macOS version

### Release checklist (maintainers)

1. Update `version` in `Cargo.toml`.
2. Run `cargo build` so `Cargo.lock` is updated, then commit both.
3. Add a `## [X.Y.Z] — YYYY-MM-DD` entry to `CHANGELOG.md`.
4. Open and merge a release PR.
5. After merge, tag the commit: `git tag v0.1.0 && git push origin v0.1.0`.
6. Create a GitHub release from the tag; paste the CHANGELOG entry as the release body.
7. Attach a pre-built `mfa` binary built with `cargo build --release` on both Intel and Apple Silicon.

