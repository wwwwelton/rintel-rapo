//! Cache-locality benchmark for RAPO's critical-thread pinning (Windows hardware only).
//! Build: `cargo test --target x86_64-pc-windows-gnu --no-run`; run the test exe on Windows with
//! `--ignored --exact cache_bench::pinned_cache_locality --nocapture --test-threads=1`.
//! Alternates unpinned/pinned windows (ABBA) in one run so background load drift cancels out,
//! then prints one `CACHE_BENCH_JSON=` line.

use crate::learning::{Score, Settings};
use crate::pin::Pinner;
use crate::windows::Running;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::GetCurrentProcessorNumber;

const WORKERS: usize = 4; // Settings::default().threads
const NOISE: usize = 12;
const LINE_WORDS: usize = 16; // one 64-byte line of u32
const PRIVATE_LINES: usize = (2 << 20) / 64; // 2 MiB: fits a 3 MiB P-core L2, overflows a shared E-cluster L2
const SHARED_LINES: usize = (16 << 20) / 64; // 16 MiB: L3-resident
const NOISE_LINES: usize = (1 << 20) / 64;
const PRIVATE_PASSES: usize = 8;
const SHARED_STEPS: usize = 8192;
const PAIRS: usize = 5;
const WINDOW: Duration = Duration::from_secs(4);
const SETTLE: Duration = Duration::from_millis(1500);
const NO_WINDOW: usize = usize::MAX;

/// Single random cycle over `lines` cache lines (Sattolo), defeating the hardware prefetcher.
fn chain(lines: usize, mut seed: u64) -> Vec<u32> {
    let mut next: Vec<u32> = (0..lines as u32).collect();
    for i in (1..lines).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        next.swap(i, (seed % i as u64) as usize);
    }
    let mut buffer = vec![0u32; lines * LINE_WORDS];
    for (line, target) in next.into_iter().enumerate() {
        buffer[line * LINE_WORDS] = target;
    }
    buffer
}

fn chase(buffer: &[u32], mut at: u32, steps: usize) -> u32 {
    for _ in 0..steps {
        at = buffer[at as usize * LINE_WORDS];
    }
    at
}

#[derive(Default)]
struct WindowStats {
    worker_frames: AtomicU64,
    migrations: AtomicU64,
    pcore_frames: AtomicU64,
    cpu0_frames: AtomicU64,
}

struct Shared {
    start: Barrier,
    end: Barrier,
    quit: AtomicBool,
    stop: AtomicBool,
    window: AtomicUsize,
    stats: Vec<WindowStats>,
    data: Vec<u32>,
    p_mask: usize,
}

fn worker(id: usize, shared: Arc<Shared>) {
    let private = chain(PRIVATE_LINES, 0x9e37_79b9_7f4a_7c15 ^ id as u64);
    let (mut at, mut shared_at) = (0u32, (id * 4099 % SHARED_LINES) as u32);
    let mut last_cpu = None;
    loop {
        shared.start.wait();
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        let window = shared.window.load(Ordering::Acquire);
        let first = unsafe { GetCurrentProcessorNumber() };
        at = chase(&private, at, PRIVATE_LINES * PRIVATE_PASSES);
        shared_at = chase(&shared.data, shared_at, SHARED_STEPS);
        let cpu = unsafe { GetCurrentProcessorNumber() };
        if let Some(stats) = shared.stats.get(window) {
            stats.worker_frames.fetch_add(1, Ordering::Relaxed);
            let moved = u64::from(last_cpu.is_some_and(|last| last != first)) + u64::from(first != cpu);
            stats.migrations.fetch_add(moved, Ordering::Relaxed);
            stats.pcore_frames.fetch_add(u64::from(shared.p_mask >> cpu & 1 == 1), Ordering::Relaxed);
            stats.cpu0_frames.fetch_add(u64::from(cpu == 0), Ordering::Relaxed);
        }
        last_cpu = Some(cpu);
        std::hint::black_box((at, shared_at));
        shared.end.wait();
    }
}

