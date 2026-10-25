use std::collections::VecDeque;
use std::time::Instant;

pub struct FpsTracker {
    recent_frame_times: VecDeque<f32>,
    last_frame_instant: Option<Instant>,
    max_history: usize,
    p1_low_fps: f32,
}

impl FpsTracker {
    pub fn new() -> Self {
        Self {
            recent_frame_times: VecDeque::with_capacity(120),
            last_frame_instant: None,
            max_history: 120,
            p1_low_fps: 0.0,
        }
    }

    pub fn record_frame_time(&mut self, dt_ms: f32) {
        if !dt_ms.is_finite() || dt_ms <= 0.0 || dt_ms > 1000.0 {
            return;
        }
        if self.recent_frame_times.len() >= self.max_history {
            self.recent_frame_times.pop_front();
        }
        self.recent_frame_times.push_back(dt_ms);
    }

    pub fn record_frame_instant(&mut self, now: Instant) {
        if let Some(previous) = self.last_frame_instant {
            let Some(elapsed) = now.checked_duration_since(previous) else {
                self.last_frame_instant = None;
                return;
            };
            self.record_frame_time(elapsed.as_secs_f32() * 1000.0);
        }
        self.last_frame_instant = Some(now);
    }
    pub fn pause_sampling(&mut self) {
        self.last_frame_instant = None;
    }

    pub fn refresh_statistics(&mut self) {
        if self.recent_frame_times.is_empty() {
            self.p1_low_fps = 0.0;
            return;
        }
        let mut sorted: Vec<f32> = self.recent_frame_times.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p99_idx = ((sorted.len() as f32 * 0.99).floor() as usize).min(sorted.len() - 1);
        let p99_dt = sorted[p99_idx];
        self.p1_low_fps = if p99_dt > 0.0 {
            (1000.0 / p99_dt).clamp(0.0, 999.0)
        } else {
            0.0
        };
    }

    pub fn reset(&mut self) {
        self.recent_frame_times.clear();
        self.last_frame_instant = None;
        self.p1_low_fps = 0.0;
    }

    pub fn current_fps(&self) -> f32 {
        self.recent_frame_times
            .back()
            .filter(|dt| **dt > 0.0)
            .map_or(0.0, |dt| (1000.0 / dt).clamp(0.0, 999.0))
    }

    pub fn avg_fps(&self) -> f32 {
        if self.recent_frame_times.is_empty() {
            return 0.0;
        }
        let avg_dt =
            self.recent_frame_times.iter().sum::<f32>() / self.recent_frame_times.len() as f32;
        if avg_dt > 0.0 {
            (1000.0 / avg_dt).clamp(0.0, 999.0)
        } else {
            0.0
        }
    }

    pub fn p1_low_fps(&self) -> f32 {
        self.p1_low_fps
    }

