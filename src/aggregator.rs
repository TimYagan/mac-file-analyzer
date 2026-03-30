/// Aggregated size data for a single directory node in the result tree.
use std::collections::HashMap;
use std::path::PathBuf;

/// Whether a size value is measured as logical bytes or disk blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    /// `st_blocks * 512` — actual storage used on disk (default, correct).
    DiskUsage,
    /// `st_size` — logical file size (what `ls -l` shows).
    Apparent,
}

/// Sort order applied to flat / by-extension output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    /// Largest entry first (default).
    #[default]
    Size,
    /// Alphabetical by path / extension name, ascending.
    Name,
}

/// A node in the directory tree produced by the aggregator.
#[derive(Debug)]
pub struct DirNode {
    pub path: PathBuf,
    pub children: Vec<EntryNode>,
    /// Total size of this directory + all descendants.
    pub total_size: u64,
    /// Number of unique inodes counted (after dedup).
    pub file_count: u64,
}

/// A leaf file entry, or a sub-directory reference.
#[derive(Debug)]
pub enum EntryNode {
    File(FileEntry),
    Dir(Box<DirNode>),
}

#[derive(Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
}

impl DirNode {
    pub fn new(path: PathBuf) -> Self {
        DirNode {
            path,
            children: Vec::new(),
            total_size: 0,
            file_count: 0,
        }
    }

    /// Recursively collect all (path, size) pairs, sorted by size descending.
    pub fn flat_sorted(&self) -> Vec<(&std::path::Path, u64)> {
        self.flat_sorted_with(SortOrder::Size, None)
    }

    /// Like `flat_sorted`, with configurable sort order and optional minimum size filter.
    ///
    /// The filter discards any entry whose size is strictly below `min_size`.
    pub fn flat_sorted_with(
        &self,
        sort: SortOrder,
        min_size: Option<u64>,
    ) -> Vec<(&std::path::Path, u64)> {
        let mut out = Vec::with_capacity(self.file_count as usize);
        self.collect_flat(&mut out);
        if let Some(min) = min_size {
            out.retain(|(_, s)| *s >= min);
        }
        match sort {
            SortOrder::Size => out.sort_unstable_by(|a, b| b.1.cmp(&a.1)),
            SortOrder::Name => out.sort_unstable_by(|a, b| a.0.cmp(b.0)),
        }
        out
    }

    fn collect_flat<'a>(&'a self, out: &mut Vec<(&'a std::path::Path, u64)>) {
        for child in &self.children {
            match child {
                EntryNode::Dir(d) => d.collect_flat(out),
                EntryNode::File(f) => out.push((f.path.as_path(), f.size)),
            }
        }
    }

    /// Collect all descendant directories (not the root itself), sorted by the given order.
    ///
    /// Each entry is `(directory_path, total_size_bytes)`.
    /// The scan root is excluded; use `self.total_size` for the root's aggregate.
    pub fn dirs_sorted_with(
        &self,
        sort: SortOrder,
        min_size: Option<u64>,
    ) -> Vec<(&std::path::Path, u64)> {
        let mut out = Vec::new();
        self.collect_dirs(&mut out);
        if let Some(min) = min_size {
            out.retain(|(_, s)| *s >= min);
        }
        match sort {
            SortOrder::Size => out.sort_unstable_by(|a, b| b.1.cmp(&a.1)),
            SortOrder::Name => out.sort_unstable_by(|a, b| a.0.cmp(b.0)),
        }
        out
    }

    fn collect_dirs<'a>(&'a self, out: &mut Vec<(&'a std::path::Path, u64)>) {
        for child in &self.children {
            if let EntryNode::Dir(d) = child {
                out.push((d.path.as_path(), d.total_size));
                d.collect_dirs(out);
            }
        }
    }

    /// Collect file counts and sizes grouped by extension, sorted by the given order.
    ///
    /// Returns `(extension_str, total_bytes, file_count)` rows.
    /// Files with no extension use an empty `extension_str`.
    pub fn by_extension_sorted(&self, sort: SortOrder) -> Vec<(String, u64, u64)> {
        let mut map: HashMap<String, (u64, u64)> = HashMap::new();
        self.collect_by_ext_inner(&mut map);
        let mut rows: Vec<(String, u64, u64)> =
            map.into_iter().map(|(k, (s, c))| (k, s, c)).collect();
        match sort {
            SortOrder::Size => rows.sort_unstable_by(|a, b| b.1.cmp(&a.1)),
            SortOrder::Name => rows.sort_unstable_by(|a, b| a.0.cmp(&b.0)),
        }
        rows
    }

    fn collect_by_ext_inner(&self, map: &mut HashMap<String, (u64, u64)>) {
        for child in &self.children {
            match child {
                EntryNode::File(f) => {
                    let ext = f
                        .path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let entry = map.entry(ext).or_insert((0, 0));
                    entry.0 += f.size;
                    entry.1 += 1;
                }
                EntryNode::Dir(d) => d.collect_by_ext_inner(map),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree() -> DirNode {
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::File(FileEntry { path: PathBuf::from("/root/b.txt"), size: 300 }),
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.rs"),  size: 100 }),
            EntryNode::File(FileEntry { path: PathBuf::from("/root/c.txt"), size: 50  }),
        ];
        root.total_size = 450;
        root.file_count = 3;
        root
    }

    #[test]
    fn flat_sorted_with_size_order() {
        let tree = make_tree();
        let entries = tree.flat_sorted_with(SortOrder::Size, None);
        // files only: b.txt (300), a.rs (100), c.txt (50)
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].1, 300);
        assert_eq!(entries[1].1, 100);
        assert_eq!(entries[2].1,  50);
    }

