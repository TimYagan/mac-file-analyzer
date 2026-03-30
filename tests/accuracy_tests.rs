//! Phase 5 accuracy integration tests.
//!
//! Each test creates a controlled filesystem fixture with known properties and
//! verifies that the walkers and output renderers report the correct values.

use std::fs;
use std::os::unix::fs as unix_fs;
use tempfile::TempDir;

use mac_file_analyzer::aggregator::{EntryNode, SizeMode, SortOrder};
use mac_file_analyzer::walker::{walk, walk_parallel, walk_parallel_getattrlist, WalkOptions};
use mac_file_analyzer::{formatter, output};

// ── helpers ───────────────────────────────────────────────────────────────────

fn opts() -> WalkOptions {
    WalkOptions::default()
}

fn walk_seq(dir: &std::path::Path) -> mac_file_analyzer::aggregator::DirNode {
    walk(dir, &opts(), &mut |_| {}).unwrap()
}

/// Build a mixed tree: 3 subdirectories with varied file sizes and extensions.
///
/// Layout:
/// ```
///   docs/guide.md      2 048 B
///   docs/notes.txt       512 B
///   src/main.rs        4 096 B
///   src/lib.rs         1 024 B
///   data/dump.csv      8 192 B
///   data/archive.tar   65 536 B
/// ```
fn build_mixed_tree() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (dir_name, files) in [
        ("docs", vec![("guide.md", 2048usize), ("notes.txt", 512)]),
        ("src",  vec![("main.rs", 4096),        ("lib.rs", 1024)]),
        ("data", vec![("dump.csv", 8192),        ("archive.tar", 65536)]),
    ] {
        let sub = tmp.path().join(dir_name);
        fs::create_dir(&sub).unwrap();
        for (filename, size) in files {
            fs::write(sub.join(filename), vec![0u8; size]).unwrap();
        }
    }
    tmp
}

// ── hardlink tests ────────────────────────────────────────────────────────────

/// Two hardlinks to the same inode must be counted as a single file.
#[test]
fn hardlinks_counted_once() {
    let tmp = tempfile::tempdir().unwrap();
    let orig = tmp.path().join("real.dat");
    fs::write(&orig, vec![0u8; 4096]).unwrap();
    fs::hard_link(&orig, tmp.path().join("link.dat")).unwrap();

    let node = walk_seq(tmp.path());
    assert_eq!(node.file_count, 1, "hardlinked inode must be counted exactly once");
}

/// Three files where two share an inode (hardlinks) → file_count must equal 2.
#[test]
fn multiple_hardlinks_counted_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.dat"), vec![1u8; 1000]).unwrap();
    let b = tmp.path().join("b.dat");
    fs::write(&b, vec![2u8; 2000]).unwrap();
    fs::hard_link(&b, tmp.path().join("b_link.dat")).unwrap();

    let node = walk_seq(tmp.path());
    assert_eq!(node.file_count, 2, "two distinct inodes → file_count = 2");
}

/// Total size with hardlinks must equal the size of the distinct inodes, not
/// the sum of all directory entries.
#[test]
fn hardlinks_total_size_is_not_doubled() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("f.dat");
    let content = vec![0u8; 8192];
    fs::write(&path, &content).unwrap();
    fs::hard_link(&path, tmp.path().join("f2.dat")).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(&tmp.path().to_path_buf(), &apparent_opts, &mut |_| {}).unwrap();

    // Apparent size must be 8 192 B (not 16 384 B).
    assert_eq!(
        node.total_size, 8192,
        "apparent total must not double-count hardlinked bytes"
    );
}

// ── symlink tests ─────────────────────────────────────────────────────────────

/// Symlinks are skipped by default; the linked file must not be counted.
#[test]
fn symlink_not_followed_by_default() {
    // Target lives in a separate dir so the walker cannot reach it directly.
    let target_dir = tempfile::tempdir().unwrap();
    fs::write(target_dir.path().join("big.dat"), vec![0u8; 8192]).unwrap();

    let scan_dir = tempfile::tempdir().unwrap();
    unix_fs::symlink(
        target_dir.path().join("big.dat"),
        scan_dir.path().join("link"),
    )
    .unwrap();

    let node = walk_seq(scan_dir.path());
    assert_eq!(node.file_count, 0, "symlink must not be counted without --follow-symlinks");
    assert_eq!(node.total_size, 0);
}

