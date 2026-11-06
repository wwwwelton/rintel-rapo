//! UDP packet redundancy (UDPspeeder style client-side replication).
//! Sends `Y` redundant duplicate datagrams for every `X` original datagrams to mitigate packet loss
//! without requiring custom server-side software.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Minimum and maximum values for X and Y in the X:Y ratio.
pub const MIN_RATIO: u32 = 1;
pub const MAX_RATIO: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedundancyConfig {
    pub enabled: bool,
    /// Number of original packets in the ratio block (default: 20).
    pub x: u32,
    /// Number of redundant packets to send per X original packets (default: 10).
    pub y: u32,
}

impl Default for RedundancyConfig {
    fn default() -> Self {
        Self { enabled: false, x: 20, y: 10 }
    }
}

impl RedundancyConfig {
    pub fn new(enabled: bool, x: u32, y: u32) -> Result<Self, &'static str> {
        let config = Self { enabled, x, y };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.x < MIN_RATIO || self.x > MAX_RATIO {
            return Err("X deve estar entre 1 e 30");
        }
        if self.y < MIN_RATIO || self.y > MAX_RATIO {
            return Err("Y deve estar entre 1 e 30");
        }
        Ok(())
    }

    /// Estimated overhead percentage (e.g. 10:5 -> 50%, 1:1 -> 100%, 1:2 -> 200%).
    pub fn overhead_pct(&self) -> u32 {
        if !self.enabled || self.x == 0 { 0 } else { ((self.y as u64 * 100) / self.x as u64) as u32 }
    }
}

/// Accumulator that tracks packet counts and determines how many duplicate sends to emit for the current datagram.
#[derive(Debug, Default)]
pub struct RedundancyEngine {
    counter: AtomicU32,
}

impl RedundancyEngine {
    pub fn new() -> Self {
        Self { counter: AtomicU32::new(0) }
    }

    /// For the next packet being transmitted, returns the total number of times the packet
    /// should be sent (1 original + N redundant duplicates).
    /// If disabled or invalid, returns 1 (original only).
    pub fn duplicates_for_next(&self, config: &RedundancyConfig) -> u32 {
        if !config.enabled || config.x == 0 || config.y == 0 {
            return 1;
        }
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let block_pos = seq % config.x;
        // Total redundant packets across the block of X packets is Y.
        // Distributed proportionally: packets from index 0..Y % X get extra duplicate if Y > X,
        // or spread evenly across the X packets using integer division and remainder.
        let base_dups = config.y / config.x;
        let remainder = config.y % config.x;
        let extra = if block_pos < remainder { 1 } else { 0 };
        1 + base_dups + extra
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_zero_or_oversized_ratios() {
        assert!(RedundancyConfig::new(true, 0, 10).is_err());
        assert!(RedundancyConfig::new(true, 10, 0).is_err());
        assert!(RedundancyConfig::new(true, 31, 10).is_err());
        assert!(RedundancyConfig::new(true, 10, 31).is_err());
        assert!(RedundancyConfig::new(true, 1, 1).is_ok());
        assert!(RedundancyConfig::new(true, 20, 10).is_ok());
    }

    #[test]
    fn overhead_calculation() {
        let disabled = RedundancyConfig { enabled: false, x: 20, y: 10 };
        assert_eq!(disabled.overhead_pct(), 0);
        let c20_10 = RedundancyConfig { enabled: true, x: 20, y: 10 };
        assert_eq!(c20_10.overhead_pct(), 50);
        let c1_1 = RedundancyConfig { enabled: true, x: 1, y: 1 };
        assert_eq!(c1_1.overhead_pct(), 100);
        let c1_2 = RedundancyConfig { enabled: true, x: 1, y: 2 };
        assert_eq!(c1_2.overhead_pct(), 200);
    }

    #[test]
    fn disabled_returns_exactly_one_send() {
        let engine = RedundancyEngine::new();
        let config = RedundancyConfig { enabled: false, x: 10, y: 5 };
        for _ in 0..100 {
            assert_eq!(engine.duplicates_for_next(&config), 1);
        }
    }

    #[test]
    fn ratio_1_to_1_doubles_every_packet() {
        let engine = RedundancyEngine::new();
        let config = RedundancyConfig { enabled: true, x: 1, y: 1 };
        for _ in 0..50 {
            assert_eq!(engine.duplicates_for_next(&config), 2); // 1 original + 1 duplicate
        }
    }

    #[test]
    fn ratio_20_to_10_yields_exactly_30_total_packets_per_block() {
        let engine = RedundancyEngine::new();
        let config = RedundancyConfig { enabled: true, x: 20, y: 10 };
        let mut total_sends = 0;
        for _ in 0..20 {
            total_sends += engine.duplicates_for_next(&config);
        }
        assert_eq!(total_sends, 30); // 20 original + 10 redundant
    }

    #[test]
    fn ratio_3_to_5_yields_eight_total_packets_per_block() {
        let engine = RedundancyEngine::new();
        let config = RedundancyConfig { enabled: true, x: 3, y: 5 };
        let mut total_sends = 0;
        for _ in 0..3 {
            total_sends += engine.duplicates_for_next(&config);
        }
        assert_eq!(total_sends, 8); // 3 original + 5 redundant
    }
}

/// One measured candidate: `sent` original datagrams, `received` distinct ones echoed back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    pub config: RedundancyConfig,
    pub sent: u32,
    pub received: u32,
}

