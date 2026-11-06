use std::time::{Duration, Instant};

const MAX_DESTINATIONS: usize = 4;
const TIMEOUT_MS: u32 = 250;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoOutcome {
    Reply(u32),
    ReplyTrace { ms: u32, hops: u8 },
    Timeout,
    Unavailable(u32),
    TtlExpired(u32),
}

impl EchoOutcome {
    pub fn label(&self) -> String {
        match self {
            Self::Reply(0) => "<1 ms".into(),
            Self::Reply(ms) => format!("{ms} ms"),
            Self::ReplyTrace { ms: 0, hops } => format!("<1 ms (traceback: {hops} saltos)"),
            Self::ReplyTrace { ms, hops } => format!("{ms} ms (traceback: {hops} saltos)"),
            Self::Timeout => format!("Timeout ({TIMEOUT_MS} ms; ICMP pode estar bloqueado)"),
            Self::Unavailable(code) => format!("Indisponível (Windows/IP {code})"),
            Self::TtlExpired(ms) => format!("TTL expirado ({ms} ms)"),
        }
    }

    pub fn round_trip_ms(&self) -> Option<u32> {
        match self {
            Self::Reply(ms) | Self::ReplyTrace { ms, .. } => Some(*ms),
            _ => None,
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) fn from_status(status: u32, elapsed_ms: u32, hops: Option<u8>) -> Self {
        match status {
            0 => match hops {
                Some(h) => Self::ReplyTrace { ms: elapsed_ms, hops: h },
                None => Self::Reply(elapsed_ms),
            },
            11010 | 1460 => Self::Timeout,
            11013 | 11014 => Self::TtlExpired(elapsed_ms),
            code => Self::Unavailable(code),
        }
    }
}

impl Default for EchoOutcome {
    fn default() -> Self {
        Self::Timeout
    }
}

pub const MAX_TRACE_HOPS: u8 = 16;
pub const TRACE_HOP_TIMEOUT_MS: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopOutcome {
    Reached { rtt_ms: u32, ttl: u8 },
    TtlExpired { intermediate_rtt_ms: u32 },
    Timeout,
    Unavailable(u32),
}

impl HopOutcome {
    pub fn is_destination_reached(&self) -> bool {
        matches!(self, Self::Reached { .. })
    }