/// With `--follow-symlinks`, the symlink target must be counted exactly once.
#[test]
fn symlink_followed_when_flag_set() {
    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("target.dat");
    fs::write(&target, vec![0u8; 4096]).unwrap();

    let scan_dir = tempfile::tempdir().unwrap();
    unix_fs::symlink(&target, scan_dir.path().join("link")).unwrap();

    let mut follow_opts = opts();
    follow_opts.follow_symlinks = true;
    let node = walk(scan_dir.path(), &follow_opts, &mut |_| {}).unwrap();

    assert_eq!(node.file_count, 1, "symlink target must be counted with --follow-symlinks");
}

/// A symlink that points back to an ancestor directory must not cause an
/// infinite loop — the walker must terminate and count each real file once.
#[test]
fn symlink_cycle_does_not_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("real.dat"), vec![0u8; 512]).unwrap();
    // Create a cycle: sub/cycle → tmp (ancestor)
    unix_fs::symlink(tmp.path(), sub.join("cycle")).unwrap();

    let mut follow_opts = opts();
    follow_opts.follow_symlinks = true;

    let node = walk(tmp.path(), &follow_opts, &mut |_| {}).unwrap();
    // The real file inode must be counted exactly once.
    assert_eq!(node.file_count, 1, "cycle: real file must appear exactly once");
}

// ── sparse file tests ─────────────────────────────────────────────────────────

/// A sparse file has an apparent (logical) size larger than its disk usage.
///
/// APFS does not always produce traditional sparse files via seek+write — it
/// may allocate full extents immediately.  The test detects this at runtime and
/// skips gracefully so that the assertion only fires on filesystems that
/// produce genuine holes (e.g. HFS+, ext4).
#[test]
fn sparse_file_apparent_exceeds_disk_usage() {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sparse.dat");
    // Create a 4 MiB sparse file — only the last byte is written.
    let mut file = fs::File::create(&path).unwrap();
    file.seek(SeekFrom::Start(4 * 1024 * 1024 - 1)).unwrap();
    file.write_all(&[0xFF]).unwrap();
    drop(file);

    // Check whether the OS actually produced a sparse file.  On APFS the
    // kernel may immediately allocate the full extent, making st_blocks*512
    // equal to st_size.  In that case the test is vacuously true — just skip.
    let meta = fs::metadata(&path).unwrap();
    let blocks_bytes = meta.blocks() * 512; // st_blocks * 512
    let apparent_bytes = meta.len();         // st_size
    if blocks_bytes >= apparent_bytes {
        eprintln!(
            "skip sparse_file_apparent_exceeds_disk_usage: \
             filesystem allocated full extent (blocks*512={}, size={}); \
             APFS does not create traditional sparse holes via seek+write",
            blocks_bytes, apparent_bytes
        );
        // Still verify apparent size is correctly reported.
        let mut apparent_opts = opts();
        apparent_opts.size_mode = SizeMode::Apparent;
        let apparent_node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();
        assert_eq!(
            apparent_node.total_size,
            4 * 1024 * 1024,
            "apparent size must equal the logical file length even when not sparse"
        );
        return;
    }

    let mut disk_opts = opts();
    disk_opts.size_mode = SizeMode::DiskUsage;
    let disk_node = walk(tmp.path(), &disk_opts, &mut |_| {}).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let apparent_node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();

    assert_eq!(
        apparent_node.total_size,
        4 * 1024 * 1024,
        "apparent size must equal the logical file length"
    );
    assert!(
        disk_node.total_size < apparent_node.total_size,
        "disk usage ({}) must be less than apparent size ({}) for a sparse file",
        disk_node.total_size,
        apparent_node.total_size
    );
}

// ── extension filter tests ────────────────────────────────────────────────────

/// Only files matching the requested extension should be counted.
#[test]
fn extension_filter_includes_only_matching_files() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.mp4"), vec![0u8; 2048]).unwrap();
    fs::write(tmp.path().join("b.mp4"), vec![0u8; 1024]).unwrap();
    fs::write(tmp.path().join("c.txt"), vec![0u8; 512]).unwrap();

    let mut ext_opts = opts();
    ext_opts.filter_ext = Some("mp4".to_string());
    let node = walk(tmp.path(), &ext_opts, &mut |_| {}).unwrap();

    assert_eq!(node.file_count, 2, "only .mp4 files should be counted");
    // .txt must not appear in children.
    let has_txt = node.children.iter().any(|e| {
        matches!(e, EntryNode::File(f) if f.path.to_string_lossy().ends_with(".txt"))
    });
    assert!(!has_txt, ".txt must be excluded when filter_ext = \"mp4\"");
}

