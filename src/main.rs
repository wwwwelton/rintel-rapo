#![cfg_attr(not(test), windows_subsystem = "windows")]

pub mod cache;
#[cfg(any(test, windows))]
mod capture;
#[cfg(any(test, windows))]
mod learning;
pub mod fps;
pub mod hardware;
pub mod intel_dtt;
#[cfg(windows)]
mod webview;
pub mod ui_model;
pub mod view;
pub mod snapshot;
pub mod command;
pub mod telemetry;
pub mod process_ping;
pub mod routes;
mod network_learning;
pub mod relay;
mod network_qos;
pub mod tray;
#[cfg(any(test, windows))]
mod prefetch;
#[cfg(windows)]
mod cpu;
#[cfg(windows)]
mod pin;
#[cfg(any(test, windows))]
mod steam;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod release;
#[cfg(windows)]
mod netqos;
#[cfg(all(test, windows))]
mod cache_bench;

#[cfg(windows)]
mod app {
    use super::{
        cache::{self, MemoryQosState},
        cpu::Monitor,
        hardware::{self, CacheTopology},
        fps::FpsTracker,
        intel_dtt::{self, IntelDttReport},
        pin::Pinner,
        prefetch,
        steam::{self, Game},
        telemetry::TelemetryCollector,
        tray::TrayController,
        windows::{self, Optimizer, Running},
    };
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::Receiver,
            Arc,
        },
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
        },
    };


    pub struct App {
        games: Vec<Game>,
        game_search_names: Vec<String>,
        running: Vec<Running>,
        optimizer: Optimizer,
        auto_games: HashSet<PathBuf>,
        pin_games: HashSet<PathBuf>,
        qos_games: HashSet<PathBuf>,
        applied_qos: HashMap<u32, MemoryQosState>,
        pinner: Option<Pinner>,
        pub windowed_mode: bool,
        search: String,
        cpu: Monitor,
        aion2_prefetched: bool,
        aion2_status: String,
        aion2_worker: Option<Receiver<Result<u64, String>>>,
        aion2_cancel: Option<Arc<AtomicBool>>,
        cpu_error: String,
        error: String,
        last_refresh: Instant,
        dtt_report: IntelDttReport,
        cache_topo: CacheTopology,
        telemetry: TelemetryCollector,
        fps: FpsTracker,
        tray: TrayController,
        training: Option<Training>,
        rapo_game: Option<PathBuf>,
        training_message: String,
        training_target: usize,
        training_selected: Option<(u32, PathBuf)>,
        learned_profiles: Vec<crate::learning::Profile>,
        runtime_profiles: HashMap<PathBuf, crate::learning::Settings>,
        learned_pids: HashSet<(u32, PathBuf)>,
        netqos: super::netqos::NetQos,
        qos_error: String,
        network_priority: bool,
        routes: crate::routes::Service,
        ui_strings: Arc<HashMap<String, String>>,
        power_watch: Option<power::PlanWatch>,
        power_plan: Option<power::ActivePlan>,
        power_error: String,
        relay: crate::relay::RelayServer,
    }

    fn load_ui_strings() -> HashMap<String, String> {
        let mut map = HashMap::new();
        for line in include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui.json")).lines() {
            let line = line.trim().trim_end_matches(',');
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().trim_matches('"');
                let v = v.trim().trim_matches('"');
                if !k.is_empty() && !v.is_empty() {
                    map.insert(k.to_string(), v.replace("\\n", "\n").replace("\\\"", "\""));
                }
            }
        }
        map
    }

    fn tr<'a>(ui_strings: &'a HashMap<String, String>, id: &str, default: &'a str) -> &'a str {
        ui_strings.get(id).map(|s| s.as_str()).unwrap_or(default)
    }

    impl App {
        pub fn new() -> Self {
            let mut app = Self {
                games: Vec::new(),
                game_search_names: Vec::new(),
                running: Vec::new(),
                optimizer: Optimizer::new(),
                auto_games: HashSet::new(),
                pin_games: HashSet::new(),
                qos_games: HashSet::new(),
                applied_qos: HashMap::new(),
                pinner: Pinner::new().ok(),
                windowed_mode: false,
                search: String::new(),
                error: String::new(),
                aion2_prefetched: false,
                aion2_status: String::new(),
                aion2_worker: None,
                aion2_cancel: None,
                cpu: Monitor::new(),
                cpu_error: String::new(),
                last_refresh: Instant::now() - Duration::from_secs(3),
                dtt_report: intel_dtt::detect_intel_dtt(),
                cache_topo: hardware::get_cache_topology(),
                fps: FpsTracker::new(),
                telemetry: TelemetryCollector::new(),
                tray: TrayController::new(),
                training: None,
                rapo_game: None,
                training_message: String::new(),
                training_target: 0,
                training_selected: None,
                learned_profiles: Vec::new(),
                runtime_profiles: HashMap::new(),
                learned_pids: HashSet::new(),
                netqos: super::netqos::NetQos::default(),
                qos_error: String::new(),
                network_priority: false,
                routes: crate::routes::Service::new(),
                ui_strings: Arc::new(load_ui_strings()),
                power_watch: power::PlanWatch::new().ok(),
                power_plan: None,
                power_error: String::new(),
                relay: crate::relay::RelayServer::new(1080),
            };
            let _ = app.relay.start();
            match crate::learning::load_profiles() {
                Ok(profiles) => app.learned_profiles = profiles,
                Err(error) => app.training_message = error,
            }
            app.scan();
            app.refresh();
            app
        }

        fn scan(&mut self) {
            self.dtt_report = intel_dtt::detect_intel_dtt();
            match windows::steam_root() {
                Some(root) => {
                    self.games = steam::discover(&root);
                    self.game_search_names = self
                        .games
                        .iter()
                        .map(|game| game.name.to_lowercase())
                        .collect();
                    self.auto_games.retain(|directory| {
                        self.games.iter().any(|game| &game.directory == directory)
                    });
                    self.pin_games.retain(|directory| {
                        self.games.iter().any(|game| &game.directory == directory)
                    });
                    self.qos_games.retain(|directory| {
                        self.games.iter().any(|game| &game.directory == directory)
                    });
                    self.error.clear();
                }
                None => {
                    self.games.clear();
                    self.game_search_names.clear();
                    self.error = "Steam não encontrado no registro do usuário".into();
                }
            }
        }
        pub fn tr<'a>(&'a self, id: &str, default: &'a str) -> &'a str {
            tr(&self.ui_strings, id, default)
        }

        fn runtime_profile(&self, exe: &std::path::Path) -> Option<crate::learning::Settings> {
            self.runtime_profiles.get(exe).or_else(|| {
                self.runtime_profiles
                    .iter()
                    .find(|(path, _)| windows::same_executable(path, exe))
                    .map(|(_, settings)| settings)
            }).copied()
        }

        fn restore_qos(&mut self, pid: u32) -> Result<(), String> {
            let Some(state) = self.applied_qos.get(&pid) else { return Ok(()) };
            unsafe {
                let handle = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid,
                );
                if handle.is_null() {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(87) {
                        self.applied_qos.remove(&pid); // The old process exited.
                        return Ok(());
                    }
                    return Err(format!("PID {pid}: {error}"));
                }
                let result = cache::restore_memory_qos(handle, state);
                CloseHandle(handle);
                if result.is_ok() {
                    self.applied_qos.remove(&pid);
                }
                result
            }
        }


        fn refresh(&mut self) {
            let optimizer_active = self.tray.is_optimizer_running();
            self.ui_strings = Arc::new(load_ui_strings());
            // Windows notifies plan changes; without that registration, read every refresh.
            if self.power_watch.is_none() || power::take_plan_change() {
                match power::active_plan() {
                    Ok(plan) => {
                        self.power_plan = Some(plan);
                        self.power_error.clear();
                    }
                    Err(error) => {
                        self.power_error = error;
                        power::retry_plan_read();
                    }
                }
            }

            match windows::running(&self.games) {
                Ok(processes) => {
                    self.running = processes;
                    self.error.clear();
                    self.sync_training();
                    self.apply_learned_profiles();
                    if optimizer_active {
                        for process in &self.running {
                            let desired = self.runtime_profile(&process.exe).map(|s| s.priority)
                                .or_else(|| self.auto_games.iter().any(|dir| windows::same_executable(dir, &process.directory)).then_some(true));
                            if desired.is_some_and(|enable| self.optimizer.enabled(process.pid) != enable) {
                                if let Err(error) = self.optimizer.toggle(process) {
                                    self.error = error;
                                }
                            }
                        }
                    } else {
                        for process in &self.running {
                            if self.optimizer.enabled(process.pid) {
                                if let Err(error) = self.optimizer.toggle(process) {
                                    self.error = error;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    self.running.clear();
                    self.fps.reset();
                    self.error = e;
                }
            }

            if optimizer_active {
                if let Some(error) = self.optimizer.update(self.windowed_mode).into_iter().next() {
                    self.error = error;
                }
            }

            let network_pid = self.selected_process().map(|p| p.pid);
            let active: Vec<&std::path::Path> = if optimizer_active && self.network_priority {
                self.running.iter().filter(|p| Some(p.pid) == network_pid).map(|p| p.exe.as_path()).collect()
            } else {
                Vec::new()
            };
            match self.netqos.sync(&active) {
                Ok(()) => self.qos_error.clear(),
                Err(error) => self.qos_error = error,
            }
            let stale: Vec<u32> = self.applied_qos.keys().copied().filter(|pid| {
                !optimizer_active || !self.running.iter().any(|p|
                    p.pid == *pid && self.effective_settings(p).qos)
            }).collect();
            for pid in stale {
                if let Err(error) = self.restore_qos(pid) {
                    self.error = error;
                }
            }
            if optimizer_active {
                for process in &self.running {
                    if !self.effective_settings(process).qos {
                        continue;
                    }
                    unsafe {
                        let handle = OpenProcess(
                            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, process.pid,
                        );
                        if handle.is_null() {
                            self.error = format!("PID {}: {}", process.pid, std::io::Error::last_os_error());
                            continue;
                        }
                        if let Some(state) = self.applied_qos.get(&process.pid) {
                            match cache::matches_process(handle, state) {
                                Ok(true) => {}
                                Ok(false) => { self.applied_qos.remove(&process.pid); }
                                Err(error) => {
                                    self.error = error;
                                    CloseHandle(handle);
                                    continue;
                                }
                            }
                        }
                        if let std::collections::hash_map::Entry::Vacant(entry) = self.applied_qos.entry(process.pid) {
                            match cache::apply_memory_qos(handle) {
                                Ok(state) => { entry.insert(state); }
                                Err(error) => self.error = error,
                            }
                        }
                        CloseHandle(handle);
                    }
                }
            }

            if let Some(pinner) = &mut self.pinner {
                if optimizer_active {
                    let processes = self.training.as_ref()
                        .and_then(|training| self.running.iter().find(|p| p.pid == training.pid && windows::same_executable(&p.exe, &training.exe)))
                        .map(std::slice::from_ref)
                        .unwrap_or(&self.running);
                    if let Err(error) = pinner.update(processes, &self.pin_games, self.windowed_mode, &self.runtime_profiles) {
                        self.error = error;
                    }
                } else {
                    let _ = pinner.update(&[], &HashSet::new(), false, &self.runtime_profiles);
                }
            }

            let pin_status = self.pinner.as_ref().and_then(Pinner::status);
            let active_process = if optimizer_active {
                self.running.iter().find(|p| {
                    self.optimizer.boosted(p.pid)
                        || self.applied_qos.contains_key(&p.pid)
                        || pin_status.is_some_and(|(pid, _, _)| pid == p.pid)
                })
            } else {
                None
            };
            if let Err(error) = self.cpu.refresh() {
                self.cpu_error = error;
            } else {
                self.cpu_error.clear();
            }

            if let Some(target_proc) = active_process.or_else(|| self.running.first()) {
                let game_pids: Vec<u32> = self
                    .running
                    .iter()
                    .filter(|p| p.directory == target_proc.directory)
                    .map(|p| p.pid)
                    .collect();
                self.telemetry
                    .refresh(&self.cpu.load, Some((target_proc.game.as_str(), &game_pids)));
            } else {
                self.telemetry.refresh(&self.cpu.load, None);
            }
            if self.running.is_empty() {
                self.fps.reset();
            } else {
                self.fps.refresh_statistics();
            }

            let aion2_selected = self.games.iter().any(|game| {
                game.is_aion2
                    && (self.auto_games.contains(&game.directory)
                        || self.pin_games.contains(&game.directory)
                        || self.qos_games.contains(&game.directory))
            });
            if !optimizer_active || !aion2_selected {
                if let Some(cancel) = &self.aion2_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.aion2_prefetched = false;
            }

            if let Some(rx) = &self.aion2_worker {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(bytes) => {
                            self.aion2_status =
                                format!("PREFETCH {:.1} MB OK", bytes as f64 / 1048576.0);
                        }
                        Err(error) => {
                            self.aion2_prefetched = false;
                            self.aion2_status.clear();
                            self.error = error;
                        }
                    }
                    self.aion2_worker = None;
                    self.aion2_cancel = None;
                }
            } else if optimizer_active && aion2_selected && !self.aion2_prefetched {
                if let Some(game) = self.games.iter().find(|g| {
                    g.is_aion2
                        && (self.auto_games.contains(&g.directory)
                            || self.pin_games.contains(&g.directory)
                            || self.qos_games.contains(&g.directory))
                }) {
                    if !self.running.iter().any(|p| p.directory == game.directory)
                        && matches!(windows::aion2_running(), Ok(false))
                    {
                        let root = game.directory.clone();
                        let cancel = Arc::new(AtomicBool::new(false));
                        let (tx, rx) = std::sync::mpsc::channel();
                        self.aion2_prefetched = true;
                        self.aion2_status = "PREFETCH EM CURSO...".into();
                        self.aion2_cancel = Some(cancel.clone());
                        self.aion2_worker = Some(rx);
                        std::thread::spawn(move || {
                            let result = prefetch::prefetch_aion2_dynamic(&root, || {
                                if cancel.load(Ordering::Relaxed) {
                                    prefetch::PrefetchControl::Stop
                                } else {
                                    match windows::aion2_running() {
                                        Ok(false) => prefetch::PrefetchControl::Continue,
                                        Ok(true) | Err(_) => prefetch::PrefetchControl::Stop,
                                    }
                                }
                            })
                            .map(|cache| cache.bytes());
                            let _ = tx.send(result);
                        });
                    }
                }
            }
            self.tick_training();
            self.last_refresh = Instant::now();
        }
    }

    include!("training.rs");

    impl Drop for App {
        fn drop(&mut self) {
            self.relay.stop();
            self.routes.shutdown();
            if let Some(training) = self.training.take() {
                self.restore_training(training, "Aplicativo encerrado; ajustes de teste restaurados.".into());
            }
            if let Some(cancel) = &self.aion2_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            for pid in self.applied_qos.keys().copied().collect::<Vec<_>>() {
                let _ = self.restore_qos(pid);
            }
        }
    }


    impl App {
        pub fn webview_state(&self) -> serde_json::Value {
            let pin_status = self.pinner.as_ref().and_then(Pinner::status);
            let tm = &self.telemetry.metrics;
            let mut alerts: Vec<String> = [&self.error, &self.cpu_error, &self.qos_error]
                .into_iter().filter(|s| !s.is_empty()).cloned().collect();
            if let Some(c) = self.pinner.as_ref().and_then(|p| p.conflict()) {
                alerts.push(format!("DISPUTA DE AFINIDADE: Thread {}:{} afinidade externa {:#x}; aplicada {:#x}.", c.pid, c.tid, c.external_mask, c.our_mask));
            }
            let running: Vec<_> = self.running.iter().map(|p| {
                let enabled = self.optimizer.enabled(p.pid);
                let qos = self.applied_qos.contains_key(&p.pid);
                let pinned = pin_status.is_some_and(|(pid, _, _)| pid == p.pid);
                let paused = self.training.as_ref().filter(|t| t.pid == p.pid && windows::same_executable(&t.exe, &p.exe)).map(|t| t.paused);
                serde_json::json!({
                    "pid": p.pid, "game": p.game, "exe": p.exe.display().to_string(),
                    "state": crate::view::process_state(paused, self.optimizer.boosted(p.pid), pinned, qos, enabled).label(),
                    "enabled": enabled, "auto": self.auto_games.contains(&p.directory), "qos": qos,
                    "priority": self.optimizer.boosted(p.pid),
                    "can_toggle": self.training.is_none() && self.tray.is_optimizer_running()
                })
            }).collect();
            let profile_process = self.training.as_ref().and_then(|t| self.running.iter().find(|p| p.pid == t.pid && windows::same_executable(&p.exe, &t.exe)))
                .or_else(|| self.running.iter().find(|p| p.pid == tm.target_pid))
                .or_else(|| self.selected_process());
            let profile = profile_process.map(|p| {
                let paused = self.training.as_ref().filter(|t| t.pid == p.pid && windows::same_executable(&t.exe, &p.exe)).map(|t| t.paused);
                let mut configured = self.effective_settings(p);
                if self.runtime_profile(&p.exe).is_none() && !self.auto_games.contains(&p.directory) {
                    configured.priority = self.optimizer.enabled(p.pid);
                }
                let learned = self.learned_profiles.iter().any(|saved| windows::same_executable(&saved.exe, &p.exe) && saved.settings == configured);
                let source = match paused {
                    Some(true) => "RAPO pausado · candidato preservado, sem ajustes de teste",
                    Some(false) => "RAPO · perfil sob avaliação",
                    None if learned => "RAPO · perfil aprendido salvo",
                    None if self.runtime_profile(&p.exe).is_some() => "RAPO · perfil restaurado",
                    None if configured.bits() != 0 => "Perfil manual / biblioteca Steam",
                    None => "Windows · sem perfil de ajuste",
                };
                serde_json::json!({
                    "pid": p.pid, "game": p.game, "source": source, "configured": configured,
                    "priority": self.optimizer.boosted(p.pid),
                    "pinned_threads": self.pinner.as_ref().map_or(0, |pin| pin.pinned_count(p.pid)),
                    "qos": self.applied_qos.contains_key(&p.pid)
                })
            });
            let rows = crate::snapshot::game_rows(&self.games, &self.auto_games, &self.pin_games, &self.qos_games, self.rapo_game.as_deref(), self.training.is_none());
            let rows: Vec<_> = rows.into_iter().zip(&self.game_search_names).filter(|(_, name)| crate::view::library_matches(&self.search, name)).map(|(row, _)| row).collect();
            let header = crate::ui_model::header_model(self.tray.is_optimizer_running(), &self.dtt_report.status, self.running.len());
            let mut metrics = vec![
                ("FPS DA INTERFACE", format!("{:.0} FPS · Média {:.0} · 1% baixo {:.0}", self.fps.current_fps(), self.fps.avg_fps(), self.fps.p1_low_fps())),
                ("USO DE CPU TOTAL", format!("{:.1}% · Sistema {:.1}%", tm.cpu_usage_pct, tm.sys_cpu_pct)),
                ("USO DE GPU", format!("{:.1}% · Engines PDH (estimativa)", tm.gpu_usage_pct)),
                ("USO DE MEMÓRIA", format!("{} · {:.1}% da RAM", crate::view::memory_label(tm.mem_used_mb), tm.mem_usage_pct)),
                (if tm.has_target { "I/O DE LEITURA (PROCESSO)" } else { "LEITURA DE DISCO (SISTEMA)" }, format!("{:.2} MB/s · Total {:.2} GB (inclui cache no processo)", tm.disk_read_mbs, tm.disk_total_read_gb)),
                ("REDE TOTAL (SISTEMA)", format!("{:.1} KB/s · Recebidos + enviados", tm.net_usage_kbs)),
                ("RTT TCP (EXTERNO)", format!("{} · TCP até DNS público; não é ping do jogo", if tm.ping_ms > 0 { format!("{} ms", tm.ping_ms) } else { "-- ms".into() })),
            ];
            metrics.push(("ICMP · IPs TCP DO PROCESSO", format!("{} · Não identifica servidor de partida; UDP não expõe destino na tabela Windows", tm.process_ping.summary)));
            for endpoint in &tm.process_ping.endpoints {
                metrics.push(("ICMP · DESTINO OBSERVADO", format!("{} · {}", endpoint.outcome.label(), endpoint.destination)));
                metrics.push(("ICMP · ROTA OBSERVADA", endpoint.route.as_ref().map_or_else(
                    || format!("{} · rota/origem não confirmada ao redor da sonda; não usar no aprendizado", endpoint.destination),
                    |route| format!("{} · if {} · gateway {} · origem {} · amostra {}", endpoint.destination, route.interface, route.gateway, route.source, tm.process_ping.sample_id))));
            }
            let cores: Vec<_> = self.cpu.load.iter().enumerate().map(|(index, load)| {
                (index, *load, index < usize::BITS as usize && self.pinner.as_ref().is_some_and(|p| p.mask & (1usize << index) != 0))
            }).collect();
            let mut cache: Vec<_> = self.cache_topo.levels.iter().map(|l| format!("L{} {} {}KB ({}x)", l.level, l.cache_type, l.size_kb, l.count)).collect();
            if self.cache_topo.total_l3_kb > 0 {
                cache.push(format!("L3 SMART CACHE {:.0}MB", self.cache_topo.total_l3_kb as f32 / 1024.0));
            }
            let mut network_tweaks: Vec<_> = self.netqos.applied().iter().map(|(name, dscp)| format!("DSCP {dscp} · {}", name.display())).collect();
            network_tweaks.sort();
            serde_json::json!({
                "header": { "optimizer_running": header.optimizer_running, "dtt": header.dtt, "default_config_enabled": header.default_config_enabled, "strings": &*self.ui_strings },
                "games": { "games": rows, "strings": &*self.ui_strings },
                "training": crate::snapshot::training_view(&self.training_state()),
                "dashboard": {
                    "alerts": alerts, "metrics": metrics, "cores": cores, "cache": cache, "running": running,
                    "profile": profile,
                    "network_tweaks": network_tweaks,
                    "network_priority": self.network_priority,
                    "relay": self.relay.state(),
                    "routes": self.routes.state,
                    "route_endpoints": tm.process_ping.endpoints.iter().map(|e| &e.destination).collect::<Vec<_>>(),
                    "network_target_pid": if tm.has_target { tm.target_pid } else { 0 },
                    "power_plan": { "plan": self.power_plan, "error": self.power_error },
                    "target": if tm.has_target { format!("{} · PID {}", tm.target_name, tm.target_pid) } else { "SISTEMA GLOBAL — AGUARDANDO JOGO".into() },
                    "auto_yield": self.pinner.as_ref().is_none_or(|p| p.auto_yield),
                    "windowed": self.windowed_mode, "minimize_tray": self.tray.is_minimize_to_tray(),
                    "search": self.search, "library_count": self.games.len(), "aion2_status": self.aion2_status,
                    "subtitle": self.tr("txt_app_subtitle", "APO e RAPO para jogos Steam"),
                    "pin_status": pin_status.map(|(pid, tid, ms)| format!("AFINIDADE ATIVA · PID {pid} · TID {tid} · {ms}ms · {} threads", self.pinner.as_ref().map_or(0, |p| p.pinned_count(pid))))
                }
            })
        }

        /// Advances refresh and FPS sampling; returns how soon the next frame is needed.
        pub fn tick(&mut self, minimized: bool) -> Duration {
            if self.last_refresh.elapsed() >= Duration::from_secs(1) {
                self.refresh();
            }
            self.routes.poll(&self.telemetry.metrics.process_ping);
            self.relay.poll();
            let pause_requested = self.tray.take_fps_pause_request();
            if pause_requested {
                self.fps.pause_sampling();
            }
            if !minimized && !pause_requested && !self.running.is_empty() {
                Duration::from_millis(8)
            } else {
                if self.running.is_empty() {
                    self.fps.reset();
                } else {
                    self.fps.pause_sampling();
                }
                Duration::from_secs(1)
            }
        }

        pub fn next_refresh(&self) -> Instant {
            self.last_refresh + Duration::from_secs(1)
        }

        pub fn record_ui_frame(&mut self) {
            if !self.running.is_empty() {
                self.fps.record_frame_instant(Instant::now());
            }
        }

        /// Returns true only after a successful elevated replacement has started.
        pub fn apply(&mut self, command: crate::command::Command) -> bool {
            match command {
                crate::command::Command::SetOptimizerRunning(running) => {
                    self.tray.set_optimizer_running(running);
                    self.refresh();
                }
                crate::command::Command::SetQos { directory, on } => {
                    if on {
                        self.qos_games.insert(directory.clone());
                    } else {
                        self.qos_games.remove(&directory);
                    }
                    self.clear_runtime_profiles(&directory);
                    self.refresh();
                }
                crate::command::Command::SetPin { directory, on } => {
                    if on {
                        if self.pinner.is_none() {
                            match Pinner::new() {
                                Ok(pinner) => self.pinner = Some(pinner),
                                Err(error) => self.error = error,
                            }
                        }
                        if self.pinner.is_some() {
                            self.pin_games.insert(directory.clone());
                        }
                    } else {
                        self.pin_games.remove(&directory);
                        if let Some(pinner) = &mut self.pinner {
                            if let Err(error) = pinner.update(&self.running, &self.pin_games, self.windowed_mode, &self.runtime_profiles) {
                                self.error = error;
                            }
                        }
                    }
                    self.clear_runtime_profiles(&directory);
                    self.refresh();
                }
                crate::command::Command::SetAuto { directory, on } => {
                    if on {
                        self.auto_games.insert(directory.clone());
                        if self.tray.is_optimizer_running() {
                            for process in &self.running {
                                if process.directory == directory && !self.optimizer.enabled(process.pid) {
                                    if let Err(error) = self.optimizer.toggle(process) { self.error = error; }
                                }
                            }
                            if let Some(error) = self.optimizer.update(self.windowed_mode).into_iter().next() { self.error = error; }
                        }
                    } else {
                        self.auto_games.remove(&directory);
                        for process in &self.running {
                            if process.directory == directory && self.optimizer.enabled(process.pid) {
                                if let Err(error) = self.optimizer.toggle(process) { self.error = error; }
                            }
                        }
                    }
                    self.clear_runtime_profiles(&directory);
                    self.refresh();
                }
                crate::command::Command::Refresh => self.refresh(),
                crate::command::Command::SetNetworkPriority(on) => { self.network_priority = on; self.refresh(); }
                crate::command::Command::SetRelay(on) => {
                    if on { let _ = self.relay.start(); } else { self.relay.stop(); }
                }
                crate::command::Command::SetRelayInterfaces(interfaces) => self.relay.set_interfaces(interfaces),
                crate::command::Command::RefreshRoutes => self.routes.request(crate::routes::Operation::Refresh),
                crate::command::Command::ApplyRoute { destination, candidate } => self.routes.request(crate::routes::Operation::Apply { destination, candidate }),
                crate::command::Command::RemoveRoute(destination) => self.routes.request(crate::routes::Operation::Remove(destination)),
                crate::command::Command::RemoveAllRoutes => self.routes.request(crate::routes::Operation::RemoveAll),
                crate::command::Command::StartRouteLearning { destination, candidates } => self.routes.request(crate::routes::Operation::Start {
                    destination, candidates, sample: self.telemetry.metrics.process_ping.clone(),
                }),
                crate::command::Command::StopNetworkLearning => self.routes.request(crate::routes::Operation::Stop),
                crate::command::Command::RemoveRate => self.routes.request(crate::routes::Operation::RateRemove),
                crate::command::Command::ApplyRate { pid, bps } => {
                    if let Some(process) = self.running.iter().find(|p| p.pid == pid) {
                        self.routes.request(crate::routes::Operation::RateApply { pid, exe: process.exe.clone(), bps });
                    } else { self.routes.state.error = "Alvo de saída não está mais em execução.".into(); }
                }
                crate::command::Command::StartRateLearning { pid, destination, rates } => {
                    if let Some(process) = self.running.iter().find(|p| p.pid == pid) {
                        self.routes.request(crate::routes::Operation::RateStart { pid, exe: process.exe.clone(), rates, destination, sample: self.telemetry.metrics.process_ping.clone() });
                    } else { self.routes.state.error = "Alvo de saída não está mais em execução.".into(); }
                }
                crate::command::Command::SetSearch(text) => self.search = text,
                crate::command::Command::RelaunchElevated => match windows::relaunch_elevated() {
                    Ok(()) => return true,
                    Err(error) => self.error = error,
                },
                crate::command::Command::ScanSteam => {
                    self.scan();
                    self.refresh();
                }
                crate::command::Command::ResetNetQos => {
                    self.qos_error.clear();
                    self.qos_error = match self.netqos.reset() {
                        Ok(()) => String::new(),
                        Err(error) => error,
                    };
                }
                crate::command::Command::SetAutoYield(auto_yield) => {
                    if let Some(pinner) = &mut self.pinner {
                        pinner.auto_yield = auto_yield;
                    }
                }
                crate::command::Command::SetMinimizeToTray(on) => self.tray.set_minimize_to_tray(on),
                crate::command::Command::SetRapo(directory) => {
                    self.select_rapo(directory);
                    self.refresh();
                }
                crate::command::Command::ApplyAion2Profile { directory } => {
                    if self.pinner.is_none() {
                        if let Ok(pinner) = Pinner::new() {
                            self.pinner = Some(pinner);
                        }
                    }
                    self.auto_games.insert(directory.clone());
                    if self.pinner.is_some() {
                        self.pin_games.insert(directory.clone());
                    }
                    self.qos_games.insert(directory.clone());
                    self.aion2_prefetched = false;
                    self.clear_runtime_profiles(&directory);
                    self.refresh();
                }
                crate::command::Command::SetWindowed(on) => self.windowed_mode = on,
                crate::command::Command::ToggleProcess { pid } => {
                    let Some(process) = self.running.iter().find(|p| p.pid == pid).cloned() else {
                        return false;
                    };
                    self.runtime_profiles.retain(|exe, _| !windows::same_executable(exe, &process.exe));
                    if let Err(e) = self.optimizer.toggle(&process) {
                        self.error = e;
                    } else if let Some(e) = self.optimizer.update(self.windowed_mode).into_iter().next() {
                        self.error = e;
                    }
                }
                crate::command::Command::StartTraining => self.start_training(),
                crate::command::Command::StopTraining => {
                    if let Some(training) = self.training.take() {
                        self.restore_training(
                            training,
                            "Aprendizado parado; último perfil salvo preservado. Sem melhoria salva, ajustes anteriores restaurados.".into(),
                        );
                        self.refresh();
                    }
                }
                crate::command::Command::SelectTrainingTarget { pid } => {
                    if let Some(process) = self.running.iter().find(|p| p.pid == pid) {
                        self.training_selected = Some((process.pid, process.exe.clone()));
                    }
                }
            }
            false
        }
    }
}

