use reproit::perf_bench::{
    BatchWorkload, FingerprintWorkload, FrontierWorkload, LogWorkload, MergeWorkload,
    PermissionWorkload, PersistenceWorkload,
};
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

fn add_live(bytes: u64) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            add_live(layout.size() as u64);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        System.dealloc(pointer, layout);
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, size: usize) -> *mut u8 {
        let next = System.realloc(pointer, old, size);
        if !next.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
            LIVE_BYTES.fetch_sub(old.size() as u64, Ordering::Relaxed);
            add_live(size as u64);
        }
        next
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

enum Workload {
    Frontier(FrontierWorkload),
    Log(LogWorkload),
    Merge(MergeWorkload),
    Batch(BatchWorkload),
    Permission(PermissionWorkload),
    Persistence(PersistenceWorkload),
    Fingerprint(FingerprintWorkload),
}

impl Workload {
    fn new(name: &str, size: usize) -> Self {
        match name {
            "frontier" => Self::Frontier(FrontierWorkload::new(size)),
            "log" => Self::Log(LogWorkload::new(size)),
            "merge" => Self::Merge(MergeWorkload::new(size)),
            "batch" => Self::Batch(BatchWorkload::new(size)),
            "permission" => Self::Permission(PermissionWorkload::new(size)),
            "persistence" => Self::Persistence(PersistenceWorkload::new(size)),
            "fingerprint" => Self::Fingerprint(FingerprintWorkload::new(size)),
            _ => panic!("unknown benchmark {name}"),
        }
    }

    fn run(&mut self) -> usize {
        match self {
            Self::Frontier(workload) => workload.run(),
            Self::Log(workload) => workload.run(),
            Self::Merge(workload) => workload.run(),
            Self::Batch(workload) => workload.run(),
            Self::Permission(workload) => workload.run(),
            Self::Persistence(workload) => workload.run(),
            Self::Fingerprint(workload) => workload.run(),
        }
    }
}

fn reset_allocations() {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    LIVE_BYTES.store(0, Ordering::Relaxed);
    PEAK_LIVE_BYTES.store(0, Ordering::Relaxed);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().expect("benchmark name");
    let size = args.next().expect("size").parse::<usize>().expect("size");
    let iterations = args
        .next()
        .unwrap_or_else(|| "1".to_string())
        .parse::<u64>()
        .expect("iterations");
    let samples = args
        .next()
        .unwrap_or_else(|| "7".to_string())
        .parse::<usize>()
        .expect("samples");
    let mut workload = Workload::new(&name, size);
    std::hint::black_box(workload.run());

    let mut timings = Vec::with_capacity(samples);
    let mut checksum = 0_usize;
    for _ in 0..samples {
        let started = Instant::now();
        for _ in 0..iterations {
            checksum ^= std::hint::black_box(workload.run());
        }
        timings.push(started.elapsed().as_nanos() as u64 / iterations);
    }
    timings.sort_unstable();

    reset_allocations();
    checksum ^= std::hint::black_box(workload.run());
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let peak_live_bytes = PEAK_LIVE_BYTES.load(Ordering::Relaxed);
    println!(
        "{}",
        json!({
            "benchmark": name,
            "size": size,
            "iterations": iterations,
            "samples": samples,
            "medianNs": timings[timings.len() / 2],
            "minNs": timings[0],
            "maxNs": timings[timings.len() - 1],
            "allocations": allocations,
            "allocatedBytes": allocated_bytes,
            "peakLiveBytes": peak_live_bytes,
            "checksum": checksum,
        })
    );
}