    pub fn destination_rtt(&self) -> Option<u32> {
        match self {
            Self::Reached { rtt_ms, .. } => Some(*rtt_ms),
            _ => None,
        }
    }
}

pub fn classify_hop_response(
    status: u32,
    rtt_ms: u32,
    ttl: u8,
    responder_ip: Option<std::net::IpAddr>,
    destination_ip: std::net::IpAddr,
) -> HopOutcome {
    const IP_SUCCESS: u32 = 0;
    const IP_REQ_TIMED_OUT: u32 = 11010;
    const ERROR_SEM_TIMEOUT: u32 = 1460;
    const IP_TTL_EXPIRED_TRANSIT: u32 = 11013;
    const IP_TTL_EXPIRED_REASSEM: u32 = 11014;

    match status {
        IP_SUCCESS => {
            if responder_ip.is_none_or(|resp| resp == destination_ip) {
                HopOutcome::Reached { rtt_ms, ttl }
            } else {
                HopOutcome::TtlExpired { intermediate_rtt_ms: rtt_ms }
            }
        }
        IP_TTL_EXPIRED_TRANSIT | IP_TTL_EXPIRED_REASSEM => {
            HopOutcome::TtlExpired { intermediate_rtt_ms: rtt_ms }
        }
        IP_REQ_TIMED_OUT | ERROR_SEM_TIMEOUT => HopOutcome::Timeout,
        code => HopOutcome::Unavailable(code),
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopTrace {
    pub ttl: u8,
    pub responder: Option<String>,
    pub outcome: HopOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TracerouteOutcome {
    pub hops: Vec<HopTrace>,
    pub reached: bool,
    pub final_ttl: Option<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct EndpointPing {
    pub destination: String,
    pub outcome: EchoOutcome,
    pub route: Option<crate::routes::Observation>,
    pub hops: Option<u8>,
    pub trace: Option<TracerouteOutcome>,
}

impl EndpointPing {
    pub fn new(destination: impl Into<String>, outcome: EchoOutcome) -> Self {
        Self {
            destination: destination.into(),
            outcome,
            route: None,
            hops: None,
            trace: None,
        }
    }

    pub fn with_trace(mut self, hops: Option<u8>, trace: Option<TracerouteOutcome>) -> Self {
        self.hops = hops;
        self.trace = trace;
        self
    }

    pub fn final_rtt_ms(&self) -> Option<u32> {
        self.outcome.round_trip_ms()
    }
}

#[derive(Debug, Clone)]
pub struct ProcessPing {
    pub summary: String,
    pub endpoints: Vec<EndpointPing>,
    pub sample_id: u64,
    pub sampled_at: Option<Instant>,
    pub pids: Vec<u32>,
}

impl Default for ProcessPing {
    fn default() -> Self {
        Self { summary: "Aguardando processo / amostra ICMP".into(), endpoints: Vec::new(), sample_id: 0, sampled_at: None, pids: Vec::new() }
    }
}

#[derive(Default)]
pub(crate) struct ProcessPingSampler {
    pids: Vec<u32>,
    sampled_at: Option<Instant>,
    snapshot: ProcessPing,
}

impl ProcessPingSampler {
    pub(crate) fn sample(&mut self, pids: &[u32]) -> ProcessPing {
        let now = Instant::now();
        if self.pids != pids {
            self.pids.clear();
            self.pids.extend_from_slice(pids);
            self.sampled_at = None;
            self.snapshot = ProcessPing::default();
        }
        if pids.is_empty() {
            self.snapshot = ProcessPing { summary: "Nenhum processo selecionado".into(), ..ProcessPing::default() };
        } else if self.sampled_at.is_none_or(|at| now.duration_since(at) >= SAMPLE_INTERVAL) {
            self.snapshot = collect(pids);
            static NEXT_SAMPLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            self.snapshot.sample_id = NEXT_SAMPLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.snapshot.sampled_at = Some(now);
            self.snapshot.pids = pids.to_vec();
            self.sampled_at = Some(Instant::now());
        }
        self.snapshot.clone()
    }
}

#[cfg(not(windows))]
fn collect(_: &[u32]) -> ProcessPing {
    ProcessPing { summary: "ICMP por processo disponível somente no Windows".into(), ..ProcessPing::default() }
}

#[cfg(not(windows))]
pub(crate) fn trace_destination(_: Destination) -> (EchoOutcome, Option<u8>) {
    (EchoOutcome::Unavailable(1), None)
}

#[cfg(not(windows))]
fn echo(_: Destination) -> EchoOutcome {
    EchoOutcome::Unavailable(1)
}

#[cfg(not(windows))]
pub(crate) fn round_trip_ms(_: std::net::Ipv4Addr) -> Option<u32> {
    None
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Destination {
    pub(crate) remote: std::net::SocketAddr,
    pub(crate) local: std::net::SocketAddr,
}

#[cfg(any(windows, test))]
fn select_destinations(destinations: &mut Vec<Destination>) -> usize {
    destinations.retain(|destination| !destination.remote.ip().is_loopback());
    // One probe per IP/scope, not per TCP port or connection; ordering is deterministic.
    destinations.sort_unstable_by_key(|destination| (destination.remote, destination.local));
    destinations.dedup_by_key(|destination| destination.remote);
    let omitted = destinations.len().saturating_sub(MAX_DESTINATIONS);
    destinations.truncate(MAX_DESTINATIONS);
    omitted
}

#[cfg(windows)]
fn collect(pids: &[u32]) -> ProcessPing {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
    use windows_sys::Win32::{NetworkManagement::IpHelper::*, Networking::WinSock::{AF_INET, AF_INET6}};
    let mut destinations = Vec::new();
    let mut errors = Vec::new();
    let v4 = unsafe {
        visit_tcp_rows::<MIB_TCPROW_OWNER_PID>(AF_INET as u32, |row| {
            if row.dwState == MIB_TCP_STATE_ESTAB as u32 && pids.contains(&row.dwOwningPid) {
                let remote = Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes());
                if !remote.is_unspecified() && !remote.is_multicast() && remote != Ipv4Addr::BROADCAST {
                    destinations.push(Destination {
                        remote: SocketAddr::from((remote, 0)),
                        local: SocketAddr::from((Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()), 0)),
                    });
                }
            }
        })
    };
    let v6 = unsafe {
        visit_tcp_rows::<MIB_TCP6ROW_OWNER_PID>(AF_INET6 as u32, |row| {
            if row.dwState == MIB_TCP_STATE_ESTAB as u32 && pids.contains(&row.dwOwningPid) {
                let remote = Ipv6Addr::from(row.ucRemoteAddr);
                if !remote.is_unspecified() && !remote.is_multicast() {
                    destinations.push(Destination {
                        remote: SocketAddr::V6(SocketAddrV6::new(remote, 0, 0, row.dwRemoteScopeId)),
                        local: SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(row.ucLocalAddr), 0, 0, row.dwLocalScopeId)),
                    });
                }
            }
        })
    };
    for (family, result) in [("IPv4", v4), ("IPv6", v6)] {
        if let Err(code) = result { errors.push(format!("{family} indisponível (Windows {code})")); }
    }
    let omitted = select_destinations(&mut destinations);
    let mut summary = if destinations.is_empty() {
        "Nenhum IP TCP estabelecido observado para os PIDs selecionados".into()
    } else {
        format!("{} IP(s) TCP observado(s); até {MAX_DESTINATIONS} sondas, {TIMEOUT_MS} ms cada; intervalo 3 s", destinations.len() + omitted)
    };
    if omitted > 0 { summary.push_str(&format!("; {omitted} IP(s) não sondado(s)")); }
    for error in errors { summary.push_str(&format!("; {error}")); }
    let endpoints = destinations.into_iter().map(|destination| {
        let address = match destination.remote {
            SocketAddr::V4(addr) => addr.ip().to_string(),
            SocketAddr::V6(addr) if addr.scope_id() != 0 => format!("{}%{}", addr.ip(), addr.scope_id()),
            SocketAddr::V6(addr) => addr.ip().to_string(),
        };
        let before = crate::routes::native::observe(destination.remote).ok();
        let probe = Destination { local: before.as_ref().map_or(destination.local, |(_, source)| *source), ..destination };
        let (outcome, hops) = trace_destination(probe);
        let after = crate::routes::native::observe(destination.remote).ok();
        let route = before.and_then(|(route, _)| after.filter(|(other, _)| *other == route).map(|_| route));
        EndpointPing { destination: address, outcome, route, hops, trace: None }
    }).collect();
    ProcessPing { summary, endpoints, ..ProcessPing::default() }
}

// T must be the OWNER_PID row corresponding to family. Windows can grow the table between calls.
#[cfg(windows)]
unsafe fn visit_tcp_rows<T: Copy>(family: u32, mut visit: impl FnMut(T)) -> Result<(), u32> {
    use windows_sys::Win32::{Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA}, NetworkManagement::IpHelper::*};
    let mut size = 0;
    let mut status = GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, family, TCP_TABLE_OWNER_PID_CONNECTIONS, 0);
    for _ in 0..3 {
        if status != ERROR_INSUFFICIENT_BUFFER { return Err(status); }
        // shortcut: cap the native table at 16 MiB; revisit only for unusually large connection tables.
        if size < 4 || size > 16 * 1024 * 1024 { return Err(ERROR_INVALID_DATA); }
        let mut buffer = vec![0u32; (size as usize + 3) / 4];
        let capacity = buffer.len() * 4;
        size = capacity as u32;
        status = GetExtendedTcpTable(buffer.as_mut_ptr().cast(), &mut size, 0, family, TCP_TABLE_OWNER_PID_CONNECTIONS, 0);
        if status == ERROR_INSUFFICIENT_BUFFER { continue; }
        if status != 0 { return Err(status); }
        if size as usize > capacity || size < 4 { return Err(ERROR_INVALID_DATA); }
        let count = buffer[0] as usize;
        let row_size = std::mem::size_of::<T>();
        if count > (size as usize - 4) / row_size { return Err(ERROR_INVALID_DATA); }
        for index in 0..count {
            let row = buffer.as_ptr().cast::<u8>().add(4 + index * row_size).cast::<T>().read_unaligned();
            visit(row);
        }
        return Ok(());
    }
    Err(status)
}

