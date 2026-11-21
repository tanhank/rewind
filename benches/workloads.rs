//! Repeatable performance benchmark suite for rewind.
//!
//! Measures Workloads A, B, C, D, and E across filesystem types (NVMe Btrfs vs RAM tmpfs).
//! Prints statistical metrics (mean, min, max, throughput).

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rewind::commands::{self, CleanOptions};
use rewind::config::Config;
use rewind::journal::{EntryStatus, Journal};
use rewind::paths::Paths;
use rewind::staging::{self, SnapshotMode, Stager};

#[derive(Debug, Clone)]
struct BenchResult {
    name: String,
    samples: Vec<Duration>,
    items: Option<u64>,
    bytes: Option<u64>,
}

impl BenchResult {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            samples: Vec::new(),
            items: None,
            bytes: None,
        }
    }

    fn record(&mut self, dur: Duration) {
        self.samples.push(dur);
    }

    fn with_items(mut self, items: u64) -> Self {
        self.items = Some(items);
        self
    }

    fn with_bytes(mut self, bytes: u64) -> Self {
        self.bytes = Some(bytes);
        self
    }

    fn print_summary(&self) {
        if self.samples.is_empty() {
            return;
        }
        let total: Duration = self.samples.iter().sum();
        let count = self.samples.len() as u32;
        let mean = total / count;
        let min = *self.samples.iter().min().unwrap();
        let max = *self.samples.iter().max().unwrap();

        let mean_ms = mean.as_secs_f64() * 1000.0;
        let min_ms = min.as_secs_f64() * 1000.0;
        let max_ms = max.as_secs_f64() * 1000.0;

        let mut throughput_str = String::new();
        if let Some(items) = self.items {
            let items_per_sec = (items as f64) / mean.as_secs_f64();
            throughput_str.push_str(&format!(" | {:.0} items/sec", items_per_sec));
        }
        if let Some(bytes) = self.bytes {
            let mb = (bytes as f64) / (1024.0 * 1024.0);
            let mb_per_sec = mb / mean.as_secs_f64();
            throughput_str.push_str(&format!(" | {:.2} MB/sec", mb_per_sec));
        }

        println!(
            "{:<48} : mean={:>8.2}ms (min={:>8.2}ms, max={:>8.2}ms, N={:>2}){}",
            self.name, mean_ms, min_ms, max_ms, count, throughput_str
        );
    }
}

fn create_flat_files(dir: &Path, count: usize, size_bytes: usize) {
    fs::create_dir_all(dir).unwrap();
    let data = vec![0xABu8; size_bytes];
    for i in 0..count {
        let file_path = dir.join(format!("file_{i:06}.dat"));
        fs::write(&file_path, &data).unwrap();
    }
}

fn create_nested_files(dir: &Path, depth: usize, files_per_level: usize, size_bytes: usize) {
    fs::create_dir_all(dir).unwrap();
    let data = vec![0xCDu8; size_bytes];
    let mut current = dir.to_path_buf();
    for d in 0..depth {
        current = current.join(format!("level_{d}"));
        fs::create_dir_all(&current).unwrap();
        for f in 0..files_per_level {
            let file_path = current.join(format!("nested_{f:04}.dat"));
            fs::write(&file_path, &data).unwrap();
        }
    }
}

fn create_sparse_file(path: &Path, size_bytes: u64) {
    let mut file = File::create(path).unwrap();
    file.seek(SeekFrom::Start(size_bytes - 4)).unwrap();
    file.write_all(b"tail").unwrap();
}

// ---------------------------------------------------------------------------
// Workload A: File Count Scaling
// ---------------------------------------------------------------------------
fn bench_workload_a(root_btrfs: &Path) {
    println!("\n=== Workload A: File Count Scaling (Traversal & Tree Size) ===");

    for count in [1_000, 5_000, 10_000] {
        let test_dir = root_btrfs.join(format!("workload_a_flat_{count}"));
        create_flat_files(&test_dir, count, 64);

        let mut res =
            BenchResult::new(format!("tree_size: flat {} files", count)).with_items(count as u64);
        for _ in 0..5 {
            let t0 = Instant::now();
            let _ = staging::tree_size(&test_dir).unwrap();
            res.record(t0.elapsed());
        }
        res.print_summary();
        let _ = fs::remove_dir_all(&test_dir);
    }

    // Nested directory structure: 10 levels deep, 500 files per level = 5,000 files
    let nested_dir = root_btrfs.join("workload_a_nested_5000");
    create_nested_files(&nested_dir, 10, 500, 64);
    let mut res_nested =
        BenchResult::new("tree_size: nested 5000 files (10 levels)").with_items(5000);
    for _ in 0..5 {
        let t0 = Instant::now();
        let _ = staging::tree_size(&nested_dir).unwrap();
        res_nested.record(t0.elapsed());
    }
    res_nested.print_summary();
    let _ = fs::remove_dir_all(&nested_dir);
}

