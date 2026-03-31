use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use mac_file_analyzer::aggregator::{SizeMode, SortOrder};
use mac_file_analyzer::{formatter, output, walker};
use walker::WalkOptions;

/// A fast, accurate file size analysis tool for macOS.
#[derive(Parser, Debug)]
#[command(name = "mfa", version, about, long_about = None)]
struct Cli {
    /// Path to analyse (default: current directory).
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Maximum directory depth to descend (unlimited if not set).
    #[arg(short = 'd', long)]
    depth: Option<usize>,

    /// Show top N largest entries in flat/json/csv modes.
    #[arg(short = 'n', long, default_value = "20")]
    top: usize,

    /// Output format.
    #[arg(short = 'f', long, value_enum, default_value = "tree")]
    format: OutputFormat,

    /// Use apparent (logical) file sizes instead of actual disk usage.
    #[arg(long)]
    apparent: bool,

    /// Follow symlinks (cycle-safe).
    #[arg(long)]
    follow_symlinks: bool,

    /// Only count files with this extension (e.g. "mp4").
    #[arg(short = 't', long)]
    r#type: Option<String>,

    /// Suppress the progress spinner.
    #[arg(long)]
    no_progress: bool,

    /// Show per-extension size breakdown instead of paths.
    #[arg(long)]
    by_ext: bool,

    /// Sort order: size (largest first, default) or name (alphabetical).
    #[arg(short = 's', long, value_enum, default_value = "size")]
    sort: CliSort,

    /// Only show entries at or above this size (e.g. 10MB, 1GiB, 500K).
    #[arg(long)]
    min_size: Option<String>,

    /// Print total + elapsed time at the end.
    #[arg(long)]
    stats: bool,

    /// Include resource fork bytes in each file's size (macOS only).
    #[arg(long)]
    include_rsrc: bool,

    /// Suppress non-critical warnings to stderr.
    #[arg(long)]
    quiet: bool,

    /// Show directories only, each ranked by its total content size.
    ///
    /// Lists every subdirectory (not the scan root itself) with its rolled-up
    /// size.  Compatible with `--min-size`, `--top N`, `--sort`, and the
    /// `flat`, `json`, and `csv` output formats.  Cannot be combined with
    /// `--by-ext`.
    #[arg(long)]
    dirs_only: bool,
}

#[derive(ValueEnum, Clone, Debug)]
enum OutputFormat {
    Tree,
    Flat,
    Json,
    Csv,
}

#[derive(ValueEnum, Clone, Debug)]
enum CliSort {
    /// Largest first (default).
    Size,
    /// Alphabetical by path / extension, ascending.
    Name,
}