#[cfg(windows)]
unsafe fn send_probe(
    handle: windows_sys::Win32::Foundation::HANDLE,
    destination: Destination,
    ttl: Option<u8>,
    timeout_ms: u32,
) -> HopOutcome {
    use std::net::SocketAddr;
    use windows_sys::Win32::{
        Foundation::GetLastError,
        NetworkManagement::IpHelper::*,
        Networking::WinSock::{AF_INET6, SOCKADDR_IN6},
    };

    let payload = *b"RAPO";
    let mut reply = [0u64; 32];
    let reply_size = std::mem::size_of_val(&reply) as u32;

    let options = ttl.map(|t| IP_OPTION_INFORMATION {
        Ttl: t,
        Tos: 0,
        Flags: 0,
        OptionsSize: 0,
        OptionsData: std::ptr::null_mut(),
    });
    let options_ptr = options.as_ref().map_or(std::ptr::null(), |o| o as *const _);

    let count = match (destination.remote, destination.local) {
        (SocketAddr::V4(remote), _) => IcmpSendEcho(
            handle,
            u32::from_ne_bytes(remote.ip().octets()),
            payload.as_ptr().cast(),
            payload.len() as u16,
            options_ptr,
            reply.as_mut_ptr().cast(),
            reply_size,
            timeout_ms,
        ),
        (SocketAddr::V6(remote), SocketAddr::V6(local)) => {
            let mut source: SOCKADDR_IN6 = std::mem::zeroed();
            source.sin6_family = AF_INET6;
            source.sin6_addr.u.Byte = local.ip().octets();
            source.Anonymous.sin6_scope_id = local.scope_id();
            let mut target: SOCKADDR_IN6 = std::mem::zeroed();
            target.sin6_family = AF_INET6;
            target.sin6_addr.u.Byte = remote.ip().octets();
            target.Anonymous.sin6_scope_id = remote.scope_id();
            Icmp6SendEcho2(
                handle,
                std::ptr::null_mut(),
                None,
                std::ptr::null(),
                &source,
                &target,
                payload.as_ptr().cast(),
                payload.len() as u16,
                options_ptr,
                reply.as_mut_ptr().cast(),
                reply_size,
                timeout_ms,
            )
        }
        _ => return HopOutcome::Unavailable(1),
    };

    let current_ttl = ttl.unwrap_or(255);
    if count == 0 {
        let code = GetLastError();
        classify_hop_response(code, 0, current_ttl, None, destination.remote.ip())
    } else if destination.remote.is_ipv4() {
        let response = &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>();
        let responder_ip = std::net::IpAddr::V4(std::net::Ipv4Addr::from(response.Address.to_ne_bytes()));
        classify_hop_response(response.Status, response.RoundTripTime, current_ttl, Some(responder_ip), destination.remote.ip())
    } else {
        let response = &*reply.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>();
        let segs = response.Address.sin6_addr;
        let mut octets = [0u8; 16];
        for (i, seg) in segs.iter().enumerate() {
            let b = seg.to_be_bytes();
            octets[i * 2] = b[0];
            octets[i * 2 + 1] = b[1];
        }
        let responder_ip = std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets));
        classify_hop_response(response.Status, response.RoundTripTime, current_ttl, Some(responder_ip), destination.remote.ip())
    }
}

