/// Criterion benchmark: measure wall-clock time to walk a synthetic directory tree.
///
/// Baseline (Phase 1): single-threaded walker using lstat.
/// Each subsequent phase must show improvement against this baseline.
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mac_file_analyzer::formatter::human_size;
use std::fs;
use tempfile::TempDir;

// Build a synthetic tree: `dirs` directories each containing `files_per_dir` files
// of `file_size` bytes.
fn build_tree(dirs: usize, files_per_dir: usize, file_size: usize) -> TempDir {
    let total = dirs * files_per_dir;
    eprintln!(
        "\n  [bench setup] building {} dirs × {} files = {} files ({} each)…",
        dirs,
        files_per_dir,
        total,
        human_size((file_size) as u64),
    );
    let root = tempfile::tempdir().expect("tempdir");
    let content = vec![b'x'; file_size];
    let report_every = (total / 10).max(1);
    let mut created = 0usize;
    for d in 0..dirs {
        let dir = root.path().join(format!("dir_{:04}", d));
        fs::create_dir(&dir).unwrap();
        for f in 0..files_per_dir {
            fs::write(dir.join(format!("file_{:04}.dat", f)), &content).unwrap();
            created += 1;
            if created % report_every == 0 {
                eprintln!("  [bench setup] {:>6} / {} files written…", created, total);
            }
        }
    }
    eprintln!("  [bench setup] done — {} files ready\n", created);
    root
}

fn bench_walk(c: &mut Criterion) {
    // 50 directories × 100 files = 5 000 files, 4 KiB each ≈ 20 MiB.
    let tree = build_tree(50, 100, 4096);
    let root = tree.path().to_path_buf();

    use mac_file_analyzer::walker::{walk, WalkOptions};

    c.bench_function("walk_5k_files_phase1", |b| {
        b.iter(|| {
            let node = walk(
                black_box(&root),
                &WalkOptions::default(),
                &mut |_| {},
            )
            .unwrap();
            black_box(node.total_size);
        })
    });

    // Larger tree: 200 dirs × 200 files = 40 000 files.
    let large_tree = build_tree(200, 200, 512);
    let large_root = large_tree.path().to_path_buf();

    c.bench_function("walk_40k_files_phase1", |b| {
        b.iter(|| {
            let node = walk(
                black_box(&large_root),
                &WalkOptions::default(),
                &mut |_| {},
            )
            .unwrap();
            black_box(node.total_size);
        })
    });
}

/// Phase 2: parallel walk benchmarks — compare directly against Phase 1 numbers.
fn bench_walk_phase2(c: &mut Criterion) {
    use mac_file_analyzer::walker::{walk_parallel, WalkOptions};

    // Same 5 000-file tree as Phase 1.
    let tree = build_tree(50, 100, 4096);
    let root = tree.path().to_path_buf();

    c.bench_function("walk_5k_files_phase2", |b| {
        b.iter(|| {
            let node =
                walk_parallel(black_box(&root), &WalkOptions::default(), &|_: &_| {}).unwrap();
            black_box(node.total_size);
        })
    });

    // Same 40 000-file tree as Phase 1.
    let large_tree = build_tree(200, 200, 512);
    let large_root = large_tree.path().to_path_buf();

    c.bench_function("walk_40k_files_phase2", |b| {
        b.iter(|| {
            let node = walk_parallel(
                black_box(&large_root),
                &WalkOptions::default(),
                &|_: &_| {},
            )
            .unwrap();
            black_box(node.total_size);
        })
    });
}

/// Phase 3: getattrlist parallel walk benchmarks — compare against Phase 2.
fn bench_walk_phase3(c: &mut Criterion) {
    use mac_file_analyzer::walker::{walk_parallel_getattrlist, WalkOptions};

    // Same 5 000-file tree as Phases 1 & 2.
    let tree = build_tree(50, 100, 4096);
    let root = tree.path().to_path_buf();

    c.bench_function("walk_5k_files_phase3", |b| {
        b.iter(|| {
            let node = walk_parallel_getattrlist(
                black_box(&root),
                &WalkOptions::default(),
                &|_: &_| {},
            )
            .unwrap();
            black_box(node.total_size);
        })
    });

    // Same 40 000-file tree as Phases 1 & 2.
    let large_tree = build_tree(200, 200, 512);
    let large_root = large_tree.path().to_path_buf();

    c.bench_function("walk_40k_files_phase3", |b| {
        b.iter(|| {
            let node = walk_parallel_getattrlist(
                black_box(&large_root),
                &WalkOptions::default(),
                &|_: &_| {},
            )
            .unwrap();
            black_box(node.total_size);
        })
    });
}

/// Phase 5: output-rendering benchmarks — measure formatter and serialiser
/// throughput on a pre-built 5 000-file tree.
///
/// These benchmarks isolate the rendering layer; the tree is built once per
/// benchmark function so setup cost is not included in measurements.
fn bench_output_rendering(c: &mut Criterion) {
    use mac_file_analyzer::aggregator::SortOrder;
    use mac_file_analyzer::walker::{walk_parallel_getattrlist, WalkOptions};
    use mac_file_analyzer::{formatter, output};

    let tree = build_tree(50, 100, 4096);
    let root = tree.path().to_path_buf();
    let opts = WalkOptions::default();
    let node = walk_parallel_getattrlist(&root, &opts, &|_: &_| {})
        .expect("walk failed during bench setup");

    c.bench_function("render_tree_5k", |b| {
        b.iter(|| {
            let s = formatter::render_tree(black_box(&node), None, SortOrder::Size);
            black_box(s);
        })
    });

    c.bench_function("render_flat_5k", |b| {
        b.iter(|| {
            let s = formatter::render_flat(black_box(&node), Some(100), SortOrder::Size, None);
            black_box(s);
        })
    });

    c.bench_function("render_by_ext_5k", |b| {
        b.iter(|| {
            let s = formatter::render_by_extension(black_box(&node), SortOrder::Size);
            black_box(s);
        })
    });

    c.bench_function("render_json_5k", |b| {
        b.iter(|| {
            let s = output::json::render_json(black_box(&node), Some(100), SortOrder::Size, None);
            black_box(s);
        })
    });

    c.bench_function("render_json_by_ext_5k", |b| {
        b.iter(|| {
            let s = output::json::render_json_by_ext(black_box(&node), SortOrder::Size);
            black_box(s);
        })
    });

    c.bench_function("render_csv_5k", |b| {
        b.iter(|| {
            let s = output::csv::render_csv(black_box(&node), Some(100), SortOrder::Size, None);
            black_box(s);
        })
    });

    c.bench_function("render_csv_by_ext_5k", |b| {
        b.iter(|| {
            let s = output::csv::render_csv_by_ext(black_box(&node), SortOrder::Size);
            black_box(s);
        })
    });
}

criterion_group!(
    benches,
    bench_walk,
    bench_walk_phase2,
    bench_walk_phase3,
    bench_output_rendering
);
criterion_main!(benches);
