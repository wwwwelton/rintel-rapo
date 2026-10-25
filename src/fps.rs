use std::collections::VecDeque;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkProfile {
    Baseline,
    Optimized,
}

impl std::fmt::Display for BenchmarkProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Baseline => write!(f, "Linha de Base (Padrão)"),
            Self::Optimized => write!(f, "Otimizado (P-cores + QoS)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    pub profile: BenchmarkProfile,
    pub duration_secs: f32,
    pub total_frames: u64,
    pub avg_fps: f32,
    pub p1_low_fps: f32,
    pub jitter_ms: f32,
    pub min_frame_time_ms: f32,
    pub max_frame_time_ms: f32,
}

pub struct BenchmarkSession {
    profile: BenchmarkProfile,
    start_time: Instant,
    max_duration_secs: Option<f32>,
    frame_times: Vec<f32>,
    pub active: bool,
}

impl BenchmarkSession {
    pub fn new() -> Self {
        Self {
            profile: BenchmarkProfile::Baseline,
            start_time: Instant::now(),
            max_duration_secs: None,
            frame_times: Vec::new(),
            active: false,
        }
    }

    pub fn start(&mut self, profile: BenchmarkProfile, max_duration_secs: Option<f32>) {
        self.profile = profile;
        self.start_time = Instant::now();
        self.max_duration_secs = max_duration_secs;
        self.frame_times.clear();
        self.active = true;
    }

    pub fn record_frame(&mut self, dt_ms: f32) {
        if !self.active || dt_ms <= 0.0 {
            return;
        }
        self.frame_times.push(dt_ms);
        if let Some(limit) = self.max_duration_secs {
            if self.start_time.elapsed().as_secs_f32() >= limit {
                self.active = false;
            }
        }
    }

    pub fn stop(&mut self) -> Option<BenchmarkReport> {
        if self.frame_times.is_empty() {
            self.active = false;
            return None;
        }
        self.active = false;
        let duration = self.start_time.elapsed().as_secs_f32().max(0.001);
        let total_frames = self.frame_times.len() as u64;

        let avg_frame_time = self.frame_times.iter().sum::<f32>() / total_frames as f32;
        let avg_fps = if avg_frame_time > 0.0 { 1000.0 / avg_frame_time } else { 0.0 };

        // Jitter (Standard Deviation of frame times in ms)
        let variance = self.frame_times.iter()
            .map(|dt| {
                let diff = dt - avg_frame_time;
                diff * diff
            })
            .sum::<f32>() / total_frames as f32;
        let jitter_ms = variance.sqrt();

        // 1% Low calculation: 99th percentile of frame time
        let mut sorted = self.frame_times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p99_idx = ((total_frames as f32 * 0.99).floor() as usize).min(sorted.len() - 1);
        let p99_dt = sorted[p99_idx];
        let p1_low_fps = if p99_dt > 0.0 { 1000.0 / p99_dt } else { 0.0 };

        let min_frame_time_ms = *sorted.first().unwrap_or(&0.0);
        let max_frame_time_ms = *sorted.last().unwrap_or(&0.0);

        Some(BenchmarkReport {
            profile: self.profile,
            duration_secs: duration,
            total_frames,
            avg_fps,
            p1_low_fps,
            jitter_ms,
            min_frame_time_ms,
            max_frame_time_ms,
        })
    }
}

pub struct FpsTracker {
    recent_frame_times: VecDeque<f32>,
    last_frame_instant: Option<Instant>,
    max_history: usize,
    pub benchmark: BenchmarkSession,
}

impl FpsTracker {
    pub fn new() -> Self {
        Self {
            recent_frame_times: VecDeque::with_capacity(120),
            last_frame_instant: None,
            max_history: 120,
            benchmark: BenchmarkSession::new(),
        }
    }

    pub fn record_frame_time(&mut self, dt_ms: f32) {
        if dt_ms <= 0.0 || dt_ms > 1000.0 {
            return;
        }
        if self.recent_frame_times.len() >= self.max_history {
            self.recent_frame_times.pop_front();
        }
        self.recent_frame_times.push_back(dt_ms);
        if self.benchmark.active {
            self.benchmark.record_frame(dt_ms);
        }
    }

