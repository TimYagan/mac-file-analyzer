//! End-to-end binary integration tests.
//!
//! Each test spawns the compiled `mfa` binary against a controlled filesystem
//! fixture and validates exit code, stdout, and stderr.  All fixtures are
//! created in temporary directories that are cleaned up automatically on drop.
//!
//! Tests that require manipulating file permissions check whether the process
//! is running as root (where chmod 000 has no effect) and skip gracefully.

use std::fs;
use std::os::unix::fs as unix_fs;
use std::process::Command;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Path to the compiled binary, injected by Cargo at build time.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mfa")
}

/// Build a deterministic four-file fixture.
///
/// Layout (apparent sizes):
/// ```text
///   alpha.txt     1 024 B
///   beta.rs       2 048 B
///   gamma.rs      4 096 B
///   sub/delta.txt   512 B
/// ```
fn make_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("alpha.txt"), vec![b'a'; 1024]).unwrap();
    fs::write(tmp.path().join("beta.rs"),   vec![b'b'; 2048]).unwrap();
    fs::write(tmp.path().join("gamma.rs"),  vec![b'c'; 4096]).unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("delta.txt"), vec![b'd'; 512]).unwrap();
    tmp
}

/// Returns true when the current process UID is 0 (root).
///
/// Tests that chmod a directory to 000 must skip when running as root, because
/// root ignores DAC permission bits and will read the directory anyway,
/// preventing the expected permission-denied warning from firing.
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| o.stdout == b"0\n")
        .unwrap_or(false)
}

/// RAII guard that restores directory permissions on drop.
///
/// Used in permission-related tests to ensure the temp directory can be
/// cleaned up even if a test assertion panics mid-way.
struct RestoreMode {
    path: std::path::PathBuf,
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(
            &self.path,
            fs::Permissions::from_mode(0o755),
        );
    }
}

// ── exit-code tests ───────────────────────────────────────────────────────────

#[test]
fn help_exits_zero() {
    let status = Command::new(bin()).arg("--help").status().unwrap();
    assert!(status.success(), "--help must exit 0");
}

#[test]
fn version_exits_zero() {
    let status = Command::new(bin()).arg("--version").status().unwrap();
    assert!(status.success(), "--version must exit 0");
}

#[test]
fn invalid_path_exits_nonzero() {
    let status = Command::new(bin())
        .arg("/this/path/absolutely/does/not/exist/4e8f2a")
        .status()
        .unwrap();
    assert!(!status.success(), "nonexistent path must exit non-zero");
}

// ── output format tests ───────────────────────────────────────────────────────

#[test]
fn default_format_is_tree_with_box_drawing_chars() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("├──") || stdout.contains("└──"),
        "default tree format must contain box-drawing characters, got:\n{}",
        stdout
    );
}

#[test]
fn flat_format_lines_are_tab_separated() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.is_empty(), "flat output must not be empty");
    for line in stdout.lines() {
        assert!(
            line.contains('\t'),
            "each flat line must contain a tab separator, got: {:?}",
            line
        );
    }
}

#[test]
fn json_format_is_valid_json_array() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("-f json must produce valid JSON");
    assert!(parsed.is_array(), "JSON output must be a top-level array");
    let arr = parsed.as_array().unwrap();
    assert!(!arr.is_empty(), "JSON array must not be empty");
    // Validate required keys on each entry.
    for item in arr {
        assert!(item.get("path").is_some(),  "each JSON entry must have 'path'");
        assert!(item.get("bytes").is_some(), "each JSON entry must have 'bytes'");
        assert!(item.get("human").is_some(), "each JSON entry must have 'human'");
    }
}

#[test]
fn csv_format_has_correct_header() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "csv", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first_line = stdout.lines().next().expect("CSV must have at least a header line");
    assert_eq!(first_line, "path,bytes,human_size", "CSV header must match expected columns");
    // Must also have at least one data row.
    assert!(
        stdout.lines().count() > 1,
        "CSV must have at least one data row beyond the header"
    );
}

// ── --top N tests ─────────────────────────────────────────────────────────────

