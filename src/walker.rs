/// Directory walker: single-threaded Phase 1 baseline and parallel Phase 2.
///
/// Correctness guarantees:
/// - Uses `lstat` (never follows symlinks unless --follow-symlinks).
/// - Deduplicates hard-linked inodes via an `InodeSet`.
/// - Skips special files (devices, pipes, sockets) via `is_regular` check.
/// - Skips inaccessible entries with a warning to stderr.
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::aggregator::{DirNode, EntryNode, FileEntry, SizeMode};
use crate::stat::{getattrlist_stat, lstat, stat_follow, InodeKey, InodeSet};

/// Options that control the walk behaviour.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub size_mode: SizeMode,
    /// Maximum directory depth (None = unlimited).
    pub max_depth: Option<usize>,
    /// Follow symlinks (cycle-safe via InodeSet).
    pub follow_symlinks: bool,
    /// If Some, only count files with this extension (lowercased).
    pub filter_ext: Option<String>,
    /// Include resource fork bytes in each file's size (Phase 3).
    pub include_rsrc: bool,
    /// Suppress non-critical warnings to stderr.
    pub quiet: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        WalkOptions {
            size_mode: SizeMode::DiskUsage,
            max_depth: None,
            follow_symlinks: false,
            filter_ext: None,
            include_rsrc: false,
            quiet: false,
        }
    }
}

/// Walk `root` recursively and return an aggregated `DirNode` tree.
///
/// `progress` is an optional callback invoked for each file visited; useful
/// for driving a progress bar without coupling the walker to `indicatif`.
pub fn walk(
    root: &Path,
    opts: &WalkOptions,
    progress: &mut dyn FnMut(&Path),
) -> io::Result<DirNode> {
    let mut seen: InodeSet = HashSet::new();
    walk_dir(root, opts, 0, &mut seen, progress)
}

// ─── internals ──────────────────────────────────────────────────────────────

fn walk_dir(
    path: &Path,
    opts: &WalkOptions,
    depth: usize,
    seen: &mut InodeSet,
    progress: &mut dyn FnMut(&Path),
) -> io::Result<DirNode> {
    let mut node = DirNode::new(path.to_path_buf());

    let entries = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            // Permission denied or other transient error — report and skip.
            if !opts.quiet {
                eprintln!("warning: cannot read directory {}: {}", path.display(), e);
            }
            return Ok(node);
        }
    };

    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: directory entry error in {}: {}", path.display(), e);
                }
                continue;
            }
        };

        let child_path: PathBuf = entry.path();
        process_entry(&child_path, opts, depth, seen, progress, &mut node);
    }

    Ok(node)
}

fn process_entry(
    child_path: &Path,
    opts: &WalkOptions,
    depth: usize,
    seen: &mut InodeSet,
    progress: &mut dyn FnMut(&Path),
    parent: &mut DirNode,
) {
    // Always lstat first — never blindly follow.
    let st = match lstat(child_path) {
        Ok(s) => s,
        Err(e) => {
            if !opts.quiet {
                eprintln!("warning: cannot stat {}: {}", child_path.display(), e);
            }
            return;
        }
    };

    if st.is_symlink {
        if opts.follow_symlinks {
            // Follow the link, but only if its real target hasn't been seen.
            let real_st = match stat_follow(child_path) {
                Ok(s) => s,
                Err(e) => {
                    if !opts.quiet {
                        eprintln!(
                            "warning: cannot follow symlink {}: {}",
                            child_path.display(),
                            e
                        );
                    }
                    return;
                }
            };
            if !insert_seen(seen, real_st.inode) {
                return; // cycle or already-counted hard link
            }
            if real_st.is_dir {
                if let Some(max) = opts.max_depth {
                    if depth >= max {
                        return;
                    }
                }
                match walk_dir(child_path, opts, depth + 1, seen, progress) {
                    Ok(child_node) => {
                        parent.total_size += child_node.total_size;
                        parent.file_count += child_node.file_count;
                        parent.children.push(EntryNode::Dir(Box::new(child_node)));
                    }
                    Err(e) => {
                        if !opts.quiet {
                            eprintln!("warning: walk error at {}: {}", child_path.display(), e);
                        }
                    }
                }
            } else {
                account_file(child_path, real_st.apparent_size, real_st.disk_usage, opts, progress, parent);
            }
        }
        // When not following symlinks, silently skip the symlink itself
        // (it occupies negligible space and its target may be elsewhere).
        return;
    }

    if st.is_dir {
        if let Some(max) = opts.max_depth {
            if depth >= max {
                return;
            }
        }
        match walk_dir(child_path, opts, depth + 1, seen, progress) {
            Ok(child_node) => {
                parent.total_size += child_node.total_size;
                parent.file_count += child_node.file_count;
                parent.children.push(EntryNode::Dir(Box::new(child_node)));
            }
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: walk error at {}: {}", child_path.display(), e);
                }
            }
        }
        return;
    }

    // Skip special files: devices, pipes, sockets, etc.
    if !st.is_regular {
        return;
    }

    // Inode deduplication — skip already-seen hard links.
    if !insert_seen(seen, st.inode) {
        return;
    }

    // Extension filter.
    if let Some(ref ext) = opts.filter_ext {
        let matches = child_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase() == *ext)
            .unwrap_or(false);
        if !matches {
            return;
        }
    }

    account_file(child_path, st.apparent_size, st.disk_usage, opts, progress, parent);
}