/// Extension matching is case-insensitive.
#[test]
fn extension_filter_is_case_insensitive() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("video.MP4"), vec![0u8; 1024]).unwrap();
    fs::write(tmp.path().join("clip.mp4"),  vec![0u8; 1024]).unwrap();
    fs::write(tmp.path().join("doc.txt"),   vec![0u8;  256]).unwrap();

    let mut ext_opts = opts();
    ext_opts.filter_ext = Some("mp4".to_string());
    let node = walk(tmp.path(), &ext_opts, &mut |_| {}).unwrap();

    assert_eq!(node.file_count, 2, ".MP4 and .mp4 both match case-insensitive filter");
}

// ── depth limit tests ─────────────────────────────────────────────────────────

/// `max_depth = 0` must not descend into any subdirectory.
#[test]
fn depth_limit_zero_excludes_all_children() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("deep.dat"), vec![0u8; 1024]).unwrap();

    let mut depth_opts = opts();
    depth_opts.max_depth = Some(0);
    let node = walk(tmp.path(), &depth_opts, &mut |_| {}).unwrap();

    assert_eq!(node.total_size, 0, "depth 0 must not descend into subdirectories");
    assert_eq!(node.file_count, 0);
}

/// `max_depth = 1` counts files in direct subdirectories but not deeper.
#[test]
fn depth_limit_one_includes_first_level_only() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("level1.dat"), vec![0u8; 512]).unwrap();
    let sub2 = sub.join("sub2");
    fs::create_dir(&sub2).unwrap();
    fs::write(sub2.join("level2.dat"), vec![0u8; 1024]).unwrap();

    let mut depth_opts = opts();
    depth_opts.max_depth = Some(1);
    let node = walk(tmp.path(), &depth_opts, &mut |_| {}).unwrap();

    // Only level1.dat (512 B) must be counted; level2.dat must be excluded.
    assert_eq!(node.file_count, 1, "depth 1 must include only first-level files");
}

// ── parallel walker consistency ───────────────────────────────────────────────

/// The parallel (rayon) walker must produce identical totals to the sequential
/// walker for the same fixture.
#[test]
fn parallel_walk_matches_sequential() {
    let tmp = build_mixed_tree();
    let o = opts();
    let seq = walk(tmp.path(), &o, &mut |_| {}).unwrap();
    let par = walk_parallel(tmp.path(), &o, &|_| {}).unwrap();

    assert_eq!(seq.total_size, par.total_size, "total_size: seq == par");
    assert_eq!(seq.file_count, par.file_count, "file_count: seq == par");
}

/// The getattrlist-based parallel walker must produce identical totals to the
/// sequential walker for the same fixture.
#[test]
fn getattrlist_walk_matches_sequential() {
    let tmp = build_mixed_tree();
    let o = opts();
    let seq = walk(tmp.path(), &o, &mut |_| {}).unwrap();
    let geo = walk_parallel_getattrlist(tmp.path(), &o, &|_| {}).unwrap();

    assert_eq!(seq.total_size, geo.total_size, "total_size: seq == getattrlist");
    assert_eq!(seq.file_count, geo.file_count, "file_count: seq == getattrlist");
}

/// All three walkers agree on total_size and file_count.
#[test]
fn all_walkers_agree() {
    let tmp = build_mixed_tree();
    let o = opts();
    let seq = walk(tmp.path(), &o, &mut |_| {}).unwrap();
    let par = walk_parallel(tmp.path(), &o, &|_| {}).unwrap();
    let geo = walk_parallel_getattrlist(tmp.path(), &o, &|_| {}).unwrap();

    assert_eq!(seq.total_size, par.total_size);
    assert_eq!(seq.total_size, geo.total_size);
    assert_eq!(seq.file_count, par.file_count);
    assert_eq!(seq.file_count, geo.file_count);
}