    pub fn record_frame_instant(&mut self, now: Instant) {
        if let Some(prev) = self.last_frame_instant {
            let elapsed_ms = prev.elapsed().as_secs_f32() * 1000.0;
            self.record_frame_time(elapsed_ms);
        }
        self.last_frame_instant = Some(now);
    }

    pub fn current_fps(&self) -> f32 {
        if let Some(&last_dt) = self.recent_frame_times.back() {
            if last_dt > 0.0 {
                return (1000.0 / last_dt).clamp(0.0, 999.0);
            }
        }
        0.0
    }

    pub fn avg_fps(&self) -> f32 {
        if self.recent_frame_times.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.recent_frame_times.iter().sum();
        let avg_dt = sum / self.recent_frame_times.len() as f32;
        if avg_dt > 0.0 {
            (1000.0 / avg_dt).clamp(0.0, 999.0)
        } else {
            0.0
        }
    }

    pub fn p1_low_fps(&self) -> f32 {
        if self.recent_frame_times.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f32> = self.recent_frame_times.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p99_idx = ((sorted.len() as f32 * 0.99).floor() as usize).min(sorted.len() - 1);
        let p99_dt = sorted[p99_idx];
        if p99_dt > 0.0 {
            (1000.0 / p99_dt).clamp(0.0, 999.0)
        } else {
            0.0
        }
    }

    pub fn frame_times_history(&self, max_samples: usize) -> Vec<f32> {
        let count = self.recent_frame_times.len();
        let start = if count > max_samples { count - max_samples } else { 0 };
        self.recent_frame_times.iter().skip(start).copied().collect()
    }
}

/// Amostragem de frame pacing para processos gráficos em Windows
#[cfg(windows)]
pub struct WindowFrameSampler {
    last_qpc: i64,
    frequency: i64,
}

#[cfg(windows)]
impl WindowFrameSampler {
    pub fn new() -> Self {
        use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
        let mut freq = 0i64;
        let mut qpc = 0i64;
        unsafe {
            QueryPerformanceFrequency(&mut freq);
            QueryPerformanceCounter(&mut qpc);
        }
        Self {
            last_qpc: qpc,
            frequency: freq.max(1),
        }
    }

    pub fn sample_interval_ms(&mut self) -> f32 {
        use windows_sys::Win32::System::Performance::QueryPerformanceCounter;
        let mut current_qpc = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut current_qpc);
        }
        let delta = current_qpc - self.last_qpc;
        self.last_qpc = current_qpc;
        if delta > 0 && self.frequency > 0 {
            (delta as f64 * 1000.0 / self.frequency as f64) as f32
        } else {
            16.6 // Default ~60fps fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_frame_time_calculation() {
        let mut tracker = FpsTracker::new();
        // Feed 100 frames at 16.666 ms (~60 FPS) and 1 slow frame at 33.333 ms (~30 FPS)
        for _ in 0..99 {
            tracker.record_frame_time(16.666);
        }
        tracker.record_frame_time(33.333);

        let avg = tracker.avg_fps();
        let p1_low = tracker.p1_low_fps();

        assert!((avg - 59.0).abs() < 2.0, "Average FPS should be around 59-60: got {}", avg);
        assert!((p1_low - 30.0).abs() < 2.0, "1% Low FPS should be around 30: got {}", p1_low);
    }

    #[test]
    fn test_benchmark_session_summary() {
        let mut session = BenchmarkSession::new();
        session.start(BenchmarkProfile::Optimized, None);

        // 50 frames at 10 ms (100 FPS), 50 frames at 20 ms (50 FPS)
        for _ in 0..50 {
            session.record_frame(10.0);
        }
        for _ in 0..50 {
            session.record_frame(20.0);
        }

        let report = session.stop().expect("Report should be generated");
        assert_eq!(report.total_frames, 100);
        // Average frame time = 15 ms -> ~66.6 FPS
        assert!((report.avg_fps - 66.66).abs() < 1.0);
        assert_eq!(report.min_frame_time_ms, 10.0);
        assert_eq!(report.max_frame_time_ms, 20.0);
        assert!(report.jitter_ms > 0.0);
    }
}
