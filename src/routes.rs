//! Session-only host routes. Never adopts existing routes, including crash leftovers.
use serde::Serialize;
use std::net::IpAddr;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub id: String,
    pub candidate: String,
    pub interface: u32,
    pub gateway: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub interface: String,
    pub index: u32,
    pub gateway: String,
    pub ipv6: bool,
}

pub fn destination(text: &str) -> Result<IpAddr, String> {
    let ip: IpAddr = text.parse().map_err(|_| "Informe somente um IP numérico, sem porta, prefixo ou scope.".to_string())?;
    let invalid = ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || match ip {
        IpAddr::V4(v4) => v4 == std::net::Ipv4Addr::BROADCAST || v4.octets()[0] == 0,
        IpAddr::V6(v6) => v6.is_unicast_link_local() || v6.to_ipv4_mapped().is_some(),
    };
    if invalid { Err("Destino deve ser um IP unicast remoto; IPv6 link-local/mapeado não é aceito.".into()) } else { Ok(ip) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    destination: IpAddr,
    prefix: u8,
    luid: u64,
    index: u32,
    gateway: IpAddr,
    scope: u32,
    site_prefix: u8,
    metric: u32,
    protocol: i32,
    origin: i32,
    valid: u32,
    preferred: u32,
    flags: [u8; 4],
}
impl Route {
    fn candidate_id(&self) -> String { format!("{}:{}:{}%{}", self.luid, self.index, self.gateway, self.scope) }
    fn host(&self) -> bool { self.prefix == if self.destination.is_ipv6() { 128 } else { 32 } }
    fn same_key(&self, other: &Self) -> bool {
        self.destination == other.destination && self.prefix == other.prefix && self.luid == other.luid
            && self.index == other.index && self.gateway == other.gateway && self.scope == other.scope
    }
}

trait Backend {
    fn rows(&mut self) -> Result<Vec<Route>, String>;
    fn candidates(&mut self) -> Result<Vec<Candidate>, String>;
    fn create(&mut self, row: &Route) -> Result<(), String>;
    fn delete(&mut self, row: &Route) -> Result<(), String>;
}

#[derive(Clone, Default, Serialize)]
pub struct State {
    pub candidates: Vec<Candidate>,
    pub owned: Vec<OwnedView>,
    pub pending: bool,
    pub message: String,
    pub error: String,
    pub automatic: String,
    pub auto_active: bool,
    pub history: Vec<String>,
    pub rate: Option<crate::network_qos::RateView>,
}
#[derive(Clone, Serialize)]
pub struct OwnedView {
    pub destination: String,
    pub interface: String,
    pub gateway: String,
    pub conflict: bool,
}
struct Owned { row: Route, interface: String, conflict: bool }
struct Manager<B> { backend: B, owned: Vec<Owned> }
impl<B: Backend> Manager<B> {
    fn new(backend: B) -> Self { Self { backend, owned: Vec::new() } }
    fn apply(&mut self, text: &str, candidate: &str) -> Result<(), String> {
        let ip = destination(text)?;
        if self.owned.iter().any(|owned| owned.row.destination == ip) {
            return Err("Este IP já tem uma rota registrada nesta sessão; remova-a primeiro.".into());
        }
        // Re-enumerate at APPLY, never trust a stale UI list or IPC-supplied gateway.
        let selected = self.backend.candidates()?.into_iter().find(|c| c.id == candidate)
            .ok_or("Interface/gateway não está mais disponível. Atualize a lista.")?;
        if selected.ipv6 != ip.is_ipv6() { return Err("Famílias IP do destino e gateway não correspondem.".into()); }
        let rows = self.backend.rows()?;
        if rows.iter().any(|row| row.destination == ip && row.host()) {
            return Err("Já existe rota host para este IP; nenhuma rota existente será substituída.".into());
        }
        let source = rows.iter().find(|row| row.candidate_id() == candidate && !row.host() && !row.gateway.is_unspecified() && row.valid != 0)
            .ok_or("Gateway desapareceu antes da aplicação.")?;
        let row = Route { destination: ip, prefix: if ip.is_ipv6() { 128 } else { 32 },
            luid: source.luid, index: source.index, gateway: source.gateway, scope: source.scope,
            site_prefix: 0, metric: 0, protocol: 3, origin: 0, valid: u32::MAX, preferred: u32::MAX,
            flags: [0, 0, 0, 0] };
        self.backend.create(&row)?;
        // Keep ownership even if a subsequent read/delete fails; never claim an uncreated row.
        self.owned.push(Owned { row, interface: selected.interface, conflict: false });
        Ok(())
    }
    fn remove(&mut self, text: &str) -> Result<(), String> {
        let ip = destination(text)?;
        let index = self.owned.iter().position(|owned| owned.row.destination == ip).ok_or("Rota não pertence a esta sessão.")?;
        if self.owned[index].conflict { return Err("Rota alterada externamente; não será removida pelo RAPO.".into()); }
        let expected = &self.owned[index].row;
        let rows = self.backend.rows()?;
        match rows.iter().find(|row| row.same_key(expected)) {
            None => { self.owned.remove(index); Ok(()) }
            Some(current) if current == expected => {
                self.backend.delete(expected)?;
                self.owned.remove(index);
                Ok(())
            }
            Some(_) => {
                self.owned[index].conflict = true;
                Err("Conflito: atributos da rota mudaram externamente. Rota preservada; inspecione no Windows.".into())
            }
        }
    }
    fn remove_all(&mut self) -> Result<(), String> {
        let destinations: Vec<_> = self.owned.iter().map(|owned| owned.row.destination.to_string()).collect();
        let errors: Vec<_> = destinations.iter().filter_map(|ip| self.remove(ip).err().map(|e| format!("{ip}: {e}"))).collect();
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }
    fn view(&self, state: &mut State) {
        state.owned = self.owned.iter().map(|owned| OwnedView { destination: owned.row.destination.to_string(),
            interface: owned.interface.clone(), gateway: format!("{}%{}", owned.row.gateway, owned.row.scope), conflict: owned.conflict }).collect();
    }
}

#[cfg(windows)]
pub mod native;
#[cfg(windows)]
mod worker;
#[cfg(windows)]
pub use worker::{Operation, Service};

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake { rows: Vec<Route>, candidates: Vec<Candidate>, fail_create: bool, fail_delete: bool, deleted: usize }
    impl Backend for Fake {
        fn rows(&mut self) -> Result<Vec<Route>, String> { Ok(self.rows.clone()) }
        fn candidates(&mut self) -> Result<Vec<Candidate>, String> { Ok(self.candidates.clone()) }
        fn create(&mut self, row: &Route) -> Result<(), String> {
            if self.fail_create { return Err("Administrador necessário".into()); }
            if self.rows.iter().any(|r| r.same_key(row)) { return Err("existing".into()); }
            self.rows.push(row.clone()); Ok(())
        }
        fn delete(&mut self, row: &Route) -> Result<(), String> {
            if self.fail_delete { return Err("acesso negado".into()); }
            self.rows.retain(|r| !r.same_key(row)); self.deleted += 1; Ok(())
        }
    }
    fn manager() -> Manager<Fake> {
        let gateway = Route { destination: "0.0.0.0".parse().unwrap(), prefix: 0, luid: 7, index: 2,
            gateway: "192.0.2.1".parse().unwrap(), scope: 0, site_prefix: 0, metric: 20, protocol: 3, origin: 0,
            valid: u32::MAX, preferred: u32::MAX, flags: [0; 4] };
        let candidate = Candidate { id: gateway.candidate_id(), interface: "Ethernet".into(), index: 2, gateway: gateway.gateway.to_string(), ipv6: false };
        Manager::new(Fake { rows: vec![gateway], candidates: vec![candidate], fail_create: false, fail_delete: false, deleted: 0 })
    }
    #[test]
    fn validates_numeric_unicast_only() {
        for value in ["example.org", "192.0.2.1/32", "192.0.2.1:80", "0.0.0.0", "127.0.0.1", "255.255.255.255", "224.0.0.1", "::", "::1", "fe80::1", "fe80::1%2", "::ffff:192.0.2.1"] { assert!(destination(value).is_err(), "{value}"); }
        for value in ["192.0.2.42", "2001:db8::42"] { assert!(destination(value).is_ok()); }
    }
    #[test]
    fn lifecycle_retains_failed_delete_and_never_deletes_default() {
        let mut m = manager(); let id = m.backend.candidates[0].id.clone();
        m.apply("192.0.2.42", &id).unwrap();
        assert_eq!(m.owned.len(), 1);
        m.backend.fail_delete = true;
        assert!(m.remove_all().is_err()); assert_eq!(m.owned.len(), 1);
        m.backend.fail_delete = false;
        m.remove_all().unwrap(); assert!(m.owned.is_empty());
        assert_eq!(m.backend.rows.len(), 1); assert_eq!(m.backend.rows[0].prefix, 0);
        assert_eq!(m.backend.deleted, 1);
    }
    #[test]
    fn rejects_foreign_host_even_on_other_interface_and_failed_create() {
        let mut m = manager(); let id = m.backend.candidates[0].id.clone();
        let mut foreign = m.backend.rows[0].clone(); foreign.destination = "192.0.2.42".parse().unwrap(); foreign.prefix = 32; foreign.index = 99;
        m.backend.rows.push(foreign);
        assert!(m.apply("192.0.2.42", &id).is_err()); assert!(m.owned.is_empty());
        m.backend.fail_create = true;
        assert!(m.apply("192.0.2.43", &id).is_err()); assert!(m.owned.is_empty()); assert_eq!(m.backend.deleted, 0);
    }
    #[test]
    fn revalidates_candidate_and_family() {
        let mut m = manager(); let id = m.backend.candidates[0].id.clone();
        assert!(m.apply("2001:db8::42", &id).is_err());
        assert!(m.apply("192.0.2.42", "forged").is_err());
        m.backend.candidates.clear(); assert!(m.apply("192.0.2.42", &id).is_err());
    }
    #[test]
    fn external_change_is_preserved_and_conflict_is_sticky() {
        let mut m = manager(); let id = m.backend.candidates[0].id.clone();
        m.apply("192.0.2.42", &id).unwrap(); m.backend.rows[1].metric += 1;
        assert!(m.remove_all().is_err()); assert!(m.owned[0].conflict); assert_eq!(m.backend.deleted, 0);
        m.backend.rows[1].metric -= 1;
        assert!(m.remove_all().is_err()); assert_eq!(m.backend.deleted, 0);
    }
    #[test]
    fn missing_owned_route_does_not_delete_replacement_with_different_key() {
        let mut m = manager(); let id = m.backend.candidates[0].id.clone();
        m.apply("192.0.2.42", &id).unwrap(); m.backend.rows[1].luid = 99;
        m.remove_all().unwrap(); assert!(m.owned.is_empty()); assert_eq!(m.backend.deleted, 0); assert_eq!(m.backend.rows.len(), 2);
    }
}