/// Parallel walkers produce the same file_count as sequential when hardlinks
/// are present.
#[test]
fn parallel_walkers_deduplicate_hardlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let orig = tmp.path().join("orig.dat");
    fs::write(&orig, vec![0u8; 4096]).unwrap();
    for i in 0..4 {
        fs::hard_link(&orig, tmp.path().join(format!("link{}.dat", i))).unwrap();
    }

    let o = opts();
    let seq = walk(tmp.path(), &o, &mut |_| {}).unwrap();
    let par = walk_parallel(tmp.path(), &o, &|_| {}).unwrap();
    let geo = walk_parallel_getattrlist(tmp.path(), &o, &|_| {}).unwrap();

    assert_eq!(seq.file_count, 1, "sequential: one unique inode");
    assert_eq!(par.file_count, 1, "parallel: one unique inode");
    assert_eq!(geo.file_count, 1, "getattrlist: one unique inode");
}

// ── resource fork tests ───────────────────────────────────────────────────────

/// When `--include-rsrc` is set, the walker must add resource fork bytes to
/// the reported size.
///
/// Resource forks are only read by the `getattrlist`-based walker (`walk_parallel_getattrlist`);
/// the sequential `walk()` uses `lstat` which has no resource-fork field.  The
/// test therefore exercises `walk_parallel_getattrlist` and skips gracefully if
/// the underlying file system does not support named forks (e.g. Linux tmpfs
/// or APFS volumes mounted with MNT_NOFOLLOW_RSRC).
#[test]
fn resource_fork_increases_reported_size() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fork_test.dat");
    fs::write(&path, vec![b'A'; 1024]).unwrap();

    // macOS resource fork path: <file>/..namedfork/rsrc
    let rsrc_path = path.join("..namedfork").join("rsrc");
    match fs::write(&rsrc_path, vec![b'R'; 2048]) {
        Err(_) => {
            eprintln!("skip resource_fork_increases_reported_size: FS does not support named forks");
            return;
        }
        Ok(_) => {}
    }

    // Use the getattrlist walker — only it reads ATTR_FILE_RSRCLENGTH.
    let node_no_rsrc =
        walk_parallel_getattrlist(tmp.path(), &opts(), &|_| {}).unwrap();

    let mut rsrc_opts = opts();
    rsrc_opts.include_rsrc = true;
    let node_with_rsrc =
        walk_parallel_getattrlist(tmp.path(), &rsrc_opts, &|_| {}).unwrap();

    assert!(
        node_with_rsrc.total_size > node_no_rsrc.total_size,
        "--include-rsrc must add resource fork bytes: {} <= {}",
        node_with_rsrc.total_size,
        node_no_rsrc.total_size
    );
}

// ── aggregator / filter tests ─────────────────────────────────────────────────

/// `flat_sorted_with` with a `min_size` filter must exclude entries below the
/// threshold.
#[test]
fn min_size_filter_excludes_small_entries() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("big.dat"),   vec![0u8; 65536]).unwrap();
    fs::write(tmp.path().join("small.dat"), vec![0u8;   512]).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();

    let threshold: u64 = 10 * 1024; // 10 KiB
    let entries = node.flat_sorted_with(SortOrder::Size, Some(threshold));

    for (_, size) in &entries {
        assert!(
            *size >= threshold,
            "entry with size {} must not appear below min_size {}",
            size,
            threshold
        );
    }
}

/// `flat_sorted_with` with `SortOrder::Size` must return entries largest-first.
#[test]
fn flat_sorted_with_respects_size_order() {
    let tmp = build_mixed_tree();
    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();

    let entries = node.flat_sorted_with(SortOrder::Size, None);
    for window in entries.windows(2) {
        assert!(window[0].1 >= window[1].1, "entries must be sorted largest-first");
    }
}

/// `by_extension_sorted` groups files by extension correctly.
#[test]
fn by_extension_groups_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.mp4"), vec![0u8; 3000]).unwrap();
    fs::write(tmp.path().join("b.mp4"), vec![0u8; 1000]).unwrap();
    fs::write(tmp.path().join("c.txt"), vec![0u8;  500]).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();
    let rows = node.by_extension_sorted(SortOrder::Size);

    let mp4 = rows.iter().find(|(e, _, _)| e == "mp4").expect("mp4 row must exist");
    let txt = rows.iter().find(|(e, _, _)| e == "txt").expect("txt row must exist");

    assert_eq!(mp4.2, 2, "two .mp4 files");
    assert_eq!(txt.2, 1, "one .txt file");
    // mp4 total (4000 B) is larger → must rank first.
    assert_eq!(rows[0].0, "mp4", "mp4 must rank first with SortOrder::Size");
}