/// Every enabled X:Y ratio with X and Y in 1..=4 (1:1, 2:1, 1:2, ...).
pub fn candidates() -> Vec<RedundancyConfig> {
    (1..=4).flat_map(|x| (1..=4).map(move |y| RedundancyConfig { enabled: true, x, y })).collect()
}

/// Best delivery wins; candidates within 0.5 points of it tie and the lowest overhead wins the tie.
pub fn select(probes: &[Probe]) -> Option<RedundancyConfig> {
    let delivery = |p: &Probe| p.received as f64 / p.sent.max(1) as f64;
    let best = probes.iter().map(delivery).fold(f64::NEG_INFINITY, f64::max);
    if !best.is_finite() { return None; }
    probes.iter()
        .filter(|p| delivery(p) >= best - 0.005)
        .min_by_key(|p| p.config.overhead_pct())
        .map(|p| p.config)
}

#[derive(Default)]
pub(super) struct RatioLearning {
    rounds: std::collections::VecDeque<Vec<Probe>>,
    current: Option<RedundancyConfig>,
    challenger: Option<RedundancyConfig>,
    missing: usize,
}

impl RatioLearning {
    pub fn observe(&mut self, probes: &[Probe]) -> Option<RedundancyConfig> {
        let configs = candidates();
        let valid = probes.len() == configs.len()
            && configs.iter().all(|config| probes.iter().filter(|p| p.config == *config).count() == 1)
            && probes.iter().all(|p| p.sent > 0 && p.received <= p.sent)
            && probes.iter().any(|p| p.received > 0);
        if !valid {
            self.missing += 1;
            self.challenger = None;
            if self.missing >= 3 { self.rounds.clear(); }
            return self.current;
        }
        self.missing = 0;
        if self.rounds.len() == 3 { self.rounds.pop_front(); }
        self.rounds.push_back(probes.to_vec());
        let estimates: Vec<_> = configs.iter().map(|config| {
            let (sent, received) = self.rounds.iter().flatten()
                .filter(|p| p.config == *config)
                .fold((0u64, 0u64), |(s, r), p| (s + p.sent as u64, r + p.received as u64));
            (*config, received as f64 / sent as f64)
        }).collect();
        let best = estimates.iter().map(|(_, rate)| *rate).fold(0.0, f64::max);
        let winner = estimates.iter().filter(|(_, rate)| *rate >= best - 0.005)
            .min_by(|(a, _), (b, _)| (a.y as u64 * b.x as u64).cmp(&(b.y as u64 * a.x as u64)))
            .map(|(config, _)| *config);
        if self.current.is_none() || winner == self.current || winner == self.challenger {
            self.current = winner;
            self.challenger = None;
        } else {
            self.challenger = winner;
        }
        self.current
    }
    pub fn is_active(&self) -> bool { self.rounds.len() == 3 }
}

#[cfg(test)]
mod learning_tests {
    use super::*;