#[test]
fn top_n_limits_flat_output_to_n_lines() {
    let tmp = make_fixture(); // 4 files total
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "-n", "2", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 2,
        "--top 2 must produce at most 2 lines, got {}",
        line_count
    );
}

#[test]
fn top_n_limits_json_array_length() {
    let tmp = make_fixture(); // 4 files total
    let out = Command::new(bin())
        .args(["-f", "json", "--apparent", "-n", "2", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    assert!(
        arr.len() <= 2,
        "--top 2 must produce at most 2 JSON entries, got {}",
        arr.len()
    );
}

#[test]
fn top_n_limits_csv_data_rows() {
    let tmp = make_fixture(); // 4 files total
    let out = Command::new(bin())
        .args(["-f", "csv", "--apparent", "-n", "2", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Lines = 1 header + at most N data rows.
    let data_rows = stdout.lines().count().saturating_sub(1);
    assert!(
        data_rows <= 2,
        "--top 2 must produce at most 2 CSV data rows, got {}",
        data_rows
    );
}

// ── --stats test ──────────────────────────────────────────────────────────────

#[test]
fn stats_flag_writes_summary_to_stderr() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--stats", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unique files"),
        "--stats must print 'unique files' on stderr, got: {:?}",
        stderr
    );
    assert!(
        stderr.contains("total"),
        "--stats must print 'total' on stderr, got: {:?}",
        stderr
    );
}

// ── --sort tests ──────────────────────────────────────────────────────────────

#[test]
fn sort_name_produces_alphabetical_flat_output() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--sort", "name", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let paths: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .collect();
    assert!(!paths.is_empty(), "--sort name output must not be empty");
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "--sort name flat output must be in alphabetical order");
}

#[test]
fn sort_name_json_output_is_alphabetical() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--apparent", "--sort", "name", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    let paths: Vec<&str> = arr.iter()
        .map(|v| v["path"].as_str().unwrap_or(""))
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "-f json --sort name must produce alphabetically ordered paths");
}

// ── --min-size tests ──────────────────────────────────────────────────────────

#[test]
fn min_size_flat_excludes_small_files() {
    let tmp = make_fixture();
    // gamma.rs = 4096 B = 4 KiB. Only it should survive --min-size 3KB (3072 B).
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--min-size", "3KB", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one file (gamma.rs) must survive --min-size 3KB");
    let path = lines[0].split('\t').nth(1).unwrap_or("");
    assert!(
        path.contains("gamma.rs"),
        "the surviving entry must be gamma.rs, got: {:?}",
        path
    );
}

#[test]
fn min_size_json_all_entries_above_threshold() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--apparent", "--min-size", "2KB", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    for item in arr {
        let bytes = item["bytes"].as_u64().expect("bytes must be a u64");
        assert!(
            bytes >= 2 * 1024,
            "--min-size 2KB: all JSON entries must be >= 2048 B, got {}",
            bytes
        );
    }
}

// ── --type filter tests ───────────────────────────────────────────────────────

#[test]
fn type_filter_includes_only_matching_extension() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--type", "rs", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.is_empty(), "--type rs must produce output for a dir with .rs files");
    for line in stdout.lines() {
        let path = line.split('\t').nth(1).unwrap_or("");
        assert!(
            path.ends_with(".rs"),
            "--type rs must only include .rs files, found: {:?}",
            path
        );
    }
}

#[test]
fn type_filter_excludes_all_when_no_match() {
    let tmp = make_fixture();
    // The fixture has no .xyz files — output should be empty.
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--type", "xyz", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "--type xyz must produce no output when no matching files exist"
    );
}

// ── --by-ext tests ────────────────────────────────────────────────────────────

#[test]
fn by_ext_default_output_contains_extension_labels() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--by-ext", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(".rs"),  "--by-ext output must show .rs extension");
    assert!(stdout.contains(".txt"), "--by-ext output must show .txt extension");
}

#[test]
fn by_ext_json_has_required_keys() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--by-ext", "-f", "json", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--by-ext -f json must produce valid JSON");
    let arr = parsed.as_array().unwrap();
    assert!(!arr.is_empty(), "--by-ext json must have at least one entry");
    for item in arr {
        assert!(item.get("extension").is_some(), "by-ext JSON entry must have 'extension'");
        assert!(item.get("bytes").is_some(),      "by-ext JSON entry must have 'bytes'");
        assert!(item.get("file_count").is_some(), "by-ext JSON entry must have 'file_count'");
        assert!(item.get("human").is_some(),      "by-ext JSON entry must have 'human'");
    }
}