fn main() {
    let cli = Cli::parse();

    // Resolve path to absolute before walking.
    let root = cli
        .path
        .canonicalize()
        .unwrap_or_else(|e| {
            eprintln!("error: cannot resolve path '{}': {}", cli.path.display(), e);
            std::process::exit(1);
        });

    let opts = WalkOptions {
        size_mode: if cli.apparent {
            SizeMode::Apparent
        } else {
            SizeMode::DiskUsage
        },
        max_depth: cli.depth,
        follow_symlinks: cli.follow_symlinks,
        filter_ext: cli.r#type.map(|s| s.to_ascii_lowercase()),
        include_rsrc: cli.include_rsrc,
        quiet: cli.quiet,
    };

    let sort = match cli.sort {
        CliSort::Size => SortOrder::Size,
        CliSort::Name => SortOrder::Name,
    };

    let min_size: Option<u64> = cli.min_size.as_deref().map(|s| {
        parse_min_size(s).unwrap_or_else(|e| {
            eprintln!("error: --min-size: {}", e);
            std::process::exit(1);
        })
    });

    // Validate incompatible flag combinations before the expensive walk.
    if cli.by_ext && cli.dirs_only {
        eprintln!("error: --by-ext and --dirs-only cannot be used together");
        std::process::exit(1);
    }

    // Set up optional progress spinner.
    let pb = if cli.no_progress {
        None
    } else {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::with_template("{spinner:.cyan} [{elapsed_precise}] {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        // Render at ~20 fps so the terminal stays readable without burning CPU.
        spinner.enable_steady_tick(std::time::Duration::from_millis(50));
        Some(spinner)
    };

    let t0 = Instant::now();

    // Thread-safe counters for the parallel walker — captured by the closure
    // which may be called from multiple rayon worker threads simultaneously.
    let cb_count = Arc::new(AtomicU64::new(0));
    let cb_count_ref = Arc::clone(&cb_count);
    // ProgressBar::clone() is cheap (just an Arc clone) and is Send + Sync.
    let spinner_cb = pb.as_ref().map(|p| p.clone());

    let progress = move |path: &std::path::Path| {
        let n = cb_count_ref.fetch_add(1, Ordering::Relaxed);
        // Throttle: redraw every 32 files to avoid spinner lock contention.
        if n & 31 == 0 {
            if let Some(ref spinner) = spinner_cb {
                spinner.set_message(format!(
                    "{} files  {}",
                    n + 1,
                    truncate_path(path, 60),
                ));
            }
        }
    };

    let node = walker::walk_parallel_getattrlist(&root, &opts, &progress).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    });

    if let Some(ref pb) = pb {
        // Final message before clearing so the user sees the finished state briefly.
        pb.set_message(format!(
            "{} files  {}  done",
            node.file_count,
            formatter::human_size(node.total_size),
        ));
        std::thread::sleep(std::time::Duration::from_millis(120));
        pb.finish_and_clear();
    }

    let elapsed = t0.elapsed();

    // Render output.
    if cli.by_ext {
        match cli.format {
            OutputFormat::Json => print!("{}", output::json::render_json_by_ext(&node, sort)),
            OutputFormat::Csv  => print!("{}", output::csv::render_csv_by_ext(&node, sort)),
            OutputFormat::Tree | OutputFormat::Flat => print!("{}", formatter::render_by_extension(&node, sort)),
        }
    } else if cli.dirs_only {
        match cli.format {
            OutputFormat::Tree => {
                // Tree mode already displays directory sizes in context; pass through unchanged.
                // Warn the user so they know --dirs-only had no effect.
                eprintln!("note: --dirs-only has no effect with --format tree; use -f flat for a dirs-only listing");
                print!("{}", formatter::render_tree(&node, cli.depth, sort))
            }
            OutputFormat::Flat => {
                print!("{}", formatter::render_dirs_flat(&node, Some(cli.top), sort, min_size))
            }
            OutputFormat::Json => {
                print!("{}", output::json::render_json_dirs(&node, Some(cli.top), sort, min_size))
            }
            OutputFormat::Csv => {
                print!("{}", output::csv::render_csv_dirs(&node, Some(cli.top), sort, min_size))
            }
        }
    } else {
        match cli.format {
            OutputFormat::Tree => print!(
                "{}",
                formatter::render_tree(&node, cli.depth, sort)
            ),
            OutputFormat::Flat => {
                print!("{}", formatter::render_flat(&node, Some(cli.top), sort, min_size))
            }
            OutputFormat::Json => {
                print!("{}", output::json::render_json(&node, Some(cli.top), sort, min_size))
            }
            OutputFormat::Csv => {
                print!("{}", output::csv::render_csv(&node, Some(cli.top), sort, min_size))
            }
        }
    }

    if cli.stats {
        eprintln!(
            "\n{} unique files  |  total {}  |  {:.2?}",
            node.file_count,
            formatter::human_size(node.total_size),
            elapsed
        );
    }
}

/// Truncate a path for display; shows "…/parent/filename" when over `max_chars`.
fn truncate_path(path: &std::path::Path, max_chars: usize) -> String {
    let s = path.to_string_lossy();
    if s.len() <= max_chars {
        return s.into_owned();
    }
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    format!("…/{}/{}", parent, file)
}