/// Files with no extension must appear under the empty-string key (displayed as
/// `(no ext)` in the formatter — not as `.`).
#[test]
fn by_extension_no_ext_key() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Makefile"),  vec![0u8; 256]).unwrap();
    fs::write(tmp.path().join("README"),    vec![0u8; 128]).unwrap();
    fs::write(tmp.path().join("script.sh"), vec![0u8;  64]).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();
    let rows = node.by_extension_sorted(SortOrder::Name);

    let no_ext = rows.iter().find(|(e, _, _)| e.is_empty());
    assert!(no_ext.is_some(), "extension-less files must appear under empty-string key");
    assert_eq!(no_ext.unwrap().2, 2, "Makefile + README → count = 2");
}

// ── output format tests ───────────────────────────────────────────────────────

/// JSON flat output must be syntactically valid JSON.
#[test]
fn json_flat_output_is_valid_json() {
    let tmp = build_mixed_tree();
    let node = walk_seq(tmp.path());
    let json_str = output::json::render_json(&node, Some(20), SortOrder::Size, None);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json_str);
    assert!(parsed.is_ok(), "flat JSON must be valid: {:?}", parsed.err());
}

/// JSON flat output must contain the expected keys.
#[test]
fn json_flat_output_has_expected_keys() {
    let tmp = build_mixed_tree();
    let node = walk_seq(tmp.path());
    let json_str = output::json::render_json(&node, Some(20), SortOrder::Size, None);
    let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    let arr = v.as_array().expect("top-level must be a JSON array");
    assert!(!arr.is_empty(), "JSON array must not be empty");
    let item = &arr[0];
    assert!(item.get("path").is_some(),  "item must have 'path' key");
    assert!(item.get("bytes").is_some(), "item must have 'bytes' key");
    assert!(item.get("human").is_some(), "item must have 'human' key");
}

/// JSON by-ext output must be valid JSON with the expected keys.
#[test]
fn json_by_ext_output_is_valid_json() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.rs"), vec![0u8; 200]).unwrap();
    fs::write(tmp.path().join("b.rs"), vec![0u8; 100]).unwrap();
    let node = walk_seq(tmp.path());
    let json_str = output::json::render_json_by_ext(&node, SortOrder::Size);
    let v: serde_json::Value = serde_json::from_str(&json_str).expect("must be valid JSON");
    let arr = v.as_array().expect("must be array");
    assert!(!arr.is_empty());
    let item = &arr[0];
    assert!(item.get("extension").is_some(), "item must have 'extension' key");
    assert!(item.get("bytes").is_some(),     "item must have 'bytes' key");
    assert!(item.get("file_count").is_some(),"item must have 'file_count' key");
}

/// CSV flat output must have a header row and at least one data row.
#[test]
fn csv_flat_output_has_header_and_data() {
    let tmp = build_mixed_tree();
    let node = walk_seq(tmp.path());
    let csv_str = output::csv::render_csv(&node, Some(20), SortOrder::Size, None);
    let mut lines = csv_str.lines();
    let header = lines.next().expect("CSV must have a header line");
    assert!(header.contains("path"),  "header must contain 'path'");
    assert!(header.contains("bytes"), "header must contain 'bytes'");
    assert!(lines.next().is_some(),   "CSV must have at least one data row");
}

/// CSV by-ext output must have the expected header columns.
#[test]
fn csv_by_ext_output_has_correct_header() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.rs"), vec![0u8; 100]).unwrap();
    let node = walk_seq(tmp.path());
    let csv_str = output::csv::render_csv_by_ext(&node, SortOrder::Size);
    let header = csv_str.lines().next().expect("must have a header line");
    assert!(header.contains("extension"), "header must contain 'extension'");
    assert!(header.contains("bytes"),     "header must contain 'bytes'");
    assert!(header.contains("file_count"),"header must contain 'file_count'");
}

/// Tree output must be non-empty and contain the root path.
#[test]
fn tree_output_contains_root_path() {
    let tmp = build_mixed_tree();
    let node = walk_seq(tmp.path());
    let tree_str = formatter::render_tree(&node, None, SortOrder::Size);
    assert!(!tree_str.is_empty(), "tree output must not be empty");
    assert!(
        tree_str.contains(tmp.path().to_str().unwrap()),
        "tree output must include the root path"
    );
}