    fn round(high_loss: bool) -> Vec<Probe> {
        candidates().into_iter().map(|config| Probe {
            config, sent: 200,
            received: if high_loss && config != (RedundancyConfig { enabled: true, x: 1, y: 4 }) { 100 } else { 200 },
        }).collect()
    }

    #[test]
    fn learned_ratio_adapts_and_preserves_choice_without_feedback() {
        let mut learning = RatioLearning::default();
        let low = RedundancyConfig { enabled: true, x: 4, y: 1 };
        let high = RedundancyConfig { enabled: true, x: 1, y: 4 };
        assert_eq!(learning.observe(&round(false)), Some(low));
        assert_eq!(learning.observe(&round(true)), Some(low));
        assert_eq!(learning.observe(&round(true)), Some(high));
        for _ in 0..3 { assert_eq!(learning.observe(&[]), Some(high)); }
        assert_eq!(learning.observe(&round(false)), Some(high));
        assert_eq!(learning.observe(&round(false)), Some(low));
    }
}

/// Sends `count` numbered datagrams to a UDP echo at `echo` using the duplication of `config`, then counts
/// the distinct sequence numbers echoed back within `wait`. Duplicates of one number count once.
pub fn probe(echo: SocketAddr, config: RedundancyConfig, count: u32, wait: Duration, running: &std::sync::atomic::AtomicBool) -> io::Result<Probe> {
    let socket = UdpSocket::bind(if echo.is_ipv4() { "127.0.0.1:0" } else { "[::1]:0" })?;
    socket.connect(echo)?;
    let engine = RedundancyEngine::new();
    let mut seen = vec![false; count as usize];
    let mut buf = [0u8; 4];
    // Echoes arrive while sending; drain them as we go, or the receive buffer overflows and drops replies.
    socket.set_nonblocking(true)?;
    for seq in 0..count {
        if !running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        for _ in 0..engine.duplicates_for_next(&config) {
            socket.send(&seq.to_be_bytes())?;
        }
        while let Ok(4) = socket.recv(&mut buf) {
            if let Some(slot) = seen.get_mut(u32::from_be_bytes(buf) as usize) { *slot = true; }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    socket.set_nonblocking(false)?;
    let deadline = Instant::now() + wait;
    loop {
        if !running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() { break; }
        socket.set_read_timeout(Some(left))?;
        match socket.recv(&mut buf) {
            Ok(4) => if let Some(slot) = seen.get_mut(u32::from_be_bytes(buf) as usize) { *slot = true; },
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(Probe { config, sent: count, received: seen.iter().filter(|s| **s).count() as u32 })
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    /// Echo that drops datagrams with a fixed pseudo-random pattern (`drop_per_mille` out of 1000).
    fn lossy_echo(drop_per_mille: u32) -> (SocketAddr, Arc<AtomicBool>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let addr = socket.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            let mut state: u32 = 0x2545_F491;
            let mut buf = [0u8; 16];
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                if (state >> 16) % 1000 >= drop_per_mille { socket.send_to(&buf[..n], from).unwrap(); }
            }
        });
        (addr, stop)
    }

    #[test]
    fn without_loss_the_lowest_overhead_candidate_wins() {
        let (addr, stop) = lossy_echo(0);
        let probes: Vec<Probe> = candidates().into_iter()
            .map(|c| probe(addr, c, 200, Duration::from_millis(300), &AtomicBool::new(true)).unwrap()).collect();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(probes.iter().all(|p| p.received == p.sent));
        assert_eq!(select(&probes), Some(RedundancyConfig { enabled: true, x: 4, y: 1 }));
    }

    #[test]
    fn under_loss_redundancy_raises_delivery_and_is_selected() {
        let (addr, stop) = lossy_echo(300);
        let single = probe(addr, RedundancyConfig { enabled: true, x: 4, y: 1 }, 400, Duration::from_millis(400), &AtomicBool::new(true)).unwrap();
        let triple = probe(addr, RedundancyConfig { enabled: true, x: 1, y: 2 }, 400, Duration::from_millis(400), &AtomicBool::new(true)).unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(triple.received > single.received, "1:2 must recover more loss than 4:1");
        let chosen = select(&[single, triple]).unwrap();
        assert_eq!(chosen, triple.config);
    }

    #[test]
    fn select_ignores_empty_probes() {
        assert_eq!(select(&[]), None);
    }
}