    #[test]
    fn flat_sorted_with_min_size_filter() {
        let tree = make_tree();
        let entries = tree.flat_sorted_with(SortOrder::Size, Some(100));
        // files only: b.txt (300), a.rs (100) — c.txt (50) filtered out
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(_, s)| *s >= 100));
    }

    #[test]
    fn flat_sorted_with_name_order() {
        let tree = make_tree();
        let entries = tree.flat_sorted_with(SortOrder::Name, None);
        // Sorted lexicographically: /root, /root/a.rs, /root/b.txt, /root/c.txt
        let paths: Vec<String> =
            entries.iter().map(|(p, _)| p.display().to_string()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn by_extension_sorted_size() {
        let tree = make_tree();
        let rows = tree.by_extension_sorted(SortOrder::Size);
        // txt: 350 (300+50, 2 files), rs: 100 (1 file) — txt first by size
        assert_eq!(rows[0].0, "txt");
        assert_eq!(rows[0].1, 350);
        assert_eq!(rows[0].2, 2);
        assert_eq!(rows[1].0, "rs");
    }

    #[test]
    fn by_extension_sorted_name() {
        let tree = make_tree();
        let rows = tree.by_extension_sorted(SortOrder::Name);
        // Alphabetical: "rs" < "txt"
        assert_eq!(rows[0].0, "rs");
        assert_eq!(rows[1].0, "txt");
    }

    // ── dirs_sorted_with tests ─────────────────────────────────────────────

    /// Build a tree that has actual directory children for dirs_sorted_with tests.
    ///
    /// Structure:
    /// ```
    ///   /root/
    ///     large_sub/   total_size = 6144  (inner/ 2048 + file.bin 4096)
    ///       inner/     total_size = 2048
    ///         file.bin   2048
    ///       file.bin   4096
    ///     small_sub/   total_size = 1024
    ///       file.bin   1024
    ///     root_file.bin  512
    /// ```
    fn make_dir_tree() -> DirNode {
        let mut inner = DirNode::new(PathBuf::from("/root/large_sub/inner"));
        inner.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/large_sub/inner/file.bin"),
            size: 2048,
        })];
        inner.total_size = 2048;
        inner.file_count = 1;

        let mut large_sub = DirNode::new(PathBuf::from("/root/large_sub"));
        large_sub.children = vec![
            EntryNode::Dir(Box::new(inner)),
            EntryNode::File(FileEntry {
                path: PathBuf::from("/root/large_sub/file.bin"),
                size: 4096,
            }),
        ];
        large_sub.total_size = 6144;
        large_sub.file_count = 2;

        let mut small_sub = DirNode::new(PathBuf::from("/root/small_sub"));
        small_sub.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/small_sub/file.bin"),
            size: 1024,
        })];
        small_sub.total_size = 1024;
        small_sub.file_count = 1;

        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::Dir(Box::new(large_sub)),
            EntryNode::Dir(Box::new(small_sub)),
            EntryNode::File(FileEntry {
                path: PathBuf::from("/root/root_file.bin"),
                size: 512,
            }),
        ];
        root.total_size = 6144 + 1024 + 512;
        root.file_count = 4;
        root
    }

    #[test]
    fn dirs_sorted_with_size_order() {
        let tree = make_dir_tree();
        let dirs = tree.dirs_sorted_with(SortOrder::Size, None);
        // 3 dirs: large_sub (6144), inner (2048), small_sub (1024) — root excluded
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0].1, 6144, "largest first: large_sub");
        assert_eq!(dirs[1].1, 2048, "second: inner");
        assert_eq!(dirs[2].1, 1024, "third: small_sub");
    }

    #[test]
    fn dirs_sorted_with_name_order() {
        let tree = make_dir_tree();
        let dirs = tree.dirs_sorted_with(SortOrder::Name, None);
        assert_eq!(dirs.len(), 3);
        let paths: Vec<String> =
            dirs.iter().map(|(p, _)| p.display().to_string()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "name sort must be alphabetical");
    }

    #[test]
    fn dirs_sorted_with_min_size_filter() {
        let tree = make_dir_tree();
        // min_size=2048: large_sub (6144) and inner (2048) survive; small_sub (1024) excluded
        let dirs = tree.dirs_sorted_with(SortOrder::Size, Some(2048));
        assert_eq!(dirs.len(), 2);
        assert!(dirs.iter().all(|(_, s)| *s >= 2048));
    }

    #[test]
    fn dirs_sorted_with_root_not_included() {
        let tree = make_dir_tree();
        let dirs = tree.dirs_sorted_with(SortOrder::Size, None);
        let root_path = PathBuf::from("/root");
        assert!(
            !dirs.iter().any(|(p, _)| *p == root_path.as_path()),
            "the scan root must not appear in dirs_sorted_with output"
        );
    }

    #[test]
    fn dirs_sorted_with_no_subdirs_returns_empty() {
        // make_tree() has only file children — no Dir entries
        let tree = make_tree();
        assert!(
            tree.dirs_sorted_with(SortOrder::Size, None).is_empty(),
            "a flat-file-only tree must return an empty dir list"
        );
    }

    #[test]
    fn dirs_sorted_with_nested_dirs_all_collected() {
        let tree = make_dir_tree();
        // large_sub + inner (nested inside large_sub) + small_sub = 3 dirs
        assert_eq!(tree.dirs_sorted_with(SortOrder::Size, None).len(), 3);
    }

    #[test]
    fn dirs_sorted_with_min_size_zero_includes_all() {
        let tree = make_dir_tree();
        // min_size=0 must not filter anything out
        assert_eq!(tree.dirs_sorted_with(SortOrder::Size, Some(0)).len(), 3);
    }
}