/// Bounded traceroute stepping TTL up to MAX_TRACE_HOPS using native Windows ICMP.
/// Invariant: reports final destination RTT only, never intermediate router RTT as final ping.
#[cfg(windows)]
pub(crate) fn trace_destination(destination: Destination) -> (EchoOutcome, Option<u8>) {
    use windows_sys::Win32::{
        Foundation::{GetLastError, INVALID_HANDLE_VALUE},
        NetworkManagement::IpHelper::*,
    };

    unsafe {
        let handle = if destination.remote.is_ipv4() { IcmpCreateFile() } else { Icmp6CreateFile() };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return (EchoOutcome::Unavailable(GetLastError()), None);
        }

        // Direct probe first with standard timeout to confirm if destination responds to ICMP.
        // If the destination blocks ICMP or is unreachable, return immediately without wasting 16 hop probes.
        let direct = send_probe(handle, destination, None, TIMEOUT_MS);
        let direct_ms = match direct {
            HopOutcome::Reached { rtt_ms, .. } => rtt_ms,
            HopOutcome::Timeout => {
                IcmpCloseHandle(handle);
                return (EchoOutcome::Timeout, None);
            }
            HopOutcome::Unavailable(code) => {
                IcmpCloseHandle(handle);
                return (EchoOutcome::Unavailable(code), None);
            }
            HopOutcome::TtlExpired { .. } => {
                IcmpCloseHandle(handle);
                return (EchoOutcome::Timeout, None);
            }
        };

        // Destination is verified reachable. Run bounded traceroute stepping TTL to find hop distance.
        // Intermediate router RTTs are strictly ignored as final ping; only final destination RTT is reported.
        let mut traceback_hops = None;
        let mut final_rtt = direct_ms;

        for ttl in 1..=MAX_TRACE_HOPS {
            let hop_result = send_probe(handle, destination, Some(ttl), TRACE_HOP_TIMEOUT_MS);
            match hop_result {
                HopOutcome::Reached { rtt_ms, ttl: reached_ttl } => {
                    traceback_hops = Some(reached_ttl);
                    final_rtt = rtt_ms;
                    break;
                }
                HopOutcome::TtlExpired { .. } => {
                    // Intermediate router responded: never report intermediate RTT as final ping.
                    continue;
                }
                HopOutcome::Timeout => {
                    // Intermediate router dropped ICMP: continue stepping TTL.
                    continue;
                }
                HopOutcome::Unavailable(_) => {
                    // Probe error on this hop: break and rely on confirmed direct reply.
                    break;
                }
            }
        }

        IcmpCloseHandle(handle);

        match traceback_hops {
            Some(hops) => (EchoOutcome::ReplyTrace { ms: final_rtt, hops }, Some(hops)),
            None => (EchoOutcome::Reply(final_rtt), None),
        }
    }
}

