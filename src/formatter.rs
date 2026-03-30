/// Human-readable formatting and tree/flat output renderers.
use std::fmt::Write as FmtWrite;

use crate::aggregator::{DirNode, EntryNode, SortOrder};

/// Format a byte count as a compact human-readable string.
/// Uses IEC binary prefixes (KiB, MiB, GiB, TiB).
pub fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = 1_024 * KIB;
    const GIB: u64 = 1_024 * MIB;
    const TIB: u64 = 1_024 * GIB;

    if bytes >= TIB {
        format!("{:.1} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Render the tree in a `du`-style flat sorted list.
/// Each line: `{size}\t{path}`
pub fn render_flat(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.flat_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());
    let mut out = String::new();
    for (path, size) in entries.iter().take(limit) {
        let _ = writeln!(out, "{:>10}\t{}", human_size(*size), path.display());
    }
    out
}

/// Render a directory-only flat sorted list.
///
/// Each line: `{size}\t{path}` — only directory entries are included.
/// Files are never emitted.  Combines naturally with `--min-size`, `--top N`,
/// and `--sort`.
pub fn render_dirs_flat(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.dirs_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());
    let mut out = String::new();
    for (path, size) in entries.iter().take(limit) {
        let _ = writeln!(out, "{:>10}\t{}", human_size(*size), path.display());
    }
    out
}

/// Render a tree view, analogous to `tree --du`.
/// Children at each level are sorted by `sort` order (Size = largest first).
pub fn render_tree(node: &DirNode, max_depth: Option<usize>, sort: SortOrder) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:>10}  {}",
        human_size(node.total_size),
        node.path.display()
    );
    let mut prefix = String::new();
    render_tree_children(&node.children, &mut prefix, max_depth, 1, sort, &mut out);
    out
}

fn entry_size(e: &EntryNode) -> u64 {
    match e {
        EntryNode::File(f) => f.size,
        EntryNode::Dir(d) => d.total_size,
    }
}

fn entry_name<'a>(e: &'a EntryNode) -> &'a std::path::Path {
    match e {
        EntryNode::File(f) => f.path.as_path(),
        EntryNode::Dir(d) => d.path.as_path(),
    }
}

fn render_tree_children(
    children: &[EntryNode],
    prefix: &mut String,
    max_depth: Option<usize>,
    depth: usize,
    sort: SortOrder,
    out: &mut String,
) {
    let mut sorted: Vec<&EntryNode> = children.iter().collect();
    match sort {
        SortOrder::Size => sorted.sort_unstable_by(|a, b| entry_size(b).cmp(&entry_size(a))),
        SortOrder::Name => sorted.sort_unstable_by(|a, b| entry_name(a).cmp(entry_name(b))),
    }
    let count = sorted.len();
    for (i, child) in sorted.iter().enumerate() {
        let is_last = i == count - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        match child {
            EntryNode::File(f) => {
                let _ = writeln!(
                    out,
                    "{}{}{:>10}  {}",
                    prefix,
                    connector,
                    human_size(f.size),
                    f.path.file_name().unwrap_or(f.path.as_os_str()).to_string_lossy()
                );
            }
            EntryNode::Dir(d) => {
                let _ = writeln!(
                    out,
                    "{}{}{:>10}  {}/",
                    prefix,
                    connector,
                    human_size(d.total_size),
                    d.path.file_name().unwrap_or(d.path.as_os_str()).to_string_lossy()
                );
                if max_depth.map(|m| depth < m).unwrap_or(true) {
                    let old_len = prefix.len();
                    prefix.push_str(child_prefix);
                    render_tree_children(
                        &d.children,
                        prefix,
                        max_depth,
                        depth + 1,
                        sort,
                        out,
                    );
                    prefix.truncate(old_len);
                }
            }
        }
    }
}