/// Returns `true` if this inode was newly inserted (not a duplicate).
#[inline(always)]
fn insert_seen(seen: &mut InodeSet, key: InodeKey) -> bool {
    seen.insert(key)
}

fn account_file(
    path: &Path,
    apparent: u64,
    disk: u64,
    opts: &WalkOptions,
    progress: &mut dyn FnMut(&Path),
    parent: &mut DirNode,
) {
    progress(path);
    let size = match opts.size_mode {
        SizeMode::DiskUsage => disk,
        SizeMode::Apparent => apparent,
    };
    parent.total_size += size;
    parent.file_count += 1;
    parent.children.push(EntryNode::File(FileEntry {
        path: path.to_path_buf(),
        size,
    }));
}

/// Returns true if `path`'s extension matches the filter in `opts` (or no filter is set).
#[inline]
fn ext_matches(path: &Path, opts: &WalkOptions) -> bool {
    match &opts.filter_ext {
        None => true,
        Some(ext) => path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase() == *ext)
            .unwrap_or(false),
    }
}

// ─── Phase 2: parallel walker ────────────────────────────────────────────────

/// Walk `root` recursively using rayon work-stealing (Phase 2).
///
/// Subdirectories at each level are dispatched in parallel across all CPU
/// cores.  Inode deduplication is protected by a single `Mutex`; contention
/// is low because the critical section is a single `HashSet::insert` call.
///
/// The `progress` callback is called from multiple threads — it must be
/// `Send + Sync`.  Use atomics or lock-free structures inside it.
pub fn walk_parallel<F>(root: &Path, opts: &WalkOptions, progress: &F) -> io::Result<DirNode>
where
    F: Fn(&Path) + Send + Sync,
{
    let seen = Arc::new(Mutex::new(InodeSet::new()));
    walk_dir_parallel(root, opts, 0, &seen, progress)
}

fn walk_dir_parallel<F>(
    path: &Path,
    opts: &WalkOptions,
    depth: usize,
    seen: &Arc<Mutex<InodeSet>>,
    progress: &F,
) -> io::Result<DirNode>
where
    F: Fn(&Path) + Send + Sync,
{
    let mut node = DirNode::new(path.to_path_buf());

    let entries = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            if !opts.quiet {
                eprintln!("warning: cannot read directory {}: {}", path.display(), e);
            }
            return Ok(node);
        }
    };

    // (path, apparent_bytes, disk_bytes) — collected before rayon dispatch.
    let mut to_account: Vec<(PathBuf, u64, u64)> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();

    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: directory entry error in {}: {}", path.display(), e);
                }
                continue;
            }
        };
        let child = entry.path();

        let st = match lstat(&child) {
            Ok(s) => s,
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: cannot stat {}: {}", child.display(), e);
                }
                continue;
            }
        };

        if st.is_symlink {
            if opts.follow_symlinks {
                let real_st = match stat_follow(&child) {
                    Ok(s) => s,
                    Err(e) => {
                        if !opts.quiet {
                            eprintln!("warning: cannot follow symlink {}: {}", child.display(), e);
                        }
                        continue;
                    }
                };
                if !seen.lock().unwrap_or_else(|p| p.into_inner()).insert(real_st.inode) {
                    continue; // cycle or already-counted
                }
                if real_st.is_dir && opts.max_depth.map_or(true, |max| depth < max) {
                    subdirs.push(child);
                } else if real_st.is_regular && ext_matches(&child, opts) {
                    to_account.push((child, real_st.apparent_size, real_st.disk_usage));
                }
            }
            continue;
        }

        if st.is_dir {
            if opts.max_depth.map_or(true, |max| depth < max) {
                subdirs.push(child);
            }
            continue;
        }

        if !st.is_regular {
            continue; // skip devices, pipes, sockets
        }

        // Inode dedup — brief critical section.
        if !seen.lock().unwrap_or_else(|p| p.into_inner()).insert(st.inode) {
            continue;
        }

        if !ext_matches(&child, opts) {
            continue;
        }

        to_account.push((child, st.apparent_size, st.disk_usage));
    }

    // Account files in this directory (sequential — no contention needed).
    for (file_path, apparent, disk) in to_account {
        progress(&file_path);
        let size = match opts.size_mode {
            SizeMode::DiskUsage => disk,
            SizeMode::Apparent => apparent,
        };
        node.total_size += size;
        node.file_count += 1;
        node.children.push(EntryNode::File(FileEntry {
            path: file_path,
            size,
        }));
    }

    // Recurse into subdirectories in parallel.
    let child_results: Vec<io::Result<DirNode>> = subdirs
        .into_par_iter()
        .map(|dir| walk_dir_parallel(&dir, opts, depth + 1, seen, progress))
        .collect();

    for result in child_results {
        match result {
            Ok(child) => {
                node.total_size += child.total_size;
                node.file_count += child.file_count;
                node.children.push(EntryNode::Dir(Box::new(child)));
            }
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: walk error: {}", e);
                }
            }
        }
    }

    Ok(node)
}

