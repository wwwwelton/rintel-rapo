use std::time::{Duration, Instant};

const MAX_DESTINATIONS: usize = 4;
const TIMEOUT_MS: u32 = 250;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EchoOutcome {
    Reply(u32),
    Timeout,
    Unavailable(u32),
}

impl EchoOutcome {
    pub fn label(&self) -> String {
        match self {
            Self::Reply(0) => "<1 ms".into(),
            Self::Reply(ms) => format!("{ms} ms"),
            Self::Timeout => format!("Timeout ({TIMEOUT_MS} ms; ICMP pode estar bloqueado)"),
            Self::Unavailable(code) => format!("Indisponível (Windows/IP {code})"),
        }
    }

    #[cfg(any(windows, test))]
    fn from_status(status: u32, elapsed_ms: u32) -> Self {
        match status {
            0 => Self::Reply(elapsed_ms),
            11010 | 1460 => Self::Timeout,
            code => Self::Unavailable(code),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndpointPing {
    pub destination: String,
    pub outcome: EchoOutcome,
}

#[derive(Debug, Clone)]
pub struct ProcessPing {
    pub summary: String,
    pub endpoints: Vec<EndpointPing>,
}

impl Default for ProcessPing {
    fn default() -> Self {
        Self { summary: "Aguardando processo / amostra ICMP".into(), endpoints: Vec::new() }
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
            self.snapshot = ProcessPing { summary: "Nenhum processo selecionado".into(), endpoints: Vec::new() };
        } else if self.sampled_at.is_none_or(|at| now.duration_since(at) >= SAMPLE_INTERVAL) {
            self.snapshot = collect(pids);
            self.sampled_at = Some(Instant::now());
        }
        self.snapshot.clone()
    }
}

#[cfg(not(windows))]
fn collect(_: &[u32]) -> ProcessPing {
    ProcessPing { summary: "ICMP por processo disponível somente no Windows".into(), endpoints: Vec::new() }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug)]
struct Destination {
    remote: std::net::SocketAddr,
    local: std::net::SocketAddr,
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
        EndpointPing { destination: address, outcome: echo(destination) }
    }).collect();
    ProcessPing { summary, endpoints }
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
fn echo(destination: Destination) -> EchoOutcome {
    use std::net::SocketAddr;
    use windows_sys::Win32::{
        Foundation::{GetLastError, INVALID_HANDLE_VALUE},
        NetworkManagement::IpHelper::*,
        Networking::WinSock::{AF_INET6, SOCKADDR_IN6},
    };
    unsafe {
        let handle = if destination.remote.is_ipv4() { IcmpCreateFile() } else { Icmp6CreateFile() };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return EchoOutcome::Unavailable(GetLastError());
        }
        let payload = *b"RAPO";
        // Aligned storage includes both reply headers, payload, error bytes, and IO_STATUS_BLOCK.
        let mut reply = [0u64; 32];
        let reply_size = std::mem::size_of_val(&reply) as u32;
        let count = match (destination.remote, destination.local) {
            (SocketAddr::V4(remote), _) => IcmpSendEcho(
                handle, u32::from_ne_bytes(remote.ip().octets()), payload.as_ptr().cast(), payload.len() as u16,
                std::ptr::null(), reply.as_mut_ptr().cast(), reply_size, TIMEOUT_MS,
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
                Icmp6SendEcho2(handle, std::ptr::null_mut(), None, std::ptr::null(), &source, &target,
                    payload.as_ptr().cast(), payload.len() as u16, std::ptr::null(),
                    reply.as_mut_ptr().cast(), reply_size, TIMEOUT_MS)
            }
            _ => unreachable!("TCP address families match"),
        };
        let outcome = if count == 0 {
            let code = GetLastError();
            // A failed call cannot become a successful zero-millisecond sample.
            if code == 0 { EchoOutcome::Unavailable(code) } else { EchoOutcome::from_status(code, 0) }
        } else if destination.remote.is_ipv4() {
            let response = &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>();
            EchoOutcome::from_status(response.Status, response.RoundTripTime)
        } else {
            let response = &*reply.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>();
            EchoOutcome::from_status(response.Status, response.RoundTripTime)
        };
        IcmpCloseHandle(handle);
        outcome
    }
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
        assert_eq!(EchoOutcome::from_status(11010, 0), EchoOutcome::Timeout);
        assert_eq!(EchoOutcome::from_status(5, 0), EchoOutcome::Unavailable(5));
        assert!(EchoOutcome::Timeout.label().starts_with("Timeout"));
        assert!(EchoOutcome::Unavailable(5).label().contains("Indisponível"));
        assert_eq!(EchoOutcome::Reply(0).label(), "<1 ms");
    }

    #[test]
    fn clearing_target_clears_previous_endpoints_immediately() {
        let mut sampler = ProcessPingSampler {
            pids: vec![42], sampled_at: Some(Instant::now()),
            snapshot: ProcessPing { summary: "Anterior".into(), endpoints: vec![EndpointPing {
                destination: "10.0.0.1".into(), outcome: EchoOutcome::Reply(12),
            }] },
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
                destination: "192.0.2.42".into(), outcome: EchoOutcome::Reply(42),
            }] },
        };
        let sample = sampler.sample(&[99]);
        assert!(sample.endpoints.is_empty());
        assert_eq!(sample.summary, "ICMP por processo disponível somente no Windows");
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
            assert!(matches!(outcome, EchoOutcome::Reply(_)), "{address}: {outcome:?}");
        }
    }
}
