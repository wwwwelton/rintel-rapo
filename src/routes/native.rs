use super::{Backend, Candidate, Observation, Route};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use windows_sys::Win32::{NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp}, Networking::WinSock::*};

pub(super) struct Windows;
fn result(code: u32, operation: &str) -> Result<(), String> {
    if code == 0 { Ok(()) } else if code == 5 || code == 1314 {
        Err(format!("{operation}: exige executar como administrador (Windows {code})."))
    } else { Err(format!("{operation}: {} (Windows {code})", std::io::Error::from_raw_os_error(code as i32))) }
}
fn address(ip: IpAddr, scope: u32) -> SOCKADDR_INET {
    unsafe {
        let mut value: SOCKADDR_INET = std::mem::zeroed();
        match ip {
            IpAddr::V4(ip) => { value.Ipv4.sin_family = AF_INET; value.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets()); }
            IpAddr::V6(ip) => { value.Ipv6.sin6_family = AF_INET6; value.Ipv6.sin6_addr.u.Byte = ip.octets(); value.Ipv6.Anonymous.sin6_scope_id = scope; }
        }
        value
    }
}
fn decode(value: SOCKADDR_INET) -> Option<(IpAddr, u32)> {
    unsafe { match value.si_family {
        AF_INET => Some((Ipv4Addr::from(value.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()).into(), 0)),
        AF_INET6 => Some((Ipv6Addr::from(value.Ipv6.sin6_addr.u.Byte).into(), value.Ipv6.Anonymous.sin6_scope_id)),
        _ => None,
    } }
}
fn decode_row(row: &MIB_IPFORWARD_ROW2) -> Option<Route> {
    let (destination, _) = decode(row.DestinationPrefix.Prefix)?;
    let (gateway, scope) = decode(row.NextHop)?;
    Some(Route { destination, prefix: row.DestinationPrefix.PrefixLength, luid: unsafe { row.InterfaceLuid.Value },
        index: row.InterfaceIndex, gateway, scope, site_prefix: row.SitePrefixLength, metric: row.Metric,
        protocol: row.Protocol, origin: row.Origin, valid: row.ValidLifetime, preferred: row.PreferredLifetime,
        flags: [row.Loopback, row.AutoconfigureAddress, row.Publish, row.Immortal] })
}
fn encode_row(row: &Route) -> MIB_IPFORWARD_ROW2 {
    unsafe {
        let mut native = std::mem::zeroed(); InitializeIpForwardEntry(&mut native);
        native.InterfaceLuid.Value = row.luid; native.InterfaceIndex = row.index;
        native.DestinationPrefix.Prefix = address(row.destination, 0); native.DestinationPrefix.PrefixLength = row.prefix;
        native.NextHop = address(row.gateway, row.scope); native.SitePrefixLength = row.site_prefix;
        native.ValidLifetime = row.valid; native.PreferredLifetime = row.preferred; native.Metric = row.metric;
        native.Protocol = row.protocol; native.Origin = row.origin;
        native.Loopback = row.flags[0]; native.AutoconfigureAddress = row.flags[1]; native.Publish = row.flags[2]; native.Immortal = row.flags[3];
        native
    }
}
impl Backend for Windows {
    fn rows(&mut self) -> Result<Vec<Route>, String> {
        unsafe {
            let mut table = std::ptr::null_mut();
            result(GetIpForwardTable2(AF_UNSPEC, &mut table), "Ler rotas")?;
            if table.is_null() { return Err("Windows retornou tabela de rotas nula.".into()); }
            let rows = std::slice::from_raw_parts(std::ptr::addr_of!((*table).Table).cast::<MIB_IPFORWARD_ROW2>(), (*table).NumEntries as usize)
                .iter().filter_map(decode_row).collect();
            FreeMibTable(table.cast()); Ok(rows)
        }
    }
    fn candidates(&mut self) -> Result<Vec<Candidate>, String> {
        let mut candidates = Vec::new();
        for route in self.rows()? {
            if route.gateway.is_unspecified() || route.gateway.is_loopback() || route.gateway.is_multicast()
                || route.host() || route.valid == 0 || route.flags[0] != 0 || route.destination.is_ipv6() != route.gateway.is_ipv6() { continue; }
            let id = route.candidate_id();
            if candidates.iter().any(|c: &Candidate| c.id == id) { continue; }
            unsafe {
                let mut interface: MIB_IF_ROW2 = std::mem::zeroed(); interface.InterfaceLuid.Value = route.luid;
                if GetIfEntry2(&mut interface) != 0 || interface.OperStatus != IfOperStatusUp || interface.InterfaceIndex != route.index { continue; }
                let mut ip: MIB_IPINTERFACE_ROW = std::mem::zeroed(); InitializeIpInterfaceEntry(&mut ip);
                ip.Family = if route.gateway.is_ipv6() { AF_INET6 } else { AF_INET };
                ip.InterfaceLuid.Value = route.luid;
                if GetIpInterfaceEntry(&mut ip) != 0 || ip.Connected == 0 { continue; }
                let end = interface.Alias.iter().position(|&v| v == 0).unwrap_or(interface.Alias.len());
                let name = String::from_utf16_lossy(&interface.Alias[..end]);
                candidates.push(Candidate { id, interface: name, index: route.index, ipv6: route.gateway.is_ipv6(),
                    gateway: if route.scope == 0 { route.gateway.to_string() } else { format!("{}%{}", route.gateway, route.scope) } });
            }
        }
        candidates.sort_unstable_by(|a, b| a.id.cmp(&b.id)); Ok(candidates)
    }
    fn create(&mut self, row: &Route) -> Result<(), String> {
        // CreateIpForwardEntry2 only touches the active store, not persistent routes.
        result(unsafe { CreateIpForwardEntry2(&encode_row(row)) }, "Criar rota host")
    }
    fn delete(&mut self, row: &Route) -> Result<(), String> {
        // Windows has no compare-and-delete API: recheck immediately before the keyed delete.
        let current = self.rows()?.into_iter().find(|r| r.same_key(row));
        match current {
            None => Ok(()),
            Some(current) if current == *row => result(unsafe { DeleteIpForwardEntry2(&encode_row(row)) }, "Remover rota host"),
            Some(_) => Err("Rota mudou antes da remoção; preservada.".into()),
        }
    }
}