// ─── Phase 3: getattrlist parallel walker ────────────────────────────────────

/// Walk `root` using rayon + `getattrlist` (Phase 3).
///
/// Identical structure to `walk_parallel` but each file is stat'd via a
/// single `getattrlist(2)` call instead of `lstat(2)`.  On APFS/HFS+ this
/// returns all required attributes — type, inode, size, allocation, resource
/// fork length — in one kernel trip.  Falls back to `lstat` transparently on
/// network volumes that don't support `getattrlist`.
///
/// When `opts.include_rsrc` is `true`, the resource fork length is added to
/// each file's accounted size.
pub fn walk_parallel_getattrlist<F>(root: &Path, opts: &WalkOptions, progress: &F) -> io::Result<DirNode>
where
    F: Fn(&Path) + Send + Sync,
{
    let seen = Arc::new(Mutex::new(InodeSet::new()));
    walk_dir_getattrlist(root, opts, 0, &seen, progress)
}

fn walk_dir_getattrlist<F>(
    path: &Path,
    opts: &WalkOptions,
    depth: usize,
    seen: &Arc<Mutex<InodeSet>>,
    progress: &F,
) -> io::Result<DirNode>
where
    F: Fn(&Path) + Send + Sync,
{
    let mut node = DirNode::new(path.to_path_buf());

    let entries = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            if !opts.quiet {
                eprintln!("warning: cannot read directory {}: {}", path.display(), e);
            }
            return Ok(node);
        }
    };

    // (path, apparent_bytes, disk_bytes, rsrc_bytes)
    let mut to_account: Vec<(PathBuf, u64, u64, u64)> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();

    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: directory entry error in {}: {}", path.display(), e);
                }
                continue;
            }
        };
        let child = entry.path();

        let st = match getattrlist_stat(&child) {
            Ok(s) => s,
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: cannot stat {}: {}", child.display(), e);
                }
                continue;
            }
        };

        if st.is_symlink {
            if opts.follow_symlinks {
                let real_st = match stat_follow(&child) {
                    Ok(s) => s,
                    Err(e) => {
                        if !opts.quiet {
                            eprintln!("warning: cannot follow symlink {}: {}", child.display(), e);
                        }
                        continue;
                    }
                };
                if !seen.lock().unwrap_or_else(|p| p.into_inner()).insert(real_st.inode) {
                    continue;
                }
                if real_st.is_dir && opts.max_depth.map_or(true, |max| depth < max) {
                    subdirs.push(child);
                } else if real_st.is_regular && ext_matches(&child, opts) {
                    to_account.push((child, real_st.apparent_size, real_st.disk_usage, 0));
                }
            }
            continue;
        }

        if st.is_dir {
            if opts.max_depth.map_or(true, |max| depth < max) {
                subdirs.push(child);
            }
            continue;
        }

        if !st.is_regular {
            continue;
        }

        if !seen.lock().unwrap_or_else(|p| p.into_inner()).insert(st.inode) {
            continue;
        }

        if !ext_matches(&child, opts) {
            continue;
        }

        to_account.push((child, st.apparent_size, st.disk_usage, st.rsrc_size));
    }

    for (file_path, apparent, disk, rsrc) in to_account {
        progress(&file_path);
        let mut size = match opts.size_mode {
            SizeMode::DiskUsage => disk,
            SizeMode::Apparent => apparent,
        };
        if opts.include_rsrc {
            size += rsrc;
        }
        node.total_size += size;
        node.file_count += 1;
        node.children.push(EntryNode::File(FileEntry {
            path: file_path,
            size,
        }));
    }

    let child_results: Vec<io::Result<DirNode>> = subdirs
        .into_par_iter()
        .map(|dir| walk_dir_getattrlist(&dir, opts, depth + 1, seen, progress))
        .collect();

    for result in child_results {
        match result {
            Ok(child) => {
                node.total_size += child.total_size;
                node.file_count += child.file_count;
                node.children.push(EntryNode::Dir(Box::new(child)));
            }
            Err(e) => {
                if !opts.quiet {
                    eprintln!("warning: walk error: {}", e);
                }
            }
        }
    }

    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn no_progress(_: &Path) {}

    #[test]
    fn walk_empty_dir() {
        let dir = tempdir().unwrap();
        let node = walk(dir.path(), &WalkOptions::default(), &mut no_progress).unwrap();
        assert_eq!(node.total_size, 0);
        assert_eq!(node.file_count, 0);
    }

    #[test]
    fn walk_single_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), b"hello world").unwrap();
        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        assert_eq!(node.total_size, 11);
        assert_eq!(node.file_count, 1);
    }

    #[test]
    fn hardlinks_counted_once() {
        let dir = tempdir().unwrap();
        let orig = dir.path().join("orig.txt");
        let hard = dir.path().join("hard.txt");
        fs::write(&orig, b"data data data").unwrap();
        fs::hard_link(&orig, &hard).unwrap();

        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        // Only one of the two hard links should be counted.
        assert_eq!(node.file_count, 1);
        assert_eq!(node.total_size, 14);
    }

    #[test]
    fn symlinks_not_followed_by_default() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("big.txt");
        let link = dir.path().join("link_to_big.txt");
        // Write 1000 bytes into target.
        fs::write(&target, vec![b'x'; 1000]).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        // Only the real file should be counted, not the symlink.
        assert_eq!(node.file_count, 1);
        assert_eq!(node.total_size, 1000);
    }

    #[test]
    fn nested_dirs_aggregate() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), vec![b'a'; 100]).unwrap();
        fs::write(sub.join("b.txt"), vec![b'b'; 200]).unwrap();

        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        assert_eq!(node.total_size, 300);
        assert_eq!(node.file_count, 2);
    }

    #[test]
    fn max_depth_limits_walk() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        let subsub = sub.join("subsub");
        fs::create_dir_all(&subsub).unwrap();
        fs::write(dir.path().join("top.txt"), vec![b't'; 10]).unwrap();
        fs::write(sub.join("mid.txt"), vec![b'm'; 20]).unwrap();
        fs::write(subsub.join("deep.txt"), vec![b'd'; 30]).unwrap();

        // depth=1: top.txt + sub/mid.txt counted; sub/subsub/ NOT descended.
        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                max_depth: Some(1),
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        assert_eq!(node.total_size, 30); // 10 + 20
        assert_eq!(node.file_count, 2);
    }

    #[test]
    fn extension_filter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("keep.rs"), vec![b'r'; 50]).unwrap();
        fs::write(dir.path().join("skip.txt"), vec![b's'; 50]).unwrap();

        let node = walk(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                filter_ext: Some("rs".into()),
                ..Default::default()
            },
            &mut no_progress,
        )
        .unwrap();
        assert_eq!(node.file_count, 1);
        assert_eq!(node.total_size, 50);
    }

    // ── Phase 2: walk_parallel ───────────────────────────────────────────────

    #[test]
    fn parallel_walk_matches_sequential() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), vec![b'a'; 100]).unwrap();
        fs::write(sub.join("b.txt"), vec![b'b'; 200]).unwrap();

        let opts = WalkOptions {
            size_mode: SizeMode::Apparent,
            ..Default::default()
        };
        let seq = walk(dir.path(), &opts, &mut no_progress).unwrap();
        let par = walk_parallel(dir.path(), &opts, &no_progress).unwrap();

        assert_eq!(par.total_size, seq.total_size);
        assert_eq!(par.file_count, seq.file_count);
    }

    #[test]
    fn parallel_hardlinks_counted_once() {
        let dir = tempdir().unwrap();
        let orig = dir.path().join("orig.txt");
        let hard = dir.path().join("hard.txt");
        fs::write(&orig, b"data data data").unwrap();
        fs::hard_link(&orig, &hard).unwrap();

        let node = walk_parallel(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                ..Default::default()
            },
            &no_progress,
        )
        .unwrap();
        assert_eq!(node.file_count, 1);
        assert_eq!(node.total_size, 14);
    }

    #[test]
    fn parallel_max_depth() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        let subsub = sub.join("subsub");
        fs::create_dir_all(&subsub).unwrap();
        fs::write(dir.path().join("top.txt"), vec![b't'; 10]).unwrap();
        fs::write(sub.join("mid.txt"), vec![b'm'; 20]).unwrap();
        fs::write(subsub.join("deep.txt"), vec![b'd'; 30]).unwrap();

        let node = walk_parallel(
            dir.path(),
            &WalkOptions {
                size_mode: SizeMode::Apparent,
                max_depth: Some(1),
                ..Default::default()
            },
            &no_progress,
        )
        .unwrap();
        assert_eq!(node.total_size, 30); // 10 + 20 only
        assert_eq!(node.file_count, 2);
    }
}