#[cfg(windows)]
fn echo(destination: Destination) -> EchoOutcome {
    trace_destination(destination).0
}

/// One ICMP round trip to an IPv4 address (the HY2 server); `None` on timeout or error.
/// Benefits from destination traceroute without regression.
#[cfg(windows)]
pub(crate) fn round_trip_ms(ip: std::net::Ipv4Addr) -> Option<u32> {
    let remote = std::net::SocketAddr::from((ip, 0));
    let (outcome, _) = trace_destination(Destination { remote, local: remote });
    outcome.round_trip_ms().map(|ms| ms.max(1))
}


/// Headline "game ping": the lowest ICMP reply among the IPs the game process holds TCP connections to.
/// It is the closest thing Windows exposes to the game's latency, but it is not proof of the match server
/// (UDP flows are invisible in the TCP table and ICMP may be filtered), so every non-measurement says why.
/// Returns `"<headline> · <detail>"`, the shape the dashboard splits into value and caption.
pub fn game_ping_label(has_target: bool, ping: &ProcessPing) -> String {
    if !has_target { return "sem jogo · Nenhum jogo Steam em execução; nada a medir".into(); }
    let replies: Vec<(u32, &str)> = ping.endpoints.iter()
        .filter_map(|e| e.outcome.round_trip_ms().map(|ms| (ms, e.destination.as_str())))
        .collect();
    let Some(&(ms, ip)) = replies.iter().min_by_key(|(ms, _)| *ms) else {
        return if ping.endpoints.is_empty() {
            "sem medição · O jogo não tem conexão TCP estabelecida; UDP não expõe destino na tabela do Windows".into()
        } else {
            "sem resposta · Os IPs TCP do jogo não responderam ao ICMP (podem bloquear ping)".into()
        };
    };
    let endpoint = ping.endpoints.iter().find(|e| e.destination == ip);
    let value = endpoint.map_or_else(
        || EchoOutcome::Reply(ms).label(),
        |e| {
            if let Some(h) = e.hops {
                if matches!(e.outcome, EchoOutcome::Reply(_)) {
                    return EchoOutcome::ReplyTrace { ms, hops: h }.label();
                }
            }
            e.outcome.label()
        },
    );
    format!("{value} · Traceback/ICMP ao IP TCP do jogo {ip} · {} de {} responderam; não prova ser o servidor da partida", replies.len(), ping.endpoints.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(remote: &str) -> Destination {
        Destination { remote: remote.parse().unwrap(), local: "127.0.0.1:0".parse().unwrap() }
    }

    #[test]
    fn selection_is_unique_deterministic_and_bounded() {
        let mut destinations = vec![destination("127.0.0.1:0"), destination("[::1]:0"),
            destination("10.0.0.6:0"), destination("10.0.0.1:0"), destination("10.0.0.3:0"),
            destination("10.0.0.2:0"), destination("10.0.0.1:0"), destination("10.0.0.4:0"), destination("10.0.0.5:0")];
        assert_eq!(select_destinations(&mut destinations), 2);
        assert_eq!(destinations.iter().map(|d| d.remote.to_string()).collect::<Vec<_>>(),
            ["10.0.0.1:0", "10.0.0.2:0", "10.0.0.3:0", "10.0.0.4:0"]);
        let mut empty = Vec::new();
        assert_eq!(select_destinations(&mut empty), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn timeout_and_unavailable_never_render_as_zero_ping() {
        assert_eq!(EchoOutcome::from_status(11010, 0, None), EchoOutcome::Timeout);
        assert_eq!(EchoOutcome::from_status(5, 0, None), EchoOutcome::Unavailable(5));
        assert!(EchoOutcome::Timeout.label().starts_with("Timeout"));
        assert!(EchoOutcome::Unavailable(5).label().contains("Indisponível"));
        assert_eq!(EchoOutcome::Reply(0).label(), "<1 ms");
        assert_eq!(EchoOutcome::ReplyTrace { ms: 14, hops: 8 }.label(), "14 ms (traceback: 8 saltos)");
        assert_eq!(EchoOutcome::from_status(11013, 15, None), EchoOutcome::TtlExpired(15));
        assert_eq!(EchoOutcome::TtlExpired(15).label(), "TTL expirado (15 ms)");
        assert_eq!(EchoOutcome::TtlExpired(15).round_trip_ms(), None);
    }

    #[test]
    fn game_ping_is_the_best_reply_and_every_non_measurement_says_why() {
        let endpoint = |destination: &str, outcome| EndpointPing { destination: destination.into(), outcome, route: None, hops: None, trace: None };
        let sample = |endpoints| ProcessPing { endpoints, ..ProcessPing::default() };
        let mixed = game_ping_label(true, &sample(vec![
            endpoint("10.0.0.1", EchoOutcome::ReplyTrace { ms: 40, hops: 9 }),
            endpoint("10.0.0.2", EchoOutcome::Timeout),
            endpoint("10.0.0.3", EchoOutcome::ReplyTrace { ms: 12, hops: 7 }),
            endpoint("10.0.0.4", EchoOutcome::Unavailable(5)),
        ]));
        assert!(mixed.starts_with("12 ms (traceback: 7 saltos) · ") && mixed.contains("10.0.0.3") && mixed.contains("2 de 4 responderam"), "{mixed}");
        assert!(game_ping_label(true, &sample(vec![endpoint("10.0.0.1", EchoOutcome::Reply(0))])).starts_with("<1 ms · "), "sub-millisecond is not rounded to a fake number");
        let silent = game_ping_label(true, &sample(vec![endpoint("10.0.0.1", EchoOutcome::Timeout)]));
        assert!(silent.starts_with("sem resposta · ") && !silent.contains(" ms ·"), "timeouts never become a number: {silent}");
        assert!(game_ping_label(true, &sample(vec![])).starts_with("sem medição · "), "a game with no TCP flow has nothing to ping");
        let none = game_ping_label(false, &sample(vec![endpoint("10.0.0.1", EchoOutcome::Reply(9))]));
        assert!(none.starts_with("sem jogo · "), "without a target stale endpoints must not be shown as the game's ping: {none}");
        let with_hops = EndpointPing::new("10.0.0.5", EchoOutcome::Reply(16)).with_trace(Some(5), None);
        assert_eq!(with_hops.final_rtt_ms(), Some(16));
        let trace_sample = sample(vec![with_hops]);
        let trace_label = game_ping_label(true, &trace_sample);
        assert!(trace_label.starts_with("16 ms (traceback: 5 saltos) · "), "{trace_label}");
    }


    #[test]
    fn clearing_target_clears_previous_endpoints_immediately() {
        let mut sampler = ProcessPingSampler {
            pids: vec![42], sampled_at: Some(Instant::now()),
            snapshot: ProcessPing { summary: "Anterior".into(), endpoints: vec![EndpointPing {
                destination: "10.0.0.1".into(), outcome: EchoOutcome::Reply(12), route: None, hops: None, trace: None,
            }], ..ProcessPing::default() },
        };
        let sample = sampler.sample(&[]);
        assert!(sample.endpoints.is_empty());
        assert_eq!(sample.summary, "Nenhum processo selecionado");
    }

    #[cfg(not(windows))]
    #[test]
    fn changing_pids_does_not_reuse_previous_available_result() {
        let mut sampler = ProcessPingSampler {
            pids: vec![42], sampled_at: Some(Instant::now()),
            snapshot: ProcessPing { summary: "Anterior".into(), endpoints: vec![EndpointPing {
                destination: "192.0.2.42".into(), outcome: EchoOutcome::Reply(42), route: None, hops: None, trace: None,
            }], ..ProcessPing::default() },
        };
        let sample = sampler.sample(&[99]);
        assert!(sample.endpoints.is_empty());
        assert_eq!(sample.summary, "ICMP por processo disponível somente no Windows");
    }

    #[test]
    fn hop_outcome_classification_distinguishes_reached_expired_timeout_and_error() {
        let dest_v4: std::net::IpAddr = "198.51.100.1".parse().unwrap();
        let other_v4: std::net::IpAddr = "198.51.100.254".parse().unwrap();

        // Status 0 + responder matches destination => Reached
        let reached = classify_hop_response(0, 15, 5, Some(dest_v4), dest_v4);
        assert_eq!(reached, HopOutcome::Reached { rtt_ms: 15, ttl: 5 });
        assert!(reached.is_destination_reached());
        assert_eq!(reached.destination_rtt(), Some(15));

        // Status 0 + no explicit responder => assumed destination Reached
        let implicit = classify_hop_response(0, 8, 3, None, dest_v4);
        assert_eq!(implicit, HopOutcome::Reached { rtt_ms: 8, ttl: 3 });
        assert!(implicit.is_destination_reached());
        assert_eq!(implicit.destination_rtt(), Some(8));

        // Status 0 + mismatched responder (intermediate router) => TtlExpired, never destination ping
        let mismatched = classify_hop_response(0, 4, 2, Some(other_v4), dest_v4);
        assert_eq!(mismatched, HopOutcome::TtlExpired { intermediate_rtt_ms: 4 });
        assert!(!mismatched.is_destination_reached());
        assert_eq!(mismatched.destination_rtt(), None);

        // Status 11013 (IP_TTL_EXPIRED_TRANSIT) => TtlExpired
        let expired_transit = classify_hop_response(11013, 10, 4, Some(other_v4), dest_v4);
        assert_eq!(expired_transit, HopOutcome::TtlExpired { intermediate_rtt_ms: 10 });
        assert!(!expired_transit.is_destination_reached());
        assert_eq!(expired_transit.destination_rtt(), None);

        // Status 11014 (IP_TTL_EXPIRED_REASSEM) => TtlExpired
        let expired_reassem = classify_hop_response(11014, 12, 6, Some(other_v4), dest_v4);
        assert_eq!(expired_reassem, HopOutcome::TtlExpired { intermediate_rtt_ms: 12 });
        assert!(!expired_reassem.is_destination_reached());

        // Status 11010 (IP_REQ_TIMED_OUT) and 1460 (ERROR_SEM_TIMEOUT) => Timeout
        let timeout1 = classify_hop_response(11010, 0, 7, None, dest_v4);
        assert_eq!(timeout1, HopOutcome::Timeout);
        assert!(!timeout1.is_destination_reached());
        assert_eq!(timeout1.destination_rtt(), None);

        let timeout2 = classify_hop_response(1460, 0, 8, None, dest_v4);
        assert_eq!(timeout2, HopOutcome::Timeout);

        // Other Windows error codes (e.g. 11003 host unreachable, 5 access denied) => Unavailable
        let unreach = classify_hop_response(11003, 0, 9, Some(other_v4), dest_v4);
        assert_eq!(unreach, HopOutcome::Unavailable(11003));
        assert!(!unreach.is_destination_reached());
        assert_eq!(unreach.destination_rtt(), None);

        let denied = classify_hop_response(5, 0, 10, None, dest_v4);
        assert_eq!(denied, HopOutcome::Unavailable(5));
    }

    #[test]
    fn destination_check_never_reports_intermediate_router_as_final_ping() {
        let dest_v6: std::net::IpAddr = "2001:db8::cafe".parse().unwrap();
        let hop_v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();

        // Intermediate router responds with ICMP Echo (status 0) along the path
        let intermediate_echo = classify_hop_response(0, 3, 2, Some(hop_v6), dest_v6);
        assert_eq!(intermediate_echo, HopOutcome::TtlExpired { intermediate_rtt_ms: 3 });
        assert!(!intermediate_echo.is_destination_reached());
        assert_eq!(intermediate_echo.destination_rtt(), None, "intermediate RTT must never leak into destination ping");

        // Target responds with ICMP Echo (status 0)
        let target_echo = classify_hop_response(0, 22, 6, Some(dest_v6), dest_v6);
        assert_eq!(target_echo, HopOutcome::Reached { rtt_ms: 22, ttl: 6 });
        assert!(target_echo.is_destination_reached());
        assert_eq!(target_echo.destination_rtt(), Some(22));
    }

    #[test]
    fn ttl_step_is_bounded_and_preserves_destination_only_measurement() {
        assert_eq!(MAX_TRACE_HOPS, 16);
        assert_eq!(TRACE_HOP_TIMEOUT_MS, 80);

        let dest: std::net::IpAddr = "192.0.2.10".parse().unwrap();
        let router: std::net::IpAddr = "192.0.2.1".parse().unwrap();

        // Simulate TTL stepping loop over 1..=MAX_TRACE_HOPS:
        // Hops 1..4: TTL expired in transit (router responds)
        // Hop 5: Destination reached (18 ms)
        let mut final_rtt = None;
        let mut traceback_hops = None;

        for ttl in 1..=MAX_TRACE_HOPS {
            let hop = if ttl < 5 {
                classify_hop_response(11013, 2 * ttl as u32, ttl, Some(router), dest)
            } else {
                classify_hop_response(0, 18, ttl, Some(dest), dest)
            };

            match hop {
                HopOutcome::Reached { rtt_ms, ttl: reached_ttl } => {
                    traceback_hops = Some(reached_ttl);
                    final_rtt = Some(rtt_ms);
                    break;
                }
                HopOutcome::TtlExpired { intermediate_rtt_ms } => {
                    // Intermediate router RTTs (e.g. 2, 4, 6, 8 ms) are strictly ignored
                    assert_ne!(Some(intermediate_rtt_ms), final_rtt);
                    continue;
                }
                _ => continue,
            }
        }

        assert_eq!(traceback_hops, Some(5));
        assert_eq!(final_rtt, Some(18));
        let outcome = EchoOutcome::ReplyTrace { ms: final_rtt.unwrap(), hops: traceback_hops.unwrap() };
        assert_eq!(outcome.label(), "18 ms (traceback: 5 saltos)");
        assert_eq!(outcome.round_trip_ms(), Some(18));

        // When destination is never reached within MAX_TRACE_HOPS:
        let mut unreached_hops = None;
        for ttl in 1..=MAX_TRACE_HOPS {
            let hop = classify_hop_response(11013, 3, ttl, Some(router), dest);
            if let HopOutcome::Reached { ttl: reached_ttl, .. } = hop {
                unreached_hops = Some(reached_ttl);
                break;
            }
        }
        assert_eq!(unreached_hops, None, "must not invent traceback hops if destination was not reached");
    }

    #[test]
    fn round_trip_ms_preserves_reply_clamps_sub_millisecond_and_returns_none_on_failure() {
        let reply_zero = EchoOutcome::Reply(0);
        assert_eq!(reply_zero.round_trip_ms().map(|ms| ms.max(1)), Some(1));

        let reply_trace_zero = EchoOutcome::ReplyTrace { ms: 0, hops: 4 };
        assert_eq!(reply_trace_zero.round_trip_ms().map(|ms| ms.max(1)), Some(1));

        let reply_trace = EchoOutcome::ReplyTrace { ms: 35, hops: 6 };
        assert_eq!(reply_trace.round_trip_ms().map(|ms| ms.max(1)), Some(35));

        let timeout = EchoOutcome::Timeout;
        assert_eq!(timeout.round_trip_ms().map(|ms| ms.max(1)), None);

        let unavailable = EchoOutcome::Unavailable(11003);
        assert_eq!(unavailable.round_trip_ms().map(|ms| ms.max(1)), None);

        #[cfg(not(windows))]
        assert_eq!(round_trip_ms(std::net::Ipv4Addr::new(1, 1, 1, 1)), None);
    }

    #[cfg(windows)]
    #[test]
    fn native_tcp_table_exposes_established_loopback_owner() {
        use std::net::{TcpListener, TcpStream};
        use windows_sys::Win32::{NetworkManagement::IpHelper::MIB_TCPROW_OWNER_PID, Networking::WinSock::AF_INET};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let _client = TcpStream::connect(address).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let mut found = false;
        unsafe {
            visit_tcp_rows::<MIB_TCPROW_OWNER_PID>(AF_INET as u32, |row| {
                found |= row.dwOwningPid == std::process::id()
                    && row.dwState == 5
                    && u16::from_be(row.dwRemotePort as u16) == address.port()
                    && row.dwRemoteAddr.to_ne_bytes() == [127, 0, 0, 1];
            }).unwrap();
        }
        assert!(found, "TCP table must expose the established test connection and its PID");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "Windows native ICMP smoke; requires working IPv4/IPv6 loopback ICMP"]
    fn native_icmp_loopback_both_families() {
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let address = address.parse().unwrap();
            let outcome = echo(Destination { remote: address, local: address });
            assert!(matches!(outcome, EchoOutcome::Reply(_) | EchoOutcome::ReplyTrace { .. }), "{address}: {outcome:?}");
        }
    }
}
