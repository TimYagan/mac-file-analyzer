/// CSV output: flat sorted entries, or per-extension breakdown.
use crate::aggregator::{DirNode, SortOrder};
use crate::formatter::human_size;

/// Escape a path string for safe inclusion in a CSV field.
///
/// - Doubles embedded double-quote characters (RFC 4180).
/// - Replaces literal newlines (macOS filenames may contain them).
/// - Prepends a tab to values starting with `=`, `+`, `-`, or `@` to prevent
///   spreadsheet formula injection when the CSV is opened in Excel / Sheets.
fn csv_escape_path(raw: &str) -> String {
    let escaped = raw
        .replace('"', "\"\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    // Formula-injection guard: a leading tab is invisible in most spreadsheets
    // and prevents `=CMD()`, `+CMD()`, `-CMD()`, `@CMD()` from being evaluated.
    if escaped.starts_with(['=', '+', '-', '@']) {
        format!("\t{}", escaped)
    } else {
        escaped
    }
}

/// Render a CSV table with columns `path,bytes,human_size`.
pub fn render_csv(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.flat_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());
    let mut out = String::from("path,bytes,human_size\n");
    for (path, size) in entries.iter().take(limit) {
        let escaped = csv_escape_path(&path.to_string_lossy());
        out.push('"');
        out.push_str(&escaped);
        out.push_str("\",");
        out.push_str(&size.to_string());
        out.push(',');
        out.push_str(&human_size(*size));
        out.push('\n');
    }
    out
}

/// Render a CSV table of directories with columns `path,bytes,human_size`.
///
/// The scan root is excluded.  Compatible with `--min-size`, `--top N`, and
/// `--sort`.  Uses RFC 4180 double-quote escaping for paths.
pub fn render_csv_dirs(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.dirs_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());
    let mut out = String::from("path,bytes,human_size\n");
    for (path, size) in entries.iter().take(limit) {
        let escaped = csv_escape_path(&path.to_string_lossy());
        out.push('"');
        out.push_str(&escaped);
        out.push_str("\",");
        out.push_str(&size.to_string());
        out.push(',');
        out.push_str(&human_size(*size));
        out.push('\n');
    }
    out
}

/// Render a CSV table with columns `extension,bytes,human_size,file_count`.
pub fn render_csv_by_ext(node: &DirNode, sort: SortOrder) -> String {
    let mut out = String::from("extension,bytes,human_size,file_count\n");
    for (ext, bytes, count) in node.by_extension_sorted(sort) {
        let escaped = csv_escape_path(&ext);
        out.push('"');
        out.push_str(&escaped);
        out.push_str("\",");
        out.push_str(&bytes.to_string());
        out.push(',');
        out.push_str(&human_size(bytes));
        out.push(',');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{EntryNode, FileEntry};
    use std::path::PathBuf;

    fn make_tree() -> DirNode {
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.rs"), size: 1024 }),
        ];
        root.total_size = 1024;
        root.file_count = 1;
        root
    }

    #[test]
    fn csv_by_ext_has_header_and_row() {
        let tree = make_tree();
        let out = render_csv_by_ext(&tree, SortOrder::Size);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "extension,bytes,human_size,file_count");
        assert!(lines[1].contains("rs"));
        assert!(lines[1].contains("1024"));
        assert!(lines[1].contains("1"));  // file_count
    }

    #[test]
    fn csv_flat_respects_min_size() {
        let tree = make_tree();
        let out = render_csv(&tree, None, SortOrder::Size, Some(2000));
        let lines: Vec<&str> = out.lines().collect();
        // Only header; both root (1024) and file (1024) are below 2000
        assert_eq!(lines.len(), 1);
    }

    fn make_dir_tree() -> DirNode {
        let mut sub = DirNode::new(PathBuf::from("/root/sub"));
        sub.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/sub/file.bin"),
            size: 2048,
        })];
        sub.total_size = 2048;
        sub.file_count = 1;

        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::Dir(Box::new(sub)),
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.rs"), size: 1024 }),
        ];
        root.total_size = 3072;
        root.file_count = 2;
        root
    }

    #[test]
    fn csv_dirs_has_correct_header() {
        let tree = make_dir_tree();
        let out = render_csv_dirs(&tree, None, SortOrder::Size, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "path,bytes,human_size", "CSV dirs header must match");
    }

    #[test]
    fn csv_dirs_has_one_data_row_for_one_subdir() {
        let tree = make_dir_tree();
        let out = render_csv_dirs(&tree, None, SortOrder::Size, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 data row");
        assert!(lines[1].contains("sub"), "data row must reference the subdir");
        assert!(lines[1].contains("2048"), "data row must contain byte count");
    }

    #[test]
    fn csv_dirs_min_size_filters_out_all() {
        let tree = make_dir_tree();
        // min_size=3000 filters out sub (2048)
        let out = render_csv_dirs(&tree, None, SortOrder::Size, Some(3000));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "only header when all dirs are below min_size");
    }

    #[test]
    fn csv_dirs_empty_when_no_subdirs() {
        let tree = make_tree(); // only file children
        let out = render_csv_dirs(&tree, None, SortOrder::Size, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "only header when there are no subdirectories");
    }

    // ── newline-in-path escaping tests ────────────────────────────────────

    #[test]
    fn csv_flat_newline_in_path_produces_single_row() {
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/bad\nname.txt"),
            size: 512,
        })];
        root.total_size = 512;
        root.file_count = 1;
        let out = render_csv(&root, None, SortOrder::Size, None);
        // The embedded newline must be escaped — output must still have exactly 2 lines.
        assert_eq!(out.lines().count(), 2, "newline in path must not create extra CSV rows");
        assert!(out.contains("\\n"), "escaped newline must appear in output");
    }

    #[test]
    fn csv_dirs_newline_in_path_produces_single_row() {
        let mut sub = DirNode::new(PathBuf::from("/root/bad\ndir"));
        sub.total_size = 1024;
        sub.file_count = 0;
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![EntryNode::Dir(Box::new(sub))];
        root.total_size = 1024;
        root.file_count = 0;
        let out = render_csv_dirs(&root, None, SortOrder::Size, None);
        assert_eq!(out.lines().count(), 2, "newline in dir path must not create extra CSV rows");
        assert!(out.contains("\\n"), "escaped newline must appear in output");
    }

    #[test]
    fn csv_by_ext_newline_in_ext_produces_single_row() {
        let mut root = DirNode::new(PathBuf::from("/root"));
        // File whose extension contains a newline (bizarre but macOS allows it).
        root.children = vec![EntryNode::File(FileEntry {
            path: PathBuf::from("/root/file.bad\next"),
            size: 256,
        })];
        root.total_size = 256;
        root.file_count = 1;
        let out = render_csv_by_ext(&root, SortOrder::Size);
        assert_eq!(out.lines().count(), 2, "newline in extension must not create extra CSV rows");
        assert!(out.contains("\\n"), "escaped newline must appear in output");
    }
}