/// Route lookup immediately around the ICMP call; it does not configure the network.
pub fn observe(destination: SocketAddr) -> Result<(Observation, SocketAddr), String> {
    observe_on(destination, 0)
}

pub(crate) fn candidates() -> Result<Vec<Candidate>, String> { Windows.candidates() }

pub(crate) fn egresses(destination: SocketAddr) -> Result<Vec<(Observation, SocketAddr)>, String> {
    let mut exits = vec![observe(destination)?];
    if destination.ip().is_loopback() { return Ok(exits); }
    for candidate in candidates()? {
        if candidate.ipv6 != destination.is_ipv6() || exits.iter().any(|(route, _)| route.interface == candidate.index) { continue; }
        if let Ok(exit) = observe_on(destination, candidate.index) { exits.push(exit); }
    }
    Ok(exits)
}

fn observe_on(destination: SocketAddr, interface: u32) -> Result<(Observation, SocketAddr), String> {
    unsafe {
        let target = address(destination.ip(), match destination { SocketAddr::V6(v6) => v6.scope_id(), _ => 0 });
        let mut row = std::mem::zeroed(); let mut source = std::mem::zeroed();
        result(GetBestRoute2(std::ptr::null(), interface, std::ptr::null(), &target, 0, &mut row, &mut source), "Consultar rota ICMP")?;
        let route = decode_row(&row).ok_or("Família de rota não suportada")?;
        let (ip, scope) = decode(source).ok_or("Origem ICMP não disponível")?;
        let local = match ip { IpAddr::V4(ip) => SocketAddr::from((ip, 0)), IpAddr::V6(ip) => SocketAddr::V6(std::net::SocketAddrV6::new(ip, 0, 0, scope)) };
        let candidate = route.candidate_id();
        let id = format!("{candidate}|{}/{}|{}|{}|{ip}%{scope}", route.destination, route.prefix, route.metric, route.protocol);
        Ok((Observation { id, candidate, interface: route.index, gateway: if route.scope == 0 { route.gateway.to_string() } else { format!("{}%{}", route.gateway, route.scope) }, source: local.ip().to_string() }, local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Elevated isolated IPv6 route smoke; requires RAPO_ROUTE_SMOKE=YES and RAPO_ROUTE_CANDIDATE"]
    fn native_ipv6_owned_add_delete() {
        assert_eq!(std::env::var("RAPO_ROUTE_SMOKE").as_deref(), Ok("YES"), "Explicit opt-in required");
        let selected = std::env::var("RAPO_ROUTE_CANDIDATE").expect("Use an id from native_list_candidates");
        let mut manager = super::super::Manager::new(Windows);
        assert!(manager.backend.candidates().unwrap().iter().any(|c| c.id == selected && c.ipv6), "Existing usable IPv6 gateway required");
        let ip = "2001:db8::5241:504f";
        manager.apply(ip, &selected).unwrap();
        // Always attempt cleanup before assertions, including a failed ownership read.
        let proof = manager.backend.rows().map(|rows| rows.iter().any(|r| *r == manager.owned[0].row));
        let removal = manager.remove_all();
        assert!(proof.unwrap(), "Created row must match all ownership attributes");
        removal.unwrap(); assert!(manager.owned.is_empty());
        assert!(!manager.backend.rows().unwrap().iter().any(|r| r.destination == ip.parse::<IpAddr>().unwrap() && r.host()));
    }
    #[test]
    fn native_list_candidates() {
        let candidates = Windows.candidates().unwrap();
        println!("{}", serde_json::to_string_pretty(&candidates).unwrap());
    }
}