#[test]
fn by_ext_csv_has_correct_header() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--by-ext", "-f", "csv", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let header = stdout.lines().next().expect("--by-ext -f csv must have a header line");
    assert_eq!(header, "extension,bytes,human_size,file_count");
}

#[test]
fn by_ext_with_sort_name_is_alphabetical() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--by-ext", "--sort", "name", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Each line's extension label is the last token; rows must be alphabetical.
    let exts: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .collect();
    let mut sorted = exts.clone();
    sorted.sort();
    assert_eq!(exts, sorted, "--by-ext --sort name must be alphabetical by extension");
}

// ── --depth tests ─────────────────────────────────────────────────────────────

#[test]
fn depth_zero_excludes_subdirectory_files() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "-d", "0", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("delta.txt"),
        "--depth 0 must not include files from subdirectories"
    );
    // Root-level files must still appear.
    assert!(
        stdout.contains("gamma.rs"),
        "--depth 0 must still include root-level files"
    );
}

#[test]
fn depth_one_includes_first_level_subdir_files() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "-d", "1", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("delta.txt"),
        "--depth 1 must include files in first-level subdirectories"
    );
}

// ── --apparent test ───────────────────────────────────────────────────────────

#[test]
fn apparent_flag_runs_successfully_and_produces_output() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!out.stdout.is_empty(), "--apparent must produce output");
}

// ── --follow-symlinks tests ───────────────────────────────────────────────────

#[test]
fn without_follow_symlinks_symlink_target_is_not_counted() {
    // target lives in a separate directory so the walker can only reach it via
    // the symlink.
    let target_dir = tempfile::tempdir().unwrap();
    fs::write(target_dir.path().join("target.txt"), vec![b't'; 1024]).unwrap();

    let scan_dir = tempfile::tempdir().unwrap();
    unix_fs::symlink(
        target_dir.path().join("target.txt"),
        scan_dir.path().join("link"),
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--no-progress", scan_dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "without --follow-symlinks the symlink target must not appear in output"
    );
}