    pub fn frame_times_history(&self, max_samples: usize) -> Vec<f32> {
        let count = self.recent_frame_times.len();
        let start = count.saturating_sub(max_samples);
        self.recent_frame_times
            .iter()
            .skip(start)
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_reports_recent_frame_rates() {
        let mut tracker = FpsTracker::new();
        for _ in 0..99 {
            tracker.record_frame_time(16.666);
        }
        tracker.record_frame_time(33.333);
        tracker.refresh_statistics();

        assert!((tracker.current_fps() - 30.0003).abs() < 0.1);
        assert!((tracker.avg_fps() - 59.405).abs() < 0.1);
        assert!((tracker.p1_low_fps() - 30.0003).abs() < 0.1);
    }

    #[test]
    fn tracker_ignores_invalid_frame_times() {
        let mut tracker = FpsTracker::new();
        tracker.record_frame_time(16.666);
        tracker.refresh_statistics();
        let history = tracker.frame_times_history(10);
        let current = tracker.current_fps();
        let average = tracker.avg_fps();
        let low = tracker.p1_low_fps();

        tracker.record_frame_time(0.0);
        tracker.record_frame_time(-1.0);
        tracker.record_frame_time(1001.0);
        tracker.record_frame_time(f32::NAN);
        tracker.refresh_statistics();

        assert_eq!(tracker.frame_times_history(10), history);
        assert_eq!(tracker.current_fps(), current);
        assert_eq!(tracker.avg_fps(), average);
        assert_eq!(tracker.p1_low_fps(), low);
    }

    #[test]
    fn accepts_maximum_frame_time_but_rejects_larger() {
        let mut tracker = FpsTracker::new();
        tracker.record_frame_time(1000.0);

        assert_eq!(tracker.frame_times_history(10), vec![1000.0]);
        assert!((tracker.current_fps() - 1.0).abs() < 0.1);

        tracker.record_frame_time(1000.0001);
        assert_eq!(tracker.frame_times_history(10), vec![1000.0]);
    }

    #[test]
    fn history_is_bounded_to_recent_samples() {
        let mut tracker = FpsTracker::new();
        for dt_ms in 1..=121 {
            tracker.record_frame_time(dt_ms as f32);
        }

        assert_eq!(
            tracker.frame_times_history(usize::MAX),
            (2..=121).map(|dt_ms| dt_ms as f32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pause_starts_new_sampling_baseline() {
        let mut tracker = FpsTracker::new();
        let start = Instant::now();
        tracker.record_frame_instant(start);
        tracker.record_frame_instant(start + std::time::Duration::from_millis(20));
        tracker.refresh_statistics();
        let history = tracker.frame_times_history(10);
        let current = tracker.current_fps();
        let average = tracker.avg_fps();
        let low = tracker.p1_low_fps();

        tracker.pause_sampling();
        tracker.record_frame_instant(start + std::time::Duration::from_millis(500));

        assert_eq!(tracker.frame_times_history(10), history);
        assert_eq!(tracker.current_fps(), current);
        assert_eq!(tracker.avg_fps(), average);
        assert_eq!(tracker.p1_low_fps(), low);

        tracker.record_frame_instant(start + std::time::Duration::from_millis(520));
        assert_eq!(tracker.frame_times_history(2).len(), 2);
        assert!((tracker.current_fps() - 50.0).abs() < 0.1);
    }

    #[test]
    fn instant_sampling_uses_supplied_timestamps() {
        let mut tracker = FpsTracker::new();
        let start = Instant::now();
        tracker.record_frame_instant(start);
        tracker.record_frame_instant(start + std::time::Duration::from_millis(20));

        assert_eq!(tracker.frame_times_history(1).len(), 1);
        assert!((tracker.current_fps() - 50.0).abs() < 0.1);
    }

    #[test]
    fn non_monotonic_sampling_restarts_baseline() {
        let mut tracker = FpsTracker::new();
        let start = Instant::now();
        tracker.record_frame_instant(start);
        tracker.record_frame_instant(start + std::time::Duration::from_millis(20));
        tracker.record_frame_instant(start + std::time::Duration::from_millis(10));
        tracker.record_frame_instant(start + std::time::Duration::from_millis(11));

        assert_eq!(tracker.frame_times_history(10).len(), 1);

        tracker.record_frame_instant(start + std::time::Duration::from_millis(31));
        assert_eq!(tracker.frame_times_history(10).len(), 2);
        assert!((tracker.current_fps() - 50.0).abs() < 0.1);
    }

    #[test]
    fn reset_clears_samples_and_statistics() {
        let mut tracker = FpsTracker::new();
        let start = Instant::now();
        tracker.record_frame_instant(start);
        tracker.record_frame_instant(start + std::time::Duration::from_millis(16));
        tracker.refresh_statistics();
        tracker.reset();
        tracker.record_frame_instant(start + std::time::Duration::from_millis(500));

        assert!(tracker.frame_times_history(10).is_empty());
        assert_eq!(tracker.current_fps(), 0.0);
        assert_eq!(tracker.avg_fps(), 0.0);
        assert_eq!(tracker.p1_low_fps(), 0.0);
    }
}