/// `render_tree` must not exceed `max_depth` levels.
#[test]
fn tree_output_respects_max_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    let sub2 = sub.join("sub2");
    fs::create_dir_all(&sub2).unwrap();
    fs::write(sub.join("level1.dat"), vec![0u8; 512]).unwrap();
    fs::write(sub2.join("level2.dat"), vec![0u8; 256]).unwrap();

    let node = walk_seq(tmp.path());
    // Render with depth = 1: sub/ is visible but sub/sub2/ and its contents
    // should not appear.
    let tree_str = formatter::render_tree(&node, Some(1), SortOrder::Size);
    assert!(
        !tree_str.contains("level2.dat"),
        "depth=1 tree must not render files at depth 2"
    );
}

/// Flat output entries must appear in descending size order.
#[test]
fn flat_output_is_size_descending() {
    let tmp = build_mixed_tree();
    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();

    let entries = node.flat_sorted_with(SortOrder::Size, None);
    for window in entries.windows(2) {
        assert!(
            window[0].1 >= window[1].1,
            "flat entries must be ordered largest first: {} < {}",
            window[0].1,
            window[1].1
        );
    }
}

/// `render_by_extension` must not prefix extension-less files with a dot.
#[test]
fn by_extension_render_no_dot_prefix_for_no_ext() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Makefile"), vec![0u8; 300]).unwrap();
    fs::write(tmp.path().join("a.rs"),     vec![0u8; 100]).unwrap();

    let mut apparent_opts = opts();
    apparent_opts.size_mode = SizeMode::Apparent;
    let node = walk(tmp.path(), &apparent_opts, &mut |_| {}).unwrap();
    let rendered = formatter::render_by_extension(&node, SortOrder::Size);

    // Extension-less display must use "(no ext)", not "."
    assert!(
        rendered.contains("(no ext)"),
        "extension-less files must display as '(no ext)'"
    );
    // Must NOT appear as a bare "." entry
    assert!(
        !rendered.lines().any(|l| l.trim_start().starts_with(". ")),
        "extension-less files must not be prefixed with a bare '.'"
    );
}

// ── combination walker-option tests ──────────────────────────────────────────

/// `filter_ext` + `max_depth` combined: only matching-extension files within
/// the depth limit must be counted.
#[test]
fn combo_type_filter_and_max_depth() {
    let tmp = tempfile::tempdir().unwrap();
    // Depth 0 (root): root.rs → counted, root.txt → not counted (wrong ext).
    fs::write(tmp.path().join("root.rs"),  vec![0u8; 100]).unwrap();
    fs::write(tmp.path().join("root.txt"), vec![0u8; 200]).unwrap();
    // Depth 1 (sub/): sub.rs → counted, sub.txt → not counted (wrong ext).
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("sub.rs"),  vec![0u8; 300]).unwrap();
    fs::write(sub.join("sub.txt"), vec![0u8; 400]).unwrap();
    // Depth 2 (sub/deep/): deep.rs → excluded by max_depth=1.
    let deep = sub.join("deep");
    fs::create_dir(&deep).unwrap();
    fs::write(deep.join("deep.rs"), vec![0u8; 500]).unwrap();

    let combo_opts = WalkOptions {
        size_mode: SizeMode::Apparent,
        filter_ext: Some("rs".to_string()),
        max_depth: Some(1),
        ..WalkOptions::default()
    };
    let node = walk(tmp.path(), &combo_opts, &mut |_| {}).unwrap();

    assert_eq!(
        node.file_count, 2,
        "only root.rs and sub/sub.rs must be counted; deep.rs is below max_depth, .txt files excluded"
    );
    assert_eq!(node.total_size, 400, "100 (root.rs) + 300 (sub.rs) = 400 B");
}

/// `follow_symlinks` + `max_depth` combined: a symlink to a directory at depth
/// 0 resolves like a real directory; depth counting still applies.
#[test]
fn combo_follow_symlinks_and_max_depth() {
    let target_dir = tempfile::tempdir().unwrap();
    fs::write(target_dir.path().join("buried.dat"), vec![0u8; 1024]).unwrap();

    let scan_dir = tempfile::tempdir().unwrap();
    // Place a symlink to target_dir at the root of scan_dir (depth 0).
    unix_fs::symlink(target_dir.path(), scan_dir.path().join("link_dir")).unwrap();

    // With follow_symlinks=true and max_depth=1: the symlink resolves at depth
    // 0 and we descend into it (depth 1 from the walker's perspective when
    // entering link_dir).  buried.dat is at depth 1 → counted.
    let follow_opts = WalkOptions {
        size_mode: SizeMode::Apparent,
        follow_symlinks: true,
        max_depth: Some(1),
        ..WalkOptions::default()
    };
    let node = walk(scan_dir.path(), &follow_opts, &mut |_| {}).unwrap();

    assert_eq!(node.file_count, 1, "symlink target's file must be reachable via follow_symlinks");
    assert_eq!(node.total_size, 1024);
}