#[test]
fn follow_symlinks_counts_symlink_target() {
    let target_dir = tempfile::tempdir().unwrap();
    fs::write(target_dir.path().join("target.txt"), vec![b't'; 1024]).unwrap();

    let scan_dir = tempfile::tempdir().unwrap();
    unix_fs::symlink(
        target_dir.path().join("target.txt"),
        scan_dir.path().join("link"),
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--follow-symlinks", "--no-progress",
               scan_dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line_count = stdout.lines().count();
    assert_eq!(
        line_count, 1,
        "--follow-symlinks must count the symlink target exactly once, got {} lines",
        line_count
    );
}

// ── --quiet tests ─────────────────────────────────────────────────────────────

#[test]
fn quiet_suppresses_permission_denied_warnings() {
    if is_root() {
        // chmod 000 has no effect for root — skip.
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("visible.txt"), b"hello world").unwrap();
    let locked = tmp.path().join("locked_dir");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("secret.txt"), b"secret").unwrap();

    // Revoke all permissions on the subdirectory.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Ensure permissions are restored on drop — even if assertions panic.
    let _restore = RestoreMode { path: locked.clone() };

    let out_quiet = Command::new(bin())
        .args(["--quiet", "--no-progress", "-f", "flat",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();

    let out_noisy = Command::new(bin())
        .args(["--no-progress", "-f", "flat",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();

    let quiet_stderr = String::from_utf8_lossy(&out_quiet.stderr);
    let noisy_stderr = String::from_utf8_lossy(&out_noisy.stderr);

    assert!(
        out_quiet.status.success(),
        "--quiet must exit 0 even with unreadable subdirectory"
    );
    assert!(
        quiet_stderr.trim().is_empty(),
        "--quiet must produce no stderr output, got: {:?}",
        quiet_stderr
    );
    assert!(
        !noisy_stderr.trim().is_empty(),
        "without --quiet a permission warning must appear on stderr"
    );
}

// ── combination tests ─────────────────────────────────────────────────────────

#[test]
fn combo_type_filter_and_depth() {
    // --type rs -d 1: only .rs files at depth ≤ 1 (root + sub/); no .txt files.
    let tmp = make_fixture(); // gamma.rs(4096), beta.rs(2048), alpha.txt(1024), sub/delta.txt(512)
    let out = Command::new(bin())
        .args(["-f", "flat", "--apparent", "--type", "rs", "-d", "1",
               "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("delta.txt"), "no .txt must appear with --type rs");
    assert!(!stdout.contains("alpha.txt"), "no .txt must appear with --type rs");
    for line in stdout.lines() {
        let path = line.split('\t').nth(1).unwrap_or("");
        assert!(path.ends_with(".rs"), "unexpected non-.rs entry: {:?}", path);
    }
}

#[test]
fn combo_min_size_and_sort_name_json() {
    let tmp = make_fixture();
    // --apparent --min-size 2KB --sort name: beta.rs(2048) and gamma.rs(4096) survive.
    let out = Command::new(bin())
        .args(["-f", "json", "--apparent", "--min-size", "2KB", "--sort", "name",
               "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    // All entries must be above 2 KB.
    for item in arr {
        assert!(item["bytes"].as_u64().unwrap() >= 2 * 1024);
    }
    // Paths must be alphabetically sorted.
    let paths: Vec<&str> = arr.iter()
        .map(|v| v["path"].as_str().unwrap_or(""))
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "json --sort name result must be alphabetical");
}

#[test]
fn combo_by_ext_and_sort_name_csv() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["--apparent", "--by-ext", "--sort", "name", "-f", "csv",
               "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let header = stdout.lines().next().unwrap();
    assert_eq!(header, "extension,bytes,human_size,file_count");
    assert!(
        stdout.lines().count() > 1,
        "--by-ext csv must have at least one data row"
    );
}

#[test]
fn combo_apparent_and_top_n_json() {
    let tmp = make_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--apparent", "-n", "2", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    assert!(arr.len() <= 2, "--apparent -n 2 must return at most 2 JSON entries");
    // With apparent sizes, the top-2 by size must be gamma.rs (4096) then beta.rs (2048).
    if arr.len() == 2 {
        assert!(
            arr[0]["bytes"].as_u64().unwrap() >= arr[1]["bytes"].as_u64().unwrap(),
            "entries must be sorted largest-first (default sort=size)"
        );
    }
}

// ── --dirs-only tests ─────────────────────────────────────────────────────────

/// Fixture for --dirs-only tests.
///
/// Layout (apparent sizes):
/// ```text
///   alpha_dir/
///     file.bin       4 096 B
///     inner/
///       file.bin     2 048 B   (alpha_dir total ≥ 6 144)
///   beta_dir/
///     file.bin       1 024 B
///   gamma_dir/
///     file.bin         512 B
///   root_file.bin    8 192 B   (root-level file, must NOT appear in dirs output)
/// ```
fn make_nested_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let alpha = tmp.path().join("alpha_dir");
    let inner = alpha.join("inner");
    let beta  = tmp.path().join("beta_dir");
    let gamma = tmp.path().join("gamma_dir");
    for d in [&alpha, &inner, &beta, &gamma] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(alpha.join("file.bin"),           vec![0u8; 4096]).unwrap();
    fs::write(inner.join("file.bin"),           vec![0u8; 2048]).unwrap();
    fs::write(beta.join("file.bin"),            vec![0u8; 1024]).unwrap();
    fs::write(gamma.join("file.bin"),           vec![0u8; 512]).unwrap();
    fs::write(tmp.path().join("root_file.bin"), vec![0u8; 8192]).unwrap();
    tmp
}

/// Basic --dirs-only flat output must list subdirectories, not files.
#[test]
fn dirs_only_flat_shows_directories_not_files() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--dirs-only", "--apparent", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "exit code must be 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // root_file.bin is a plain file at the scan root — must not appear
    assert!(
        !stdout.contains("root_file.bin"),
        "root-level files must not appear in --dirs-only output"
    );
    // Every non-empty line must reference a known directory name
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            line.contains("alpha_dir") || line.contains("beta_dir")
                || line.contains("gamma_dir") || line.contains("inner"),
            "unexpected entry in --dirs-only output: {line}"
        );
    }
}

/// --dirs-only -n 2 must return at most 2 output lines.
#[test]
fn dirs_only_top_n_limits_output() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--dirs-only", "--apparent", "-n", "2", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let non_empty: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        non_empty.len() <= 2,
        "--dirs-only -n 2 must yield at most 2 lines, got {}",
        non_empty.len()
    );
}

/// --dirs-only --sort name must output paths in alphabetical order.
#[test]
fn dirs_only_sort_name_is_alphabetical() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "flat", "--dirs-only", "--apparent", "--sort", "name", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let paths: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').nth(1).unwrap_or(l).trim())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "--dirs-only --sort name must be alphabetical");
}

