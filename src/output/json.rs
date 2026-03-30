/// JSON output: flat sorted entries, or per-extension breakdown.
use crate::aggregator::{DirNode, SortOrder};
use crate::formatter::human_size;

/// Render a JSON array of `{ path, bytes, human }` entries.
pub fn render_json(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.flat_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());

    let rows: Vec<serde_json::Value> = entries
        .iter()
        .take(limit)
        .map(|(path, size)| {
            serde_json::json!({
                "path": path.to_string_lossy(),
                "bytes": size,
                "human": human_size(*size),
            })
        })
        .collect();

    serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string())
}

/// Render a JSON array of `{ path, bytes, human }` entries for directories only.
///
/// The scan root is excluded.  Entries are sorted and filtered exactly like
/// the file-level `render_json`, but only directory aggregates are emitted.
pub fn render_json_dirs(
    node: &DirNode,
    top_n: Option<usize>,
    sort: SortOrder,
    min_size: Option<u64>,
) -> String {
    let entries = node.dirs_sorted_with(sort, min_size);
    let limit = top_n.unwrap_or(entries.len());

    let rows: Vec<serde_json::Value> = entries
        .iter()
        .take(limit)
        .map(|(path, size)| {
            serde_json::json!({
                "path": path.to_string_lossy(),
                "bytes": size,
                "human": human_size(*size),
            })
        })
        .collect();

    serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string())
}

/// Render a JSON array of `{ extension, bytes, human, file_count }` rows.
pub fn render_json_by_ext(node: &DirNode, sort: SortOrder) -> String {
    let rows: Vec<serde_json::Value> = node
        .by_extension_sorted(sort)
        .into_iter()
        .map(|(ext, bytes, count)| {
            serde_json::json!({
                "extension": ext,
                "bytes": bytes,
                "human": human_size(bytes),
                "file_count": count,
            })
        })
        .collect();

    serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{EntryNode, FileEntry};
    use std::path::PathBuf;

    fn make_tree() -> DirNode {
        let mut root = DirNode::new(PathBuf::from("/root"));
        root.children = vec![
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.rs"), size: 100 }),
        ];
        root.total_size = 100;
        root.file_count = 1;
        root
    }

    #[test]
    fn json_by_ext_is_valid_json() {
        let tree = make_tree();
        let out = render_json_by_ext(&tree, SortOrder::Size);
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("must be valid JSON");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["extension"], "rs");
        assert_eq!(arr[0]["bytes"], 100u64);
        assert_eq!(arr[0]["file_count"], 1u64);
    }

    #[test]
    fn json_flat_respects_min_size() {
        let tree = make_tree();
        // min_size larger than any entry — only the root dir (total 100) qualifies
        let out = render_json(&tree, None, SortOrder::Size, Some(100));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().unwrap();
        // root dir (100) passes; file (100) passes; nothing filtered below 100
        assert!(arr.iter().all(|e| e["bytes"].as_u64().unwrap() >= 100));
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
            EntryNode::File(FileEntry { path: PathBuf::from("/root/a.rs"), size: 100 }),
        ];
        root.total_size = 2148;
        root.file_count = 2;
        root
    }

    #[test]
    fn json_dirs_has_correct_schema() {
        let tree = make_dir_tree();
        let out = render_json_dirs(&tree, None, SortOrder::Size, None);
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("render_json_dirs must produce valid JSON");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1, "exactly one subdirectory");
        assert!(arr[0].get("path").is_some(),  "must have 'path'");
        assert!(arr[0].get("bytes").is_some(), "must have 'bytes'");
        assert!(arr[0].get("human").is_some(), "must have 'human'");
        assert!(arr[0].get("file_count").is_none(), "must NOT have 'file_count'");
        assert_eq!(arr[0]["bytes"].as_u64().unwrap(), 2048);
    }

    #[test]
    fn json_dirs_min_size_filters_correctly() {
        let tree = make_dir_tree();
        // min_size larger than sub (2048) — nothing survives at 3000
        let out = render_json_dirs(&tree, None, SortOrder::Size, Some(3000));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.as_array().unwrap().is_empty());
    }

    #[test]
    fn json_dirs_empty_when_no_subdirs() {
        let tree = make_tree(); // only file children
        let out = render_json_dirs(&tree, None, SortOrder::Size, None);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.as_array().unwrap().is_empty());
    }
}