/// `apparent` size mode + `include_rsrc` flag for the getattrlist walker:
/// a plain file without a resource fork must report identical totals regardless
/// of `include_rsrc`, because rsrc_size is 0 for such files.
#[test]
fn combo_apparent_and_include_rsrc_plain_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("plain.bin"), vec![b'Z'; 512]).unwrap();

    let base_opts = WalkOptions {
        size_mode: SizeMode::Apparent,
        include_rsrc: false,
        ..WalkOptions::default()
    };
    let rsrc_opts = WalkOptions {
        size_mode: SizeMode::Apparent,
        include_rsrc: true,
        ..WalkOptions::default()
    };

    let base_node = walk_parallel_getattrlist(tmp.path(), &base_opts, &|_| {}).unwrap();
    let rsrc_node = walk_parallel_getattrlist(tmp.path(), &rsrc_opts, &|_| {}).unwrap();

    assert_eq!(
        base_node.total_size, rsrc_node.total_size,
        "a plain file with rsrc_size=0 must report the same total regardless of include_rsrc"
    );
    assert_eq!(base_node.file_count, rsrc_node.file_count);
}

/// `filter_ext` case-insensitivity with parallel walker: both walkers must
/// count the same files when an extension filter is applied.
#[test]
fn combo_case_insensitive_ext_all_walkers_agree() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("video.MP4"), vec![0u8; 4096]).unwrap();
    fs::write(tmp.path().join("clip.mp4"),  vec![0u8; 2048]).unwrap();
    fs::write(tmp.path().join("doc.txt"),   vec![0u8; 1024]).unwrap();

    let ext_opts = WalkOptions {
        size_mode: SizeMode::Apparent,
        filter_ext: Some("mp4".to_string()),
        ..WalkOptions::default()
    };

    let seq = walk(tmp.path(), &ext_opts, &mut |_| {}).unwrap();
    let par = walk_parallel(tmp.path(), &ext_opts, &|_| {}).unwrap();
    let geo = walk_parallel_getattrlist(tmp.path(), &ext_opts, &|_| {}).unwrap();

    assert_eq!(seq.file_count, 2, "sequential: both .mp4 and .MP4 must match");
    assert_eq!(par.file_count, 2, "parallel: both .mp4 and .MP4 must match");
    assert_eq!(geo.file_count, 2, "getattrlist: both .mp4 and .MP4 must match");
    assert_eq!(seq.total_size, par.total_size);
    assert_eq!(seq.total_size, geo.total_size);
}

// ── dirs_sorted_with walker-level accuracy tests ──────────────────────────────

/// `dirs_sorted_with` must return only subdirectories, never the scan root,
/// and never plain files.
#[test]
fn dirs_sorted_with_returns_only_subdirs() {
    let tmp = tempfile::tempdir().unwrap();
    let sub_a = tmp.path().join("subA");
    let sub_b = tmp.path().join("subB");
    fs::create_dir(&sub_a).unwrap();
    fs::create_dir(&sub_b).unwrap();
    fs::write(sub_a.join("file.bin"), vec![0u8; 1024]).unwrap();
    fs::write(sub_b.join("file.bin"), vec![0u8; 2048]).unwrap();
    fs::write(tmp.path().join("root_file.bin"), vec![0u8; 512]).unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();

    let dirs = node.dirs_sorted_with(SortOrder::Size, None);
    assert_eq!(dirs.len(), 2, "exactly 2 subdirectories, not root and not files");
    // Largest first: subB (2048), subA (1024)
    assert_eq!(dirs[0].1, 2048, "subB must be first (largest)");
    assert_eq!(dirs[1].1, 1024, "subA must be second");
    // Scan root must not appear
    assert!(
        !dirs.iter().any(|(p, _)| *p == tmp.path()),
        "scan root must not appear in dirs_sorted_with output"
    );
}