#[cfg(windows)]
fn export_ui_assets() -> std::io::Result<()> {
    use std::io::Write;
    let dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| std::io::Error::other("Executable path has no parent"))?
        .to_path_buf();
    for (name, content) in [
        ("ui.json", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui.json"))),
        ("ui_figma.svg", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui_figma.svg"))),
    ] {
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(name))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        file.write_all(content.as_bytes())?;
    }
    Ok(())
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().any(|arg| arg == "--export-ui-assets") {
        if let Err(error) = export_ui_assets() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if std::env::args_os().any(|arg| arg == "--generate-sha256") {
        if let Ok(exe) = std::env::current_exe() {
            match release::generate_sha256_file(&exe) {
                Ok(hash) => println!("{hash}  {}", exe.file_name().unwrap_or_default().to_string_lossy()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        return Ok(());
    }
    if let Err(error) = windows::wait_for_parent() {
        eprintln!("{error}");
        return Ok(());
    }
    let result = webview::run();
    if let Err(error) = &result {
        let message: Vec<u16> = format!("Não foi possível abrir a interface WebView2: {error}\nVerifique a instalação do Microsoft Edge WebView2 Runtime.").encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "Rintel RAPO".encode_utf16().chain(Some(0)).collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(), message.as_ptr(), title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("A interface e os controles de processos exigem Windows.");
}