/// Parse a human-readable size string into a byte count.
///
/// Accepts decimal or integer values with an optional unit suffix:
/// `B`, `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, `G`/`GB`/`GiB`, `T`/`TB`/`TiB`.
/// A space between the number and unit is allowed.
///
/// Examples: `"500"`, `"10MB"`, `"1.5 GiB"`, `"2T"`.
fn parse_min_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    // Reject special float literals before the unit-split so the error message
    // includes the actual value the user typed (e.g. 'nan') rather than ''.
    {
        let lower = s.to_lowercase();
        if lower == "nan" || lower == "inf" || lower == "infinity"
            || lower == "-inf" || lower == "-infinity"
            || lower == "+inf" || lower == "+infinity"
        {
            return Err(format!("'{}' is not a valid number", s));
        }
    }
    // Find where the numeric part ends (first alphabetic char or space).
    let split = s
        .find(|c: char| c.is_alphabetic() || c == ' ')
        .unwrap_or(s.len());
    let num_str  = s[..split].trim();
    let unit_str = s[split..].trim().to_uppercase();

    let num: f64 = num_str
        .parse()
        .map_err(|_| format!("'{}' is not a valid number", num_str))?;
    if !num.is_finite() {
        return Err(format!("'{}' is not a valid number", num_str));
    }
    if num < 0.0 {
        return Err("size must be non-negative".to_string());
    }

    let multiplier: u64 = match unit_str.as_str() {
        "" | "B"          => 1,
        "K" | "KB" | "KIB" => 1_024,
        "M" | "MB" | "MIB" => 1_024 * 1_024,
        "G" | "GB" | "GIB" => 1_024u64 * 1_024 * 1_024,
        "T" | "TB" | "TIB" => 1_024u64 * 1_024 * 1_024 * 1_024,
        other => return Err(format!("unknown unit '{}'", other)),
    };

    let result_f64 = num * multiplier as f64;
    if result_f64 > u64::MAX as f64 {
        return Err(format!("'{}' is too large (maximum is {} bytes)", s, u64::MAX));
    }
    Ok(result_f64 as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_bytes() {
        assert_eq!(parse_min_size("0").unwrap(), 0);
        assert_eq!(parse_min_size("512").unwrap(), 512);
        assert_eq!(parse_min_size("1024B").unwrap(), 1_024);
    }

    #[test]
    fn parse_kilobytes() {
        assert_eq!(parse_min_size("1K").unwrap(),   1_024);
        assert_eq!(parse_min_size("1KB").unwrap(),  1_024);
        assert_eq!(parse_min_size("1KiB").unwrap(), 1_024);
        assert_eq!(parse_min_size("2 KiB").unwrap(), 2_048);
    }

    #[test]
    fn parse_megabytes() {
        assert_eq!(parse_min_size("10MB").unwrap(),  10 * 1_024 * 1_024);
        assert_eq!(parse_min_size("10MiB").unwrap(), 10 * 1_024 * 1_024);
        assert_eq!(parse_min_size("10M").unwrap(),   10 * 1_024 * 1_024);
    }

    #[test]
    fn parse_gigabytes() {
        assert_eq!(parse_min_size("1GiB").unwrap(), 1_024u64 * 1_024 * 1_024);
        assert_eq!(parse_min_size("1GB").unwrap(),  1_024u64 * 1_024 * 1_024);
    }

    #[test]
    fn parse_fractional() {
        let half_gib = (0.5_f64 * (1_024u64 * 1_024 * 1_024) as f64) as u64;
        assert_eq!(parse_min_size("0.5GiB").unwrap(), half_gib);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_min_size("abc").is_err());
        assert!(parse_min_size("10QQ").is_err());
        assert!(parse_min_size("-1MB").is_err());
        // NaN and infinity must be rejected, not silently treated as 0 or pass the overflow check.
        assert!(parse_min_size("nan").is_err());
        assert!(parse_min_size("inf").is_err());
        assert!(parse_min_size("-inf").is_err());
    }

    // ── truncate_path tests ──────────────────────────────────────────────────

    #[test]
    fn truncate_path_short_path_returned_unchanged() {
        let p = std::path::Path::new("/usr/local/bin/mfa");
        assert_eq!(truncate_path(p, 80), "/usr/local/bin/mfa");
    }

    #[test]
    fn truncate_path_at_exact_boundary_returned_unchanged() {
        let p = std::path::Path::new("/exact");
        let s = p.to_string_lossy().to_string();
        // Path length equals max_chars → must not be truncated.
        assert_eq!(truncate_path(p, s.len()), s);
    }

    #[test]
    fn truncate_path_long_path_uses_ellipsis_format() {
        let p = std::path::Path::new(
            "/a/very/deeply/nested/directory/structure/with/many/levels/file.txt",
        );
        let result = truncate_path(p, 20);
        assert!(
            result.starts_with('…'),
            "long path must start with '…', got: {:?}",
            result
        );
        assert!(
            result.contains("file.txt"),
            "truncated path must contain the filename, got: {:?}",
            result
        );
        assert!(
            result.contains("levels"),
            "truncated path must contain the immediate parent directory name, got: {:?}",
            result
        );
    }

    #[test]
    fn truncate_path_result_format_is_ellipsis_parent_file() {
        let p = std::path::Path::new("/parent_dir/child_file.bin");
        // Make max_chars smaller than the full path.
        let result = truncate_path(p, 5);
        assert_eq!(result, "…/parent_dir/child_file.bin");
    }

    // ── parse_min_size overflow tests ────────────────────────────────────────

    #[test]
    fn parse_min_size_overflow_returns_error() {
        // 999_999_999_999 TB vastly exceeds u64::MAX (~18.4 EB).
        assert!(
            parse_min_size("999999999999TB").is_err(),
            "hugely oversized value must return an error, not silently truncate"
        );
    }

    #[test]
    fn parse_min_size_large_valid_value_succeeds() {
        // 16383 TiB is a large but representable u64 value (~16 PiB).
        let val = parse_min_size("16383TB").unwrap();
        let expected: u64 = 16383u64 * 1_024 * 1_024 * 1_024 * 1_024;
        assert_eq!(val, expected);
    }
}
