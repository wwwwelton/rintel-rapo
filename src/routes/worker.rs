use super::{native::Windows, Backend, Manager, State};
use crate::{network_learning::{Action, Session}, network_qos::Policy, process_ping::ProcessPing};
use std::{path::PathBuf, sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc::{self, Receiver, SyncSender}, Arc}, time::{Duration, Instant}};

pub enum Operation {
    Refresh,
    Apply { destination: String, candidate: String },
    Remove(String),
    RemoveAll,
    Start { destination: String, candidates: Vec<String>, sample: ProcessPing },
    RateApply { pid: u32, exe: PathBuf, bps: u64 },
    RateStart { pid: u32, exe: PathBuf, rates: Vec<u64>, destination: String, sample: ProcessPing },
    RateRemove,
    Stop,
    Observe(ProcessPing),
}
pub struct Service {
    pub state: State,
    sender: Option<SyncSender<Operation>>,
    receiver: Receiver<State>,
    done: Receiver<()>,
    stop: Arc<AtomicBool>,
    busy: Arc<AtomicUsize>,
    last_sample: u64,
}
struct RateTarget { pid: u32, exe: PathBuf, route: String }
struct Runtime {
    manager: Manager<Windows>,
    state: State,
    session: Option<Session>,
    rate: Option<Policy>,
    rate_target: Option<RateTarget>,
}
fn shutdown_log(error: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("rintel-rapo-routes.log");
    if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(log, "{:?} {error}", std::time::SystemTime::now());
    }
}
impl Runtime {
    fn remove_rate(&mut self) -> Result<(), String> {
        if let Some(policy) = &mut self.rate { policy.remove()?; }
        self.rate = None; Ok(())
    }
    fn apply_rate(&mut self, pid: u32, exe: PathBuf, bps: u64) -> Result<(), String> {
        if self.rate.is_some() { return Err("Remova a política própria atual antes de aplicar outra taxa.".into()); }
        // Record intent before starting a subprocess: timeout must not lose cleanup ownership.
        self.rate = Some(Policy::intent(pid, &exe, bps)?);
        self.rate.as_mut().unwrap().create()
    }
    fn cleanup_session(&mut self) -> Result<(), String> {
        let session = self.session.take();
        let rate_mode = self.rate_target.take().is_some();
        if rate_mode { return self.remove_rate(); }
        if let Some(auto) = session {
            if self.manager.owned.iter().any(|o| o.row.destination.to_string() == auto.destination) { self.manager.remove(&auto.destination)?; }
        }
        Ok(())
    }
    fn require_manual(&self) -> Result<(), String> {
        if self.session.is_some() { Err("Pare Aprender e aplicar antes de alterar rota/taxa manualmente.".into()) } else { Ok(()) }
    }
    fn rate_sample(&mut self, sample: &ProcessPing) -> Result<ProcessPing, String> {
        let target = self.rate_target.as_ref().ok_or("Sessão de taxa sem alvo")?;
        super::super::network_qos::Policy::check_target(target.pid, &target.exe)?;
        if !sample.pids.contains(&target.pid) { return Err("Alvo de QoS deixou os PIDs observados.".into()); }
        if let Some(policy) = &mut self.rate { policy.validate()?; }
        let destination = &self.session.as_ref().ok_or("Sessão ausente")?.destination;
        decorate_rate_sample(sample, destination, &target.route, self.rate.as_ref().map(|p| p.view.bps))
    }
    fn perform(&mut self, operation: Operation) -> Result<(), String> {
        match operation {
            Operation::Refresh => {
                self.state.candidates = self.manager.backend.candidates()?;
                self.state.message = "Gateways/interfaces atualizados sob demanda; disponibilidade não comprova conectividade.".into();
            }
            Operation::Apply { destination, candidate } => {
                self.require_manual()?; self.manager.apply(&destination, &candidate)?;
                self.state.message = "Rota host criada nesta sessão para TODOS os aplicativos ao IP.".into();
            }
            Operation::Remove(destination) => { self.require_manual()?; self.manager.remove(&destination)?; self.state.message = "Rota própria removida ou já ausente; terceiros preservados.".into(); }
            Operation::RemoveAll => { self.cleanup_session()?; self.manager.remove_all()?; self.remove_rate()?; self.state.automatic = "Automático parado; ajustes próprios removidos quando inalterados.".into(); }
            Operation::Start { destination, candidates, sample } => {
                self.require_manual()?;
                if self.rate.is_some() { return Err("Remova o limite de saída antes de aprender rotas; modos não podem se misturar.".into()); }
                let ip = super::destination(&destination)?;
                if self.manager.owned.iter().any(|o| o.row.destination == ip) { return Err("Remova a rota manual desse IP antes de iniciar o automático.".into()); }
                if self.manager.backend.rows()?.iter().any(|r| r.destination == ip && r.host()) { return Err("Já existe rota host; automático não a substituirá.".into()); }
                let available = self.manager.backend.candidates()?;
                if candidates.iter().any(|id| !available.iter().any(|c| &c.id == id && c.ipv6 == ip.is_ipv6())) { return Err("Gateway autorizado indisponível ou família incompatível; atualize a lista.".into()); }
                let endpoint = sample.endpoints.iter().find(|e| e.destination == destination).ok_or("IP não observado")?;
                let socket = std::net::SocketAddr::new(ip, 0);
                let current = super::native::observe(socket)?.0;
                if endpoint.route.as_ref() != Some(&current) { return Err("Rota mudou desde a amostra; aguarde uma nova amostra antes de iniciar.".into()); }
                self.state.history.clear(); self.session = Some(Session::start(destination, candidates, &sample, Instant::now())?);
            }
            Operation::RateApply { pid, exe, bps } => { self.require_manual()?; self.apply_rate(pid, exe, bps)?; self.state.message = "Limite de saída por executável confirmado no ActiveStore; não é prova de pacing/ganho real.".into(); }
            Operation::RateRemove => { self.cleanup_session()?; self.remove_rate()?; self.state.message = "Limite próprio removido; políticas anteriores preservadas.".into(); }
            Operation::RateStart { pid, exe, mut rates, destination, sample } => {
                self.require_manual()?;
                if self.rate.is_some() || !self.manager.owned.is_empty() { return Err("Remova ajustes manuais de rede antes do aprendizado de taxas.".into()); }
                Policy::check_target(pid, &exe)?;
                rates.sort_unstable(); rates.dedup();
                if rates.len() < 2 || rates.contains(&0) { return Err("Autorize ao menos duas taxas positivas distintas em Mbps; banda de upload nunca é inferida da placa.".into()); }
                if !sample.pids.contains(&pid) { return Err("Executável deve pertencer aos PIDs da amostra atual.".into()); }
                let route = sample.endpoints.iter().find(|e| e.destination == destination).and_then(|e| e.route.as_ref()).ok_or("IP/rota ICMP atual indisponível")?.id.clone();
                let current = super::native::observe(std::net::SocketAddr::new(super::destination(&destination)?, 0))?.0;
                if current.id != route { return Err("Rota mudou desde a amostra; aguarde uma nova amostra antes de aprender taxas.".into()); }
                let decorated = decorate_rate_sample(&sample, &destination, &route, None)?;
                let mut authorized = vec!["uncapped".into()]; authorized.extend(rates.iter().map(|r| r.to_string()));
                let session = Session::start(destination, authorized, &decorated, Instant::now())?;
                self.rate_target = Some(RateTarget { pid, exe, route }); self.session = Some(session); self.state.history.clear();
                self.state.message = "Taxas autorizadas serão comparadas à referência sem limite RAPO, não a pacotes/latência de envio medidos.".into();
            }
            Operation::Stop => { self.cleanup_session()?; self.state.automatic = "Aprendizado parado; referência solicitada. Falhas de remoção continuam registradas para tentar novamente.".into(); }
            Operation::Observe(sample) => {
                if self.session.is_none() { return Ok(()); }
                if self.rate_target.is_none() {
                    let destination = &self.session.as_ref().unwrap().destination;
                    if let Some(owned) = self.manager.owned.iter().find(|o| o.row.destination.to_string() == *destination) {
                        let expected = owned.row.clone();
                        if !self.manager.backend.rows()?.contains(&expected) { return Err("Rota própria ausente/alterada externamente durante janela; suspender e preservar terceiros.".into()); }
                        let identity = format!("{}|{}/{}|{}|{}|", expected.candidate_id(), expected.destination, expected.prefix, expected.metric, expected.protocol);
                        if !sample.endpoints.iter().find(|e| &e.destination == destination).and_then(|e| e.route.as_ref()).is_some_and(|r| r.id.starts_with(&identity)) {
                            return Err("A sonda não confirmou a rota host aplicada; janela inválida.".into());
                        }
                    }
                }
                let sample = if self.rate_target.is_some() { self.rate_sample(&sample)? } else { sample };
                let auto = self.session.as_mut().unwrap();
                let action = auto.observe(&sample, Instant::now())?;
                self.state.history = auto.history(Instant::now());
                let destination = auto.destination.clone();
                let changed = !matches!(action, Action::None);
                match action {
                    Action::None => (),
                    Action::Apply(candidate) => {
                        if let Some(target) = &self.rate_target {
                            let pid = target.pid; let exe = target.exe.clone();
                            self.apply_rate(pid, exe, candidate.parse().map_err(|_| "Taxa interna inválida")?)?;
                        } else { self.manager.apply(&destination, &candidate)?; }
                    }
                    Action::Restore => {
                        if self.rate_target.is_some() { self.remove_rate()?; }
                        else if self.manager.owned.iter().any(|o| o.row.destination.to_string() == destination) { self.manager.remove(&destination)?; }
                    }
                }
                if changed { self.session.as_mut().unwrap().barrier(Instant::now()); }
            }
        }
        Ok(())
    }
    fn snapshot(&mut self) -> State {
        self.state.auto_active = self.session.is_some();
        if let Some(auto) = &self.session { self.state.automatic = format!("{} · {} · {}", if self.rate_target.is_some() { "QoS saída" } else { "Rotas" }, auto.destination, auto.message); }
        self.state.rate = self.rate.as_ref().map(|p| p.view.clone()); self.manager.view(&mut self.state); self.state.clone()
    }
}
fn decorate_rate_sample(sample: &ProcessPing, destination: &str, expected_route: &str, bps: Option<u64>) -> Result<ProcessPing, String> {
    let mut sample = sample.clone();
    let route = sample.endpoints.iter_mut().find(|e| e.destination == destination).and_then(|e| e.route.as_mut()).ok_or("Destino/rota deixou de ser observado")?;
    if route.id != expected_route { return Err("Rota de rede mudou durante aprendizado de taxa; não confundir causas.".into()); }
    let label = bps.map_or_else(|| "uncapped".into(), |r| r.to_string());
    route.id = format!("{}|rate:{label}", route.id); route.candidate = label;
    route.gateway = format!("{} · {}", route.gateway, bps.map_or_else(|| "sem limite RAPO".into(), |r| format!("{:.6} Mbps saída", r as f64 / 1_000_000.0)));
    Ok(sample)
}
impl Service {
    pub fn new() -> Self {
        let (sender, requests) = mpsc::sync_channel(1); let (results, receiver) = mpsc::channel(); let (finished, done) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false)); let stopping = stop.clone();
        let busy = Arc::new(AtomicUsize::new(0)); let working = busy.clone();
        let worker = std::thread::Builder::new().name("rapo-network".into()).spawn(move || {
            let mut runtime = Runtime { manager: Manager::new(Windows), state: State::default(), session: None, rate: None, rate_target: None };
            loop {
                if stopping.load(Ordering::Acquire) { break; }
                let request = match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if runtime.session.as_ref().is_some_and(|s| s.expired(Instant::now())) {
                            working.fetch_add(1, Ordering::AcqRel);
                            let cleanup = runtime.cleanup_session();
                            runtime.state.error = format!("Janela ICMP sem confirmação atual; automático suspenso. {}", cleanup.err().unwrap_or_default());
                            let _ = results.send(runtime.snapshot()); working.fetch_sub(1, Ordering::AcqRel);
                        }
                        continue;
                    }
                };
                if stopping.load(Ordering::Acquire) { break; }
                let observation = matches!(&request, Operation::Observe(_));
                if let Err(error) = runtime.perform(request) {
                    runtime.state.error = error;
                    if runtime.session.is_some() {
                        if let Err(cleanup) = runtime.cleanup_session() { runtime.state.error.push_str(&format!(" Restauração falhou; ownership mantido: {cleanup}")); }
                    }
                    runtime.state.automatic = "Automático suspenso; nenhuma vitória declarada.".into();
                } else if !observation { runtime.state.error.clear(); }
                let _ = results.send(runtime.snapshot()); working.fetch_sub(1, Ordering::AcqRel);
            }
            if let Err(error) = runtime.manager.remove_all() { shutdown_log(&format!("Cleanup de rotas falhou: {error}")); }
            if let Err(error) = runtime.remove_rate() { shutdown_log(&format!("Cleanup QoS falhou: {error}")); }
            let _ = finished.send(());
        });
        let mut state = State::default();
        let sender = match worker { Ok(_) => Some(sender), Err(error) => { state.error = format!("Worker de rede indisponível: {error}"); None } };
        Self { state, sender, receiver, done, stop, busy, last_sample: 0 }
    }
    pub fn request(&mut self, operation: Operation) {
        if self.busy.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
        let result = self.sender.as_ref().ok_or("Worker indisponível".to_string()).and_then(|s| s.try_send(operation).map_err(|e| e.to_string()));
        match result { Ok(()) => self.state.pending = true,
            Err(error) => { self.busy.fetch_sub(1, Ordering::AcqRel); self.state.error = format!("Operação não enviada: {error}"); } }
    }
    pub fn poll(&mut self, sample: &ProcessPing) {
        while let Ok(state) = self.receiver.try_recv() { self.state = state; }
        self.state.pending = self.busy.load(Ordering::Acquire) != 0;
        if self.state.auto_active && !self.state.pending && sample.sample_id != self.last_sample {
            self.request(Operation::Observe(sample.clone())); self.last_sample = sample.sample_id;
        }
    }
    pub fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::AcqRel) { return; }
        if self.sender.take().is_none() { return; }
        if self.done.recv_timeout(Duration::from_secs(30)).is_err() {
            shutdown_log("Cleanup ainda pendente após 30 s; inspecione rotas e políticas ActiveStore. Encerramento forçado pode deixá-las até reiniciar.");
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) { self.shutdown(); }
}