/// Nested directories: `dirs_sorted_with` must recurse and collect all levels.
#[test]
fn dirs_sorted_with_collects_nested_dirs_recursively() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).unwrap();
    fs::write(outer.join("file.bin"), vec![0u8; 2048]).unwrap();
    fs::write(inner.join("file.bin"), vec![0u8; 1024]).unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();

    let dirs = node.dirs_sorted_with(SortOrder::Size, None);
    assert_eq!(dirs.len(), 2, "outer and inner must both be collected");
    assert_eq!(dirs[0].1, 3072, "outer's total (2048 + 1024) must be largest");
    assert_eq!(dirs[1].1, 1024, "inner must be second");
}

/// `dirs_sorted_with` with `min_size` only keeps directories above the threshold.
#[test]
fn dirs_sorted_with_min_size_filters_small_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let big   = tmp.path().join("big");
    let small = tmp.path().join("small");
    fs::create_dir(&big).unwrap();
    fs::create_dir(&small).unwrap();
    fs::write(big.join("file.bin"),   vec![0u8; 4096]).unwrap();
    fs::write(small.join("file.bin"), vec![0u8; 100]).unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();
    let dirs = node.dirs_sorted_with(SortOrder::Size, Some(1000));

    assert_eq!(dirs.len(), 1, "only 'big' survives min_size=1000");
    assert_eq!(dirs[0].1, 4096);
}

/// A directory that contains only files (no subdirs) must give an empty result.
#[test]
fn dirs_sorted_with_no_subdirs_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("file.txt"), b"hello world").unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();

    assert!(
        node.dirs_sorted_with(SortOrder::Size, None).is_empty(),
        "no subdirectories must yield an empty dir list"
    );
}

/// An entirely empty directory must also give an empty result (no panic).
#[test]
fn dirs_sorted_with_empty_dir_does_not_crash() {
    let tmp = tempfile::tempdir().unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();

    assert!(node.dirs_sorted_with(SortOrder::Size, None).is_empty());
}

/// `SortOrder::Name` must produce alphabetically sorted paths.
#[test]
fn dirs_sorted_with_name_order_is_alphabetical() {
    let tmp = tempfile::tempdir().unwrap();
    for name in &["charlie", "alpha", "bravo"] {
        let d = tmp.path().join(name);
        fs::create_dir(&d).unwrap();
        fs::write(d.join("f.bin"), b"x").unwrap();
    }

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();
    let dirs = node.dirs_sorted_with(SortOrder::Name, None);

    let paths: Vec<String> = dirs.iter().map(|(p, _)| p.display().to_string()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "dirs_sorted_with SortOrder::Name must be alphabetical");
}

/// `dirs_sorted_with` with `min_size=0` must behave identically to `None`.
#[test]
fn dirs_sorted_with_min_size_zero_passes_all() {
    let tmp = tempfile::tempdir().unwrap();
    for name in &["a", "b"] {
        let d = tmp.path().join(name);
        fs::create_dir(&d).unwrap();
        fs::write(d.join("f.bin"), b"x").unwrap();
    }

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let node = walk(tmp.path(), &opts, &mut |_| {}).unwrap();

    assert_eq!(
        node.dirs_sorted_with(SortOrder::Size, Some(0)).len(),
        node.dirs_sorted_with(SortOrder::Size, None).len(),
        "min_size=0 must not filter anything"
    );
}

/// All three walkers must agree on `dirs_sorted_with` totals.
#[test]
fn dirs_sorted_with_all_walkers_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("file.bin"), vec![0u8; 8192]).unwrap();

    let opts = WalkOptions { size_mode: SizeMode::Apparent, ..WalkOptions::default() };
    let seq = walk(tmp.path(), &opts, &mut |_| {}).unwrap();
    let par = walk_parallel(tmp.path(), &opts, &|_| {}).unwrap();
    let geo = walk_parallel_getattrlist(tmp.path(), &opts, &|_| {}).unwrap();

    let seq_dirs = seq.dirs_sorted_with(SortOrder::Size, None);
    let par_dirs = par.dirs_sorted_with(SortOrder::Size, None);
    let geo_dirs = geo.dirs_sorted_with(SortOrder::Size, None);

    assert_eq!(seq_dirs.len(), par_dirs.len(), "seq and par must find the same number of dirs");
    assert_eq!(seq_dirs.len(), geo_dirs.len(), "seq and geo must find the same number of dirs");
    assert_eq!(seq_dirs[0].1, par_dirs[0].1, "seq and par total must agree");
    assert_eq!(seq_dirs[0].1, geo_dirs[0].1, "seq and geo total must agree");
}
