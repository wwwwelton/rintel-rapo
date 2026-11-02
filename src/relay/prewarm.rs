//! Pool of pre-opened TCP connections. The relay's own handshake to the game server
//! (~11 ms) is paid ahead of time for destinations the game keeps reusing, so the first
//! bytes of a new game connection leave without waiting for it.
//!
//! Only direct (non-HY2) exits are pooled, only for destinations seen `HOT` times, at most
//! one spare per destination, and a spare older than `MAX_AGE` is dropped: servers close idle
//! sockets and a dead spare would turn into a failed connection instead of a faster one.
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const HOT: u32 = 2;
const MAX_AGE: Duration = Duration::from_secs(8);
const MAX_SPARES: usize = 16;

#[derive(Default)]
pub(super) struct Prewarm {
    seen: HashMap<SocketAddr, u32>,
    spare: HashMap<SocketAddr, (TcpStream, Instant)>,
    pub hits: u64,
    pub misses: u64,
}

impl Prewarm {
    /// Takes a live spare for `target`, if any. A spare the peer already closed is discarded.
    pub fn take(&mut self, target: SocketAddr, now: Instant) -> Option<TcpStream> {
        let (stream, made) = self.spare.remove(&target)?;
        if now.saturating_duration_since(made) > MAX_AGE || !alive(&stream) {
            self.misses += 1;
            return None;
        }
        self.hits += 1;
        Some(stream)
    }
    /// Records a real use of `target`; true when a spare should be opened for the next one.
    pub fn used(&mut self, target: SocketAddr, now: Instant) -> bool {
        let count = {
            let c = self.seen.entry(target).or_insert(0);
            *c = c.saturating_add(1);
            *c
        };
        if self.seen.len() > 128 { self.seen.retain(|_, c| *c >= HOT); }
        self.expire(now);
        count >= HOT && !self.spare.contains_key(&target) && self.spare.len() < MAX_SPARES
    }
    pub fn store(&mut self, target: SocketAddr, stream: TcpStream, now: Instant) {
        if self.spare.len() < MAX_SPARES { self.spare.insert(target, (stream, now)); }
    }
    fn expire(&mut self, now: Instant) {
        self.spare.retain(|_, (stream, made)| now.saturating_duration_since(*made) <= MAX_AGE && alive(stream));
    }
}

/// A spare is usable only if the peer neither closed it nor sent anything unexpected.
fn alive(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() { return false; }
    let mut byte = [0u8; 1];
    let open = matches!(stream.peek(&mut byte), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock);
    let _ = stream.set_nonblocking(false);
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn warms_only_hot_destinations_and_drops_dead_or_old_spares() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let now = Instant::now();
        let mut pool = Prewarm::default();
        assert!(!pool.used(target, now), "a destination seen once is not warmed");
        assert!(pool.used(target, now), "seen twice: warm one spare");

        // live spare is handed out
        pool.store(target, TcpStream::connect(target).unwrap(), now);
        let (_peer_live, _) = listener.accept().unwrap();
        assert!(!pool.used(target, now), "one spare per destination");
        assert!(pool.take(target, now).is_some());
        assert_eq!(pool.hits, 1);

        // spare older than MAX_AGE is dropped even though the peer is still open
        pool.store(target, TcpStream::connect(target).unwrap(), now);
        let (_peer_old, _) = listener.accept().unwrap();
        assert!(pool.take(target, now + MAX_AGE + Duration::from_secs(1)).is_none());

        // spare the peer already closed is dropped
        pool.store(target, TcpStream::connect(target).unwrap(), now);
        let (peer_dead, _) = listener.accept().unwrap();
        drop(peer_dead);
        std::thread::sleep(Duration::from_millis(50));
        assert!(pool.take(target, now).is_none());
        assert_eq!(pool.misses, 2);
    }
}