/// --dirs-only with a --min-size larger than all dirs must produce no output.
#[test]
fn dirs_only_min_size_filters_all_dirs() {
    let tmp = make_nested_fixture();
    // 100 MB — no dir in the fixture comes close
    let out = Command::new(bin())
        .args(["-f", "flat", "--dirs-only", "--apparent", "--min-size", "104857600",
               "--no-progress", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "no directory exceeds 100 MB; output must be empty, got: {stdout}"
    );
}

/// --dirs-only with JSON format must emit an array with {path, bytes, human}.
#[test]
fn dirs_only_json_has_required_keys() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--dirs-only", "--apparent", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().expect("--dirs-only json must be a JSON array");
    assert!(!arr.is_empty(), "JSON array must have at least one entry");
    for item in arr {
        assert!(item["path"].is_string(),  "each entry must have a 'path' string");
        assert!(item["bytes"].is_number(), "each entry must have a 'bytes' number");
        assert!(item["human"].is_string(), "each entry must have a 'human' string");
        assert!(
            item.get("file_count").is_none(),
            "'file_count' must NOT appear in --dirs-only json output"
        );
    }
}

/// --dirs-only JSON: default sort must be descending by size.
#[test]
fn dirs_only_json_largest_dir_first_by_default() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "json", "--dirs-only", "--apparent", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = parsed.as_array().unwrap();
    assert!(arr.len() >= 2, "must have at least 2 dir entries for ordering check");
    assert!(
        arr[0]["bytes"].as_u64().unwrap() >= arr[1]["bytes"].as_u64().unwrap(),
        "default --dirs-only JSON sort must be descending by size"
    );
}

/// --dirs-only with CSV format must have the correct header and 3-column rows.
#[test]
fn dirs_only_csv_has_correct_header_and_data() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["-f", "csv", "--dirs-only", "--apparent", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let header = lines.next().expect("CSV must have a header line");
    assert_eq!(header, "path,bytes,human_size", "CSV header must be 'path,bytes,human_size'");
    let data_rows: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    assert!(!data_rows.is_empty(), "CSV must have at least one data row");
    for row in &data_rows {
        let cols: Vec<&str> = row.splitn(3, ',').collect();
        assert_eq!(cols.len(), 3, "each CSV data row must have 3 columns: {row}");
    }
}

/// --dirs-only on a directory that contains only files must produce no output.
#[test]
fn dirs_only_files_only_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("foo.txt"), b"hello").unwrap();
    fs::write(tmp.path().join("bar.txt"), b"world").unwrap();

    let out = Command::new(bin())
        .args(["-f", "flat", "--dirs-only", "--apparent", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "dirs with only files must produce empty --dirs-only output"
    );
}

/// --dirs-only combined with --by-ext must exit non-zero and print to stderr.
#[test]
fn dirs_only_and_by_ext_is_an_error() {
    let tmp = make_nested_fixture();
    let out = Command::new(bin())
        .args(["--dirs-only", "--by-ext", "--no-progress",
               tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--dirs-only and --by-ext together must fail with a non-zero exit code"
    );
    assert!(
        !out.stderr.is_empty(),
        "--dirs-only --by-ext must print an error message to stderr"
    );
}