/// Background threads at roughly one-third duty that preempt workers and pollute caches.
fn noise(id: usize, shared: Arc<Shared>) {
    let buffer = chain(NOISE_LINES, 0xd1b5_4a32_d192_ed03 ^ id as u64);
    let mut at = 0;
    while !shared.stop.load(Ordering::Acquire) {
        let busy = Instant::now();
        while busy.elapsed() < Duration::from_millis(1) {
            at = chase(&buffer, at, 256);
        }
        std::hint::black_box(at);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Frame loop: every frame releases all workers and waits for the slowest one.
fn frames(shared: Arc<Shared>) -> Vec<(usize, f64)> {
    let mut times = Vec::new();
    loop {
        let quit = shared.stop.load(Ordering::Acquire);
        // Published before the barrier so every participant sees the same decision for this frame.
        shared.quit.store(quit, Ordering::Release);
        let window = shared.window.load(Ordering::Acquire);
        let started = Instant::now();
        shared.start.wait();
        if quit {
            return times;
        }
        shared.end.wait();
        if window != NO_WINDOW {
            times.push((window, started.elapsed().as_secs_f64()));
        }
    }
}

fn score(times: &[f64]) -> Score {
    let mut sorted = times.to_vec();
    sorted.sort_by(|a, b| b.total_cmp(a));
    let slow = sorted.len().div_ceil(100);
    Score {
        fps: times.len() as f64 / times.iter().sum::<f64>(),
        low_fps: slow as f64 / sorted[..slow].iter().sum::<f64>(),
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 0 { (values[mid - 1] + values[mid]) / 2.0 } else { values[mid] }
}

#[test]
#[ignore = "benchmark: run explicitly on Windows hardware"]
fn pinned_cache_locality() {
    let mut pinner = Pinner::new().expect("CPU topology");
    let exe = std::env::current_exe().unwrap();
    let pid = std::process::id();
    let running = [Running {
        pid,
        game: "cache-bench".into(),
        directory: exe.parent().unwrap().to_path_buf(),
        exe: exe.clone(),
    }];
    let profiles: HashMap<PathBuf, Settings> = HashMap::from([(exe, Settings { pin: true, ..Settings::default() })]);
    let none = HashSet::new();
    let shared = Arc::new(Shared {
        start: Barrier::new(WORKERS + 1),
        end: Barrier::new(WORKERS + 1),
        quit: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        window: AtomicUsize::new(NO_WINDOW),
        stats: (0..PAIRS * 2).map(|_| WindowStats::default()).collect(),
        data: chain(SHARED_LINES, 0x2545_f491_4f6c_dd1d),
        p_mask: pinner.mask,
    });
    let workers: Vec<_> = (0..WORKERS).map(|id| { let s = shared.clone(); std::thread::spawn(move || worker(id, s)) }).collect();
    let noise_threads: Vec<_> = (0..NOISE).map(|id| { let s = shared.clone(); std::thread::spawn(move || noise(id, s)) }).collect();
    let coordinator = { let s = shared.clone(); std::thread::spawn(move || frames(s)) };

    std::thread::sleep(Duration::from_secs(2));
    let (mut pinned_windows, mut min_pinned, mut max_unpinned) = (Vec::new(), usize::MAX, 0);
    for pair in 0..PAIRS {
        let order = if pair % 2 == 0 { [false, true] } else { [true, false] };
        for pinned in order {
            if pinned {
                // Two polls: the first only samples thread CPU times, the second selects and pins.
                pinner.update(&running, &none, true, &profiles).unwrap();
                std::thread::sleep(Duration::from_secs(1));
                pinner.update(&running, &none, true, &profiles).unwrap();
                std::thread::sleep(SETTLE - Duration::from_secs(1));
            } else {
                pinner.update(&[], &none, true, &profiles).unwrap();
                std::thread::sleep(SETTLE);
            }
            shared.window.store(pinned_windows.len(), Ordering::Release);
            let end = Instant::now() + WINDOW;
            loop {
                let pins = pinner.pinned_count(pid);
                if pinned { min_pinned = min_pinned.min(pins) } else { max_unpinned = max_unpinned.max(pins) }
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                std::thread::sleep(left.min(Duration::from_secs(1)));
                if pinned {
                    pinner.update(&running, &none, true, &profiles).unwrap();
                }
            }
            shared.window.store(NO_WINDOW, Ordering::Release);
            pinned_windows.push(pinned);
        }
    }
    pinner.update(&[], &none, true, &profiles).unwrap();
    shared.stop.store(true, Ordering::Release);
    let times = coordinator.join().unwrap();
    workers.into_iter().chain(noise_threads).for_each(|t| t.join().unwrap());

    let per_window: Vec<Vec<f64>> = (0..pinned_windows.len())
        .map(|w| times.iter().filter(|(id, _)| *id == w).map(|(_, t)| *t).collect())
        .collect();
    let scores: Vec<Score> = per_window.iter().map(|t| score(t)).collect();
    let gains: Vec<f64> = (0..PAIRS)
        .map(|pair| {
            let (a, b) = (pair * 2, pair * 2 + 1);
            let (p, u) = if pinned_windows[a] { (a, b) } else { (b, a) };
            scores[p].rank() / scores[u].rank()
        })
        .collect();
    let side = |pinned: bool, metric: &dyn Fn(usize) -> f64| {
        median((0..pinned_windows.len()).filter(|w| pinned_windows[*w] == pinned).map(metric).collect())
    };
    let ratio = |pinned: bool, part: &dyn Fn(&WindowStats) -> u64| {
        let windows = shared.stats.iter().enumerate().filter(|(w, _)| pinned_windows[*w] == pinned).map(|(_, s)| s);
        let (num, frames) = windows.fold((0, 0), |(n, f), s| (n + part(s), f + s.worker_frames.load(Ordering::Relaxed)));
        num as f64 / frames.max(1) as f64
    };
    let json = serde_json::json!({
        "pinned_gain": median(gains.clone()),
        "gain_min": gains.iter().copied().fold(f64::INFINITY, f64::min),
        "gain_max": gains.iter().copied().fold(0.0, f64::max),
        "pinned_fps": side(true, &|w| scores[w].fps),
        "pinned_low_fps": side(true, &|w| scores[w].low_fps),
        "unpinned_fps": side(false, &|w| scores[w].fps),
        "unpinned_low_fps": side(false, &|w| scores[w].low_fps),
        "pinned_migrations_per_kframe": 1000.0 * ratio(true, &|s| s.migrations.load(Ordering::Relaxed)),
        "unpinned_migrations_per_kframe": 1000.0 * ratio(false, &|s| s.migrations.load(Ordering::Relaxed)),
        "pinned_pcore_share": ratio(true, &|s| s.pcore_frames.load(Ordering::Relaxed)),
        "unpinned_pcore_share": ratio(false, &|s| s.pcore_frames.load(Ordering::Relaxed)),
        "pinned_cpu0_share": ratio(true, &|s| s.cpu0_frames.load(Ordering::Relaxed)),
        "min_window_frames": per_window.iter().map(Vec::len).min().unwrap_or(0),
        "min_pinned_threads": min_pinned,
        "max_unpinned_pins": max_unpinned,
        "harness_ok": 1,
    });
    println!("CACHE_BENCH_JSON={json}");
}