// ---------------------------------------------------------------------------
// Workload B: File Size Scaling
// ---------------------------------------------------------------------------
fn bench_workload_b(root_btrfs: &Path) {
    println!("\n=== Workload B: File Size Scaling (Snapshot & Copy) ===");

    // 1,000 small files (4 KB each = 4 MB)
    let small_dir = root_btrfs.join("workload_b_small");
    create_flat_files(&small_dir, 1_000, 4 * 1024);
    let clone_small_dst = root_btrfs.join("workload_b_small_clone");
    let mut res_small = BenchResult::new("clone_tree: 1000 x 4KB files (4 MB)")
        .with_items(1000)
        .with_bytes(4 * 1024 * 1024);
    for _ in 0..3 {
        let _ = staging::remove_tree(&clone_small_dst);
        let t0 = Instant::now();
        let _ = staging::clone_tree(&small_dir, &clone_small_dst).unwrap();
        res_small.record(t0.elapsed());
    }
    res_small.print_summary();
    let _ = staging::remove_tree(&small_dir);
    let _ = staging::remove_tree(&clone_small_dst);

    // 50 medium files (1 MB each = 50 MB)
    let med_dir = root_btrfs.join("workload_b_med");
    create_flat_files(&med_dir, 50, 1024 * 1024);
    let clone_med_dst = root_btrfs.join("workload_b_med_clone");
    let mut res_med = BenchResult::new("clone_tree: 50 x 1MB files (50 MB)")
        .with_items(50)
        .with_bytes(50 * 1024 * 1024);
    for _ in 0..3 {
        let _ = staging::remove_tree(&clone_med_dst);
        let t0 = Instant::now();
        let _ = staging::clone_tree(&med_dir, &clone_med_dst).unwrap();
        res_med.record(t0.elapsed());
    }
    res_med.print_summary();
    let _ = staging::remove_tree(&med_dir);
    let _ = staging::remove_tree(&clone_med_dst);

    // 1 large file (50 MB)
    let large_src = root_btrfs.join("workload_b_large.bin");
    fs::write(&large_src, vec![0xEFu8; 50 * 1024 * 1024]).unwrap();
    let large_dst = root_btrfs.join("workload_b_large_clone.bin");
    let meta = fs::symlink_metadata(&large_src).unwrap();
    let mut res_large = BenchResult::new("clone_file_isolated: 1 x 50MB file (50 MB)")
        .with_items(1)
        .with_bytes(50 * 1024 * 1024);
    for _ in 0..3 {
        let _ = fs::remove_file(&large_dst);
        let t0 = Instant::now();
        let _ = staging::clone_file_isolated(&large_src, &large_dst, &meta).unwrap();
        res_large.record(t0.elapsed());
    }
    res_large.print_summary();
    let _ = fs::remove_file(&large_src);
    let _ = fs::remove_file(&large_dst);

    // 1 sparse file (50 MB logical, 4 bytes physical)
    let sparse_src = root_btrfs.join("workload_b_sparse.bin");
    create_sparse_file(&sparse_src, 50 * 1024 * 1024);
    let sparse_dst = root_btrfs.join("workload_b_sparse_clone.bin");
    let meta_sp = fs::symlink_metadata(&sparse_src).unwrap();
    let mut res_sparse = BenchResult::new("clone_file_isolated: 1 x 50MB sparse file")
        .with_items(1)
        .with_bytes(50 * 1024 * 1024);
    for _ in 0..3 {
        let _ = fs::remove_file(&sparse_dst);
        let t0 = Instant::now();
        let _ = staging::clone_file_isolated(&sparse_src, &sparse_dst, &meta_sp).unwrap();
        res_sparse.record(t0.elapsed());
    }
    res_sparse.print_summary();
    let _ = fs::remove_file(&sparse_src);
    let _ = fs::remove_file(&sparse_dst);
}

