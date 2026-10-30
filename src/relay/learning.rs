use super::{CONNECT_TIMEOUT, Egress};
use crate::network_learning::flow::Flow;
use serde::Serialize;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const MAX_HISTORY: usize = 128;
const FRESH: Duration = Duration::from_secs(120);
const WARMUP: usize = 6;

#[derive(Serialize)]
pub struct Prediction {
    destination: SocketAddr,
    interface: u32,
    source: std::net::IpAddr,
    gateway: String,
    samples: usize,
    failures: usize,
    observed_ms: Option<f64>,
    predicted_ms: Option<f64>,
    uncertainty_ms: f64,
    selected: bool,
}
struct History {
    target: SocketAddr,
    egress: Egress,
    model: Flow,
    mean: f64,
    deviation: f64,
    residual: f64,
    last: f64,
    previous: f64,
    attempts: usize,
    failures: usize,
    failed: bool,
    selected_at: u64,
    measured_at: Instant,
}
impl History {
    fn input(&self) -> [f64; 9] {
        let scale = CONNECT_TIMEOUT.as_secs_f64() * 1000.0;
        [self.mean / scale, self.deviation / scale, self.last / scale, self.previous / scale,
            (self.last - self.previous) / scale, self.model.samples.min(32) as f64 / 32.0,
            self.residual / scale, f64::from(self.target.is_ipv6()), 1.0]
    }
    fn prediction(&self) -> f64 { self.model.predict(&self.input()).clamp(0.0, 1.0) * CONNECT_TIMEOUT.as_secs_f64() * 1000.0 }
    fn score(&self) -> f64 {
        let confidence = self.model.samples.min(32) as f64 / 32.0;
        confidence * self.prediction() + (1.0 - confidence) * self.mean + self.residual + self.deviation
    }
}
#[derive(Default)]
pub(super) struct Learning { history: Vec<History>, requests: u64 }
impl Learning {
    pub fn choose(&mut self, target: SocketAddr, choices: &[Egress], now: Instant) -> usize {
        self.history.retain(|h| now.saturating_duration_since(h.measured_at) <= FRESH);
        self.requests = self.requests.wrapping_add(1);
        let attempts = self.history.iter().filter(|h| h.target == target).fold(0usize, |sum, h| sum.saturating_add(h.attempts));
        let history = |choice: &Egress| self.history.iter().find(|h| h.target == target && h.egress.id == choice.id);
        let selected = choices.iter().enumerate().min_by(|(_, a), (_, b)| {
            let a = history(a); let b = history(b);
            let a_attempts = a.map_or(0, |h| h.attempts);
            let b_attempts = b.map_or(0, |h| h.attempts);
            if a_attempts < 3 || b_attempts < 3 { return a_attempts.cmp(&b_attempts); }
            let a = a.unwrap(); let b = b.unwrap();
            if attempts % 16 == 15 { a.selected_at.cmp(&b.selected_at) } else { a.score().total_cmp(&b.score()) }
        }).map_or(0, |(index, _)| index);
        let index = self.entry(target, &choices[selected], now);
        self.history[index].attempts = self.history[index].attempts.saturating_add(1);
        self.history[index].selected_at = self.requests;
        selected
    }
    fn entry(&mut self, target: SocketAddr, egress: &Egress, now: Instant) -> usize {
        if let Some(index) = self.history.iter().position(|h| h.target == target && h.egress.id == egress.id) { return index; }
        if self.history.len() >= MAX_HISTORY {
            let oldest = self.history.iter().enumerate().min_by_key(|(_, h)| h.measured_at).unwrap().0;
            self.history.swap_remove(oldest);
        }
        self.history.push(History { target, egress: egress.clone(), model: Flow::new(42), mean: 0.0,
            deviation: 0.0, residual: 0.0, last: 0.0, previous: 0.0, attempts: 0, failures: 0,
            failed: false, selected_at: 0, measured_at: now });
        self.history.len() - 1
    }
    pub fn observe(&mut self, target: SocketAddr, egress: &Egress, elapsed: Duration, success: bool, now: Instant) {
        let index = self.entry(target, egress, now);
        let h = &mut self.history[index];
        let ms = if success { elapsed.min(CONNECT_TIMEOUT).as_secs_f64() * 1000.0 } else { CONNECT_TIMEOUT.as_secs_f64() * 1000.0 };
        let input = h.input();
        let prediction = h.prediction();
        if h.model.samples == 0 { h.mean = ms; h.last = ms; h.previous = ms; }
        h.model.train(&input, ms / (CONNECT_TIMEOUT.as_secs_f64() * 1000.0));
        h.residual = 0.8 * h.residual + 0.2 * (ms - prediction).abs();
        h.deviation = 0.8 * h.deviation + 0.2 * (ms - h.mean).abs();
        h.mean = 0.8 * h.mean + 0.2 * ms;
        h.previous = h.last; h.last = ms; h.measured_at = now;
        h.failed = !success;
        h.failures += usize::from(!success);
    }
    pub fn predictions(&self, now: Instant) -> Vec<Prediction> {
        let mut predictions: Vec<_> = self.history.iter().filter(|h| h.model.samples > 0 && now.saturating_duration_since(h.measured_at) <= FRESH).map(|h| {
            let selected = !self.history.iter().any(|other| other.target == h.target && other.selected_at > h.selected_at);
            Prediction { destination: h.target, interface: h.egress.interface, source: h.egress.source.ip(),
                gateway: h.egress.gateway.clone(), samples: h.model.samples, failures: h.failures,
                observed_ms: (!h.failed).then_some(h.last),
                predicted_ms: (h.model.samples >= WARMUP && !h.failed).then(|| h.prediction()),
                uncertainty_ms: h.residual + h.deviation, selected }
        }).collect();
        predictions.sort_unstable_by_key(|p| (p.destination, p.interface));
        predictions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exits() -> [Egress; 2] {
        [1, 2].map(|index| Egress { id: index.to_string(), authorization: index.to_string(), interface: index,
            source: format!("192.0.2.{index}:0").parse().unwrap(), gateway: "192.0.2.254".into() })
    }
    #[test]
    fn learns_per_destination_and_rechecks_alternatives_without_inventing_measurements() {
        let mut model = Learning::default(); let choices = exits();
        let a = "203.0.113.1:443".parse().unwrap(); let b = "203.0.113.2:443".parse().unwrap();
        let start = Instant::now();
        assert!(model.predictions(start).is_empty());
        for i in 0..200 {
            for (target, fast) in [(a, 0), (b, 1)] {
                let now = start + Duration::from_millis(i * 2 + u64::from(target == b));
                let selected = model.choose(target, &choices, now);
                model.observe(target, &choices[selected], Duration::from_millis(if selected == fast { 10 } else { 120 }), true, now);
            }
        }
        let now = start + Duration::from_secs(1);
        let mut a_choices = 0; let mut b_choices = 0;
        for _ in 0..32 {
            a_choices += usize::from(model.choose(a, &choices, now) == 0);
            b_choices += usize::from(model.choose(b, &choices, now) == 1);
        }
        assert!(a_choices >= 28 && b_choices >= 28, "destinations must not share a winner");
        assert!(model.predictions(now).iter().all(|p| p.samples >= 6 && p.predicted_ms.is_some()));
        assert_eq!(model.choose(a, &choices[1..], now), 0, "removed interfaces are ineligible");
        assert!(model.predictions(start + FRESH + Duration::from_secs(1)).is_empty());
    }
    #[test]
    fn failed_connection_is_not_a_fast_path_and_unmeasured_prediction_is_hidden() {
        let mut model = Learning::default(); let choices = exits(); let now = Instant::now();
        let target = "203.0.113.1:443".parse().unwrap();
        for _ in 0..8 {
            for (i, choice) in choices.iter().enumerate() {
                model.choose(target, std::slice::from_ref(choice), now);
                model.observe(target, choice, Duration::from_millis(1), i == 1, now);
            }
        }
        assert_eq!(model.choose(target, &choices, now), 1);
        let failed = model.predictions(now).into_iter().find(|p| p.interface == 1).unwrap();
        assert_eq!(failed.failures, 8);
        assert!(failed.observed_ms.is_none() && failed.predicted_ms.is_none());
        let mut fresh = Learning::default();
        fresh.choose(target, &choices, now);
        fresh.observe(target, &choices[0], Duration::from_millis(10), true, now);
        assert!(fresh.predictions(now)[0].predicted_ms.is_none());
    }
}