/// Render an extension breakdown: sorted list of (ext, total_bytes, file_count).
pub fn render_by_extension(node: &DirNode, sort: SortOrder) -> String {
    let rows = node.by_extension_sorted(sort);
    let mut out = String::new();
    for (ext, size, count) in &rows {
        let label = if ext.is_empty() {
            "(no ext)".to_string()
        } else {
            format!(".{}", ext)
        };
        let _ = writeln!(
            out,
            "{:>10}  {:>8} files  {}",
            human_size(*size),
            count,
            label
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{EntryNode, FileEntry};
    use std::path::PathBuf;

    fn make_five_file_tree() -> DirNode {
        let mut root = DirNode::new(PathBuf::from("/root"));
        for i in 0u64..5 {
            root.children.push(EntryNode::File(FileEntry {
                path: PathBuf::from(format!("/root/file{}.bin", i)),
                size: (i + 1) * 1024,
            }));
        }
        root.total_size = (1 + 2 + 3 + 4 + 5) * 1024;
        root.file_count = 5;
        root
    }

    /// Tree with two subdirs and some root-level files — used by render_dirs_flat tests.
    ///
    /// Structure:
    /// ```
    ///   /root/
    ///     alpha_dir/   total_size = 4096
    ///       file.bin   4096
    ///     beta_dir/    total_size = 1024
    ///       file.bin   1024
    ///     root.bin     512
    /// ```
    fn make_mixed_tree() -> DirNode {
        let mut alpha = DirNode::new(PathBuf::from("/root/alpha_dir"));
        alpha.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/alpha_dir/file.bin"),
            size: 4096,
        })];
        alpha.total_size = 4096;
        alpha.file_count = 1;

        let mut beta = DirNode::new(PathBuf::from("/root/beta_dir"));
        beta.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/beta_dir/file.bin"),
            size: 1024,
        })];
        beta.total_size = 1024;
        beta.file_count = 1;

        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::Dir(Box::new(alpha)),
            EntryNode::Dir(Box::new(beta)),
            EntryNode::File(FileEntry {
                path: PathBuf::from("/root/root.bin"),
                size: 512,
            }),
        ];
        root.total_size = 4096 + 1024 + 512;
        root.file_count = 3;
        root
    }

    #[test]
    fn human_size_boundaries() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human_size(1024u64 * 1024 * 1024 * 1024), "1.0 TiB");
    }

    #[test]
    fn human_size_fractional() {
        // 1.5 MiB
        assert_eq!(human_size(1024 * 1024 + 512 * 1024), "1.5 MiB");
    }

    #[test]
    fn render_flat_top_n_limits_output_to_n_lines() {
        let tree = make_five_file_tree();
        let output = render_flat(&tree, Some(2), SortOrder::Size, None);
        let line_count = output.lines().count();
        assert_eq!(
            line_count, 2,
            "render_flat with top_n=Some(2) must produce exactly 2 lines, got {}",
            line_count
        );
    }

    #[test]
    fn render_flat_top_n_zero_produces_empty_output() {
        let tree = make_five_file_tree();
        let output = render_flat(&tree, Some(0), SortOrder::Size, None);
        assert!(
            output.is_empty(),
            "render_flat with top_n=Some(0) must produce no output"
        );
    }

    #[test]
    fn render_flat_none_top_n_includes_all_entries() {
        let tree = make_five_file_tree();
        let output = render_flat(&tree, None, SortOrder::Size, None);
        assert_eq!(
            output.lines().count(), 5,
            "render_flat with top_n=None must include all 5 entries"
        );
    }

    #[test]
    fn render_flat_name_sort_is_alphabetical() {
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::File(FileEntry { path: PathBuf::from("/root/c.bin"), size: 300 }),
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.bin"), size: 100 }),
            EntryNode::File(FileEntry { path: PathBuf::from("/root/b.bin"), size: 200 }),
        ];
        root.total_size = 600;
        root.file_count = 3;

        let output = render_flat(&root, None, SortOrder::Name, None);
        let paths: Vec<&str> = output
            .lines()
            .filter_map(|l| l.split('\t').nth(1))
            .collect();
        assert_eq!(paths.len(), 3, "must have 3 paths");
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "render_flat with SortOrder::Name must be alphabetical");
    }

    #[test]
    fn render_flat_size_sort_is_largest_first() {
        let tree = make_five_file_tree();
        let output = render_flat(&tree, None, SortOrder::Size, None);
        // The tree has files with sizes 5KiB, 4KiB, 3KiB, 2KiB, 1KiB.
        // Lines are "{size}\t{path}" — verify descending size by checking the
        // size column (first token before the tab).
        let sizes_in_output: Vec<&str> = output
            .lines()
            .filter_map(|l| l.split('\t').next().map(str::trim))
            .collect();
        // "5.0 KiB" > "4.0 KiB" > ... lexicographically and numerically.
        let mut prev_path = None::<&str>;
        for line in output.lines() {
            let path = line.split('\t').nth(1).unwrap_or("");
            let idx: u64 = path
                .trim_start_matches("/root/file")
                .trim_end_matches(".bin")
                .parse()
                .unwrap_or(99);
            if let Some(p) = prev_path {
                let prev_idx: u64 = p
                    .trim_start_matches("/root/file")
                    .trim_end_matches(".bin")
                    .parse()
                    .unwrap_or(0);
                assert!(
                    prev_idx > idx,
                    "render_flat SortOrder::Size must be largest-first: {} then {}",
                    p, path
                );
            }
            prev_path = Some(path);
        }
        let _ = sizes_in_output; // used above indirectly
    }

    // ── render_dirs_flat tests ─────────────────────────────────────────────

    #[test]
    fn render_dirs_flat_shows_only_directories() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, None, SortOrder::Size, None);
        // root.bin must NOT appear; alpha_dir and beta_dir must appear
        assert!(
            !output.contains("root.bin"),
            "render_dirs_flat must not include file entries"
        );
        assert!(output.contains("alpha_dir"), "must include alpha_dir");
        assert!(output.contains("beta_dir"),  "must include beta_dir");
    }

    #[test]
    fn render_dirs_flat_line_count_matches_dir_count() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, None, SortOrder::Size, None);
        assert_eq!(
            output.lines().count(), 2,
            "mixed tree has exactly 2 subdirs — must produce exactly 2 lines"
        );
    }

    #[test]
    fn render_dirs_flat_top_n_limits_to_n_lines() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, Some(1), SortOrder::Size, None);
        assert_eq!(
            output.lines().count(), 1,
            "top_n=1 must produce exactly 1 line"
        );
        // The single line must be alpha_dir (largest at 4 KiB)
        assert!(
            output.contains("alpha_dir"),
            "top_n=1 must show the largest directory (alpha_dir)"
        );
    }

    #[test]
    fn render_dirs_flat_top_n_zero_is_empty() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, Some(0), SortOrder::Size, None);
        assert!(output.is_empty(), "top_n=0 must produce no output");
    }

    #[test]
    fn render_dirs_flat_min_size_filters_small_dirs() {
        let tree = make_mixed_tree();
        // alpha_dir = 4096 B, beta_dir = 1024 B; min_size=2000 keeps only alpha_dir
        let output = render_dirs_flat(&tree, None, SortOrder::Size, Some(2000));
        assert_eq!(output.lines().count(), 1, "only alpha_dir survives min_size=2000");
        assert!(output.contains("alpha_dir"));
        assert!(!output.contains("beta_dir"));
    }

    #[test]
    fn render_dirs_flat_name_sort_is_alphabetical() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, None, SortOrder::Name, None);
        let paths: Vec<&str> = output
            .lines()
            .filter_map(|l| l.split('\t').nth(1))
            .collect();
        assert_eq!(paths.len(), 2);
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "render_dirs_flat with SortOrder::Name must be alphabetical");
    }

    #[test]
    fn render_dirs_flat_no_subdirs_produces_empty_output() {
        // make_five_file_tree() has no Dir children — result must be empty
        let tree = make_five_file_tree();
        let output = render_dirs_flat(&tree, None, SortOrder::Size, None);
        assert!(
            output.is_empty(),
            "a tree with no subdirectories must produce empty render_dirs_flat output"
        );
    }

    #[test]
    fn render_dirs_flat_lines_are_tab_separated() {
        let tree = make_mixed_tree();
        let output = render_dirs_flat(&tree, None, SortOrder::Size, None);
        for line in output.lines() {
            assert!(
                line.contains('\t'),
                "each render_dirs_flat line must be tab-separated, got: {:?}",
                line
            );
        }
    }

    // ── render_tree tests ──────────────────────────────────────────────────

    #[test]
    fn render_tree_first_line_is_root_with_total_size() {
        let tree = make_mixed_tree(); // total = 4096+1024+512 = 5632 B
        let output = render_tree(&tree, None, SortOrder::Size);
        let first_line = output.lines().next().expect("output must have at least one line");
        assert!(first_line.contains("/root"), "first line must contain root path");
        // human_size(5632) = "5.5 KiB"
        assert!(
            first_line.contains("5.5"),
            "first line must contain total size, got: {:?}",
            first_line
        );
    }

    #[test]
    fn render_tree_no_children_shows_just_root_line() {
        let mut root = DirNode::new(PathBuf::from("/empty"));
        root.total_size = 0;
        root.file_count = 0;
        let output = render_tree(&root, None, SortOrder::Size);
        assert_eq!(
            output.lines().count(), 1,
            "root with no children must produce exactly 1 line"
        );
        assert!(output.contains("/empty"), "output must contain root path");
    }

    #[test]
    fn render_tree_contains_all_children_when_no_depth_limit() {
        let tree = make_mixed_tree();
        let output = render_tree(&tree, None, SortOrder::Size);
        assert!(output.contains("alpha_dir"), "output must contain alpha_dir");
        assert!(output.contains("beta_dir"),  "output must contain beta_dir");
        assert!(output.contains("root.bin"),  "output must contain root-level file");
    }

    #[test]
    fn render_tree_sort_name_lists_subtrees_alphabetically() {
        let tree = make_mixed_tree();
        let output = render_tree(&tree, None, SortOrder::Name);
        let alpha_pos = output.find("alpha_dir").expect("alpha_dir must be in output");
        let beta_pos  = output.find("beta_dir").expect("beta_dir must be in output");
        assert!(
            alpha_pos < beta_pos,
            "SortOrder::Name must place alpha_dir before beta_dir"
        );
    }

    #[test]
    fn render_tree_max_depth_one_collapses_subdirs_to_single_lines() {
        let tree = make_mixed_tree();
        let output = render_tree(&tree, Some(1), SortOrder::Size);
        // alpha_dir and beta_dir appear but their internal file.bin must NOT be shown.
        assert!(output.contains("alpha_dir"), "alpha_dir must be shown at depth=1");
        assert!(output.contains("beta_dir"),  "beta_dir must be shown at depth=1");
        // Count occurrences of "file.bin" — only root.bin is at root level; the
        // internal file.bin entries inside alpha_dir/beta_dir must not be rendered.
        let file_bin_count = output.lines().filter(|l| l.contains("file.bin")).count();
        assert_eq!(
            file_bin_count, 0,
            "max_depth=1 must not expand files inside subdirs, got {} such lines",
            file_bin_count
        );
    }
}