// ---------------------------------------------------------------------------
// Workload C: Snapshot Operations (NVMe Btrfs CoW vs RAM tmpfs Copy)
// ---------------------------------------------------------------------------
fn bench_workload_c(root_btrfs: &Path, root_tmpfs: &Path) {
    println!("\n=== Workload C: Filesystem Comparison (NVMe Btrfs vs RAM tmpfs) ===");

    // Measure medium 50MB file copy on Btrfs (CoW reflink FICLONE)
    let btrfs_src = root_btrfs.join("cow_50mb.bin");
    fs::write(&btrfs_src, vec![0x11u8; 50 * 1024 * 1024]).unwrap();
    let btrfs_meta = fs::symlink_metadata(&btrfs_src).unwrap();
    let btrfs_dst = root_btrfs.join("cow_50mb_clone.bin");

    let mut res_btrfs = BenchResult::new("NVMe Btrfs (CoW reflink): 50MB file")
        .with_items(1)
        .with_bytes(50 * 1024 * 1024);
    for _ in 0..5 {
        let _ = fs::remove_file(&btrfs_dst);
        let t0 = Instant::now();
        let _ = staging::clone_file_isolated(&btrfs_src, &btrfs_dst, &btrfs_meta).unwrap();
        res_btrfs.record(t0.elapsed());
    }
    res_btrfs.print_summary();
    let _ = fs::remove_file(&btrfs_src);
    let _ = fs::remove_file(&btrfs_dst);

    // Measure medium 50MB file copy on tmpfs (Byte copy fallback)
    let tmpfs_src = root_tmpfs.join("copy_50mb.bin");
    fs::write(&tmpfs_src, vec![0x22u8; 50 * 1024 * 1024]).unwrap();
    let tmpfs_meta = fs::symlink_metadata(&tmpfs_src).unwrap();
    let tmpfs_dst = root_tmpfs.join("copy_50mb_clone.bin");

    let mut res_tmpfs = BenchResult::new("RAM tmpfs (Byte copy fallback): 50MB file")
        .with_items(1)
        .with_bytes(50 * 1024 * 1024);
    for _ in 0..5 {
        let _ = fs::remove_file(&tmpfs_dst);
        let t0 = Instant::now();
        let _ = staging::clone_file_isolated(&tmpfs_src, &tmpfs_dst, &tmpfs_meta).unwrap();
        res_tmpfs.record(t0.elapsed());
    }
    res_tmpfs.print_summary();
    let _ = fs::remove_file(&tmpfs_src);
    let _ = fs::remove_file(&tmpfs_dst);

    // Symlinks test: 500 symlinks staging & restore
    let sym_dir = root_btrfs.join("symlinks_dir");
    fs::create_dir_all(&sym_dir).unwrap();
    let target_file = sym_dir.join("target.txt");
    fs::write(&target_file, b"target").unwrap();
    for i in 0..500 {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&target_file, sym_dir.join(format!("link_{i:04}")));
    }
    let sym_clone = root_btrfs.join("symlinks_clone");
    let mut res_sym = BenchResult::new("clone_tree: 500 symlinks").with_items(500);
    for _ in 0..3 {
        let _ = staging::remove_tree(&sym_clone);
        let t0 = Instant::now();
        let _ = staging::clone_tree(&sym_dir, &sym_clone).unwrap();
        res_sym.record(t0.elapsed());
    }
    res_sym.print_summary();
    let _ = staging::remove_tree(&sym_dir);
    let _ = staging::remove_tree(&sym_clone);
}

// ---------------------------------------------------------------------------
// Workload D: End-to-End Commands & Storage Accounting
// ---------------------------------------------------------------------------
fn bench_workload_d(root_btrfs: &Path) {
    println!("\n=== Workload D: End-to-End Command & Staging Workloads ===");

    let data_dir = root_btrfs.join("data");
    let paths = Paths {
        trash_dir: data_dir.join("trash"),
        journal_path: data_dir.join("journal.db"),
        lock_path: data_dir.join("rewind.lock"),
        config_path: root_btrfs.join("config").join("config.toml"),
        data_dir,
    };
    let journal_path = paths.journal_path.clone();
    let mut journal = Journal::open(&journal_path).unwrap();

    // End-to-end rm staging (500 files moved into trash)
    let rm_src = root_btrfs.join("rm_src");
    create_flat_files(&rm_src, 500, 128);
    let entry_id = journal
        .begin_entry("rm", &["rm".into(), "-rf".into()], root_btrfs)
        .unwrap();
    let stager = Stager::new(&paths, entry_id);

    let mut res_rm = BenchResult::new("Stager::stage_move (500 files)").with_items(500);
    let t0 = Instant::now();
    for i in 0..500 {
        let f = rm_src.join(format!("file_{i:06}.dat"));
        let _ = stager.stage_move(&f).unwrap();
    }
    res_rm.record(t0.elapsed());
    res_rm.print_summary();
    journal
        .finish_entry(entry_id, EntryStatus::Done, Some(0), true, None)
        .unwrap();
    let _ = staging::remove_tree(&rm_src);

    // End-to-end snapshot copy staging (500 files snapshotted with Clone)
    let cp_src = root_btrfs.join("cp_src");
    create_flat_files(&cp_src, 500, 128);
    let entry_id2 = journal
        .begin_entry("cp", &["cp".into(), "file".into()], root_btrfs)
        .unwrap();
    let stager2 = Stager::new(&paths, entry_id2);

    let mut res_cp = BenchResult::new("Stager::snapshot (500 files, Clone)").with_items(500);
    let t0 = Instant::now();
    for i in 0..500 {
        let f = cp_src.join(format!("file_{i:06}.dat"));
        let _ = stager2.snapshot(&f, SnapshotMode::Clone).unwrap();
    }
    res_cp.record(t0.elapsed());
    res_cp.print_summary();
    journal
        .finish_entry(entry_id2, EntryStatus::Done, Some(0), true, None)
        .unwrap();
    let _ = staging::remove_tree(&cp_src);

    // End-to-end clean execution
    let config = Config::default();
    let clean_opts = CleanOptions {
        dry_run: false,
        all: true,
        older_than: None,
        json: false,
    };
    let mut res_clean = BenchResult::new("commands::clean (purging all entries)");
    let t0 = Instant::now();
    let _ = commands::clean(&mut journal, &paths, &config, &clean_opts).unwrap();
    res_clean.record(t0.elapsed());
    res_clean.print_summary();
}

// ---------------------------------------------------------------------------
// Workload E: Database Operations
// ---------------------------------------------------------------------------
fn bench_workload_e(root_btrfs: &Path) {
    println!("\n=== Workload E: Database Throughput & Integrity Operations ===");

    let db_path = root_btrfs.join("bench_journal.db");
    let mut journal = Journal::open(&db_path).unwrap();

    // 1,000 begin_entry + finish_entry operations
    let count = 1_000u64;
    let mut res_entries =
        BenchResult::new("Journal::begin_entry + finish_entry (1000)").with_items(count);
    let t0 = Instant::now();
    for i in 0..count {
        let entry_id = journal
            .begin_entry("bench", &["test".into()], root_btrfs)
            .unwrap();
        journal
            .finish_entry(entry_id, EntryStatus::Done, Some(0), true, None)
            .unwrap();
        if i % 100 == 0 {
            // keep batches reasonable
        }
    }
    res_entries.record(t0.elapsed());
    res_entries.print_summary();

    // Capacity reservation lifecycle (1000 acquire + release via delete_entry)
    let mut res_res =
        BenchResult::new("Journal::reserve_capacity + delete (1000)").with_items(count);
    let t0 = Instant::now();
    for _ in 0..count {
        let entry_id = journal
            .begin_entry("bench_res", &["reserve".into()], root_btrfs)
            .unwrap();
        journal
            .reserve_capacity(entry_id, 1024 * 1024, 100 * 1024 * 1024, 30)
            .unwrap();
        journal.delete_entry(entry_id).unwrap();
    }
    res_res.record(t0.elapsed());
    res_res.print_summary();

    // Vacuum operation
    let mut res_vac = BenchResult::new("Journal::vacuum");
    let t0 = Instant::now();
    journal.vacuum().unwrap();
    res_vac.record(t0.elapsed());
    res_vac.print_summary();

    // WAL Checkpoint operation
    let mut res_wal = BenchResult::new("Journal::wal_checkpoint(TRUNCATE)");
    let t0 = Instant::now();
    journal.wal_checkpoint().unwrap();
    res_wal.record(t0.elapsed());
    res_wal.print_summary();

    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(root_btrfs.join("bench_journal.db-wal"));
    let _ = fs::remove_file(root_btrfs.join("bench_journal.db-shm"));
}

fn main() {
    println!("============================================================");
    println!("REWIND PERFORMANCE BENCHMARK SUITE");
    println!("============================================================");

    // Setup isolated test directories
    let cwd = std::env::current_dir().unwrap();
    let btrfs_dir = cwd.join("target").join("bench_btrfs");
    let tmpfs_dir = PathBuf::from("/tmp").join("bench_tmpfs");

    let _ = fs::remove_dir_all(&btrfs_dir);
    let _ = fs::remove_dir_all(&tmpfs_dir);
    fs::create_dir_all(&btrfs_dir).unwrap();
    fs::create_dir_all(&tmpfs_dir).unwrap();

    println!("NVMe Btrfs test root: {}", btrfs_dir.display());
    println!("RAM tmpfs test root:  {}", tmpfs_dir.display());

    bench_workload_a(&btrfs_dir);
    bench_workload_b(&btrfs_dir);
    bench_workload_c(&btrfs_dir, &tmpfs_dir);
    bench_workload_d(&btrfs_dir);
    bench_workload_e(&btrfs_dir);

    // Clean up
    let _ = fs::remove_dir_all(&btrfs_dir);
    let _ = fs::remove_dir_all(&tmpfs_dir);

    println!("\n============================================================");
    println!("BENCHMARK SUITE COMPLETE");
    println!("============================================================");
}
