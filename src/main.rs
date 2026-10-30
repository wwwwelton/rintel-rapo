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
        tray::{self, TrayController},
        windows::{self, Optimizer, Running},
    };
    use eframe::egui;
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

    // NotMeter Design Tokens
    const COLOR_BG: egui::Color32 = egui::Color32::from_rgb(5, 12, 19); // #050c13
    const COLOR_SURFACE: egui::Color32 = egui::Color32::from_rgb(9, 21, 32); // #091520
    const COLOR_SURFACE_ELEVATED: egui::Color32 = egui::Color32::from_rgb(12, 26, 39); // #0c1a27
    const COLOR_SURFACE_HOVER: egui::Color32 = egui::Color32::from_rgb(16, 34, 48); // #102230
    const COLOR_LINE: egui::Color32 = egui::Color32::from_rgb(30, 57, 76); // #1e394c
    const COLOR_LINE_SOFT: egui::Color32 = egui::Color32::from_rgb(20, 42, 57); // #142a39
    const COLOR_TEXT: egui::Color32 = egui::Color32::from_rgb(243, 248, 251); // #f3f8fb
    const COLOR_MUTED: egui::Color32 = egui::Color32::from_rgb(141, 169, 187); // #8da9bb
    const COLOR_MUTED_2: egui::Color32 = egui::Color32::from_rgb(104, 134, 154); // #68869a
    const COLOR_ACCENT: egui::Color32 = egui::Color32::from_rgb(70, 224, 213); // #46e0d5 (Mint/Cyan)
    const COLOR_CYAN: egui::Color32 = egui::Color32::from_rgb(85, 215, 255); // #55d7ff
    const COLOR_YELLOW: egui::Color32 = egui::Color32::from_rgb(255, 217, 107); // #ffd96b
    const COLOR_DANGER: egui::Color32 = egui::Color32::from_rgb(255, 104, 119); // #ff6877

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
        last_icon_active: bool,
        theme_applied: bool,
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
        qos_sweep_error: String,
        ui_strings: Arc<HashMap<String, String>>,
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
                last_icon_active: true,
                theme_applied: false,
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
                qos_sweep_error: String::new(),
                ui_strings: Arc::new(load_ui_strings()),
            };
            match crate::learning::load_profiles() {
                Ok(profiles) => app.learned_profiles = profiles,
                Err(error) => app.training_message = error,
            }
            if let Err(error) = super::netqos::clear_all() {
                app.qos_sweep_error = error;
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

            let active: Vec<&std::path::Path> = if optimizer_active {
                self.running.iter().filter(|p| self.optimizer.enabled(p.pid)).map(|p| p.exe.as_path()).collect()
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
            if let Some(cancel) = &self.aion2_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            for pid in self.applied_qos.keys().copied().collect::<Vec<_>>() {
                let _ = self.restore_qos(pid);
            }
        }
    }

    // NotMeter Reusable UI Helper Components
    fn notmeter_card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
        egui::Frame::new()
            .fill(COLOR_SURFACE)
            .stroke(egui::Stroke::new(1.0f32, COLOR_LINE))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::same(14))
            .show(ui, add_contents)
            .inner
    }

    fn notmeter_hero_card<R>(ui: &mut egui::Ui, is_active: bool, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let stroke_color = if is_active { COLOR_ACCENT } else { COLOR_LINE };
        egui::Frame::new()
            .fill(COLOR_SURFACE_ELEVATED)
            .stroke(egui::Stroke::new(if is_active { 1.5f32 } else { 1.0f32 }, stroke_color))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(16))
            .show(ui, add_contents)
            .inner
    }

    fn notmeter_badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
        let bg = egui::Color32::from_rgba_premultiplied(
            (color.r() as f32 * 0.15) as u8,
            (color.g() as f32 * 0.15) as u8,
            (color.b() as f32 * 0.15) as u8,
            60,
        );
        egui::Frame::new()
            .fill(bg)
            .stroke(egui::Stroke::new(1.0f32, color))
            .corner_radius(egui::CornerRadius::same(5))
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(text).size(11.0).strong().color(color));
            });
    }

    impl eframe::App for App {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            if self.last_refresh.elapsed() >= Duration::from_secs(1) {
                self.refresh();
            }

            let minimized = ctx.input(|input| input.viewport().minimized == Some(true));
            let pause_requested = self.tray.take_fps_pause_request();
            if pause_requested {
                self.fps.pause_sampling();
            }
            if !minimized && !pause_requested && !self.running.is_empty() {
                self.fps.record_frame_instant(Instant::now());
                ctx.request_repaint_after(Duration::from_millis(8));
            } else {
                if self.running.is_empty() {
                    self.fps.reset();
                } else {
                    self.fps.pause_sampling();
                }
                ctx.request_repaint_after(Duration::from_secs(1));
            }

            if !self.theme_applied {
                let mut visuals = egui::Visuals::dark();
                visuals.override_text_color = Some(COLOR_TEXT);
                visuals.panel_fill = COLOR_BG;
                visuals.window_fill = COLOR_SURFACE;
                visuals.extreme_bg_color = COLOR_BG;
                visuals.faint_bg_color = COLOR_SURFACE_ELEVATED;

                visuals.widgets.inactive.bg_fill = COLOR_SURFACE_ELEVATED;
                visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0f32, COLOR_LINE);
                visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
                visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0f32, COLOR_TEXT);

                visuals.widgets.hovered.bg_fill = COLOR_SURFACE_HOVER;
                visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0f32, COLOR_ACCENT);
                visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
                visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0f32, egui::Color32::WHITE);

                visuals.widgets.active.bg_fill = COLOR_ACCENT;
                visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0f32, COLOR_ACCENT);
                visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
                visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0f32, COLOR_BG);

                visuals.selection.bg_fill = egui::Color32::from_rgba_premultiplied(70, 224, 213, 50);
                visuals.selection.stroke = egui::Stroke::new(1.0f32, COLOR_ACCENT);

                ctx.set_visuals(visuals);
                self.theme_applied = true;
            }

            // Sincronizar ícone da janela com o estado Start/Stop da bandeja
            let current_opt_running = self.tray.is_optimizer_running();
            if current_opt_running != self.last_icon_active {
                self.last_icon_active = current_opt_running;
                let (rgba, width, height) = tray::build_window_icon_rgba(current_opt_running);
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(
                    egui::IconData { rgba, width, height },
                ))));
                self.refresh();
            }

            let strings_arc = self.ui_strings.clone();
            let header = crate::snapshot::Snapshot::new(current_opt_running, &self.dtt_report.status, self.running.len()).header;
            let mut commands: Vec<crate::command::Command> = Vec::new();
            egui::CentralPanel::default().show(ctx, |ui| {
                let strings = strings_arc.as_ref();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.training_panel(ui);
                    ui.add_space(8.0);

                    // NOTMETER HEADER BAR
                    ui.horizontal(|ui| {
                        ui.heading(
                            egui::RichText::new(tr(strings, "txt_app_title", "RINTEL RAPO"))
                                .size(24.0)
                                .extra_letter_spacing(-0.5)
                                .strong()
                                .color(COLOR_TEXT),
                        );
                        notmeter_badge(ui, tr(strings, "badge_scheduling", "AGENDAMENTO WINDOWS"), COLOR_ACCENT);
                        if current_opt_running {
                            notmeter_badge(ui, tr(strings, "badge_status_active", "🟢 ATIVO (START)"), COLOR_ACCENT);
                        } else {
                            notmeter_badge(ui, tr(strings, "badge_status_paused", "⏸ PAUSADO (STOP)"), COLOR_YELLOW);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            match header.dtt {
                                crate::ui_model::DttBadge::Running => {
                                    notmeter_badge(ui, tr(strings, "badge_dtt_running", "INTEL DTT EM EXECUÇÃO"), COLOR_YELLOW);
                                }
                                crate::ui_model::DttBadge::Stopped => {
                                    notmeter_badge(ui, tr(strings, "badge_dtt_stopped", "INTEL DTT DETECTADO"), COLOR_MUTED);
                                }
                                crate::ui_model::DttBadge::NotInstalled => {
                                    notmeter_badge(ui, tr(strings, "badge_dtt_none", "DTT NÃO DETECTADO"), COLOR_ACCENT);
                                }
                            }

                            if ui.button(egui::RichText::new(tr(strings, "btn_refresh", "Atualizar")).size(12.0)).clicked() {
                                self.refresh();
                            }
                            if ui.button(egui::RichText::new(tr(strings, "btn_scan_steam", "Reexaminar Steam")).size(12.0)).clicked() {
                                self.scan();
                                self.refresh();
                            }
                            let default_enabled = header.default_config_enabled;
                            if ui.add_enabled(default_enabled, egui::Button::new(egui::RichText::new(tr(strings, "btn_default_config", "Configuração padrão")).size(12.0))).clicked() {
                                self.qos_error.clear();
                                self.qos_sweep_error = match self.netqos.reset() {
                                    Ok(()) => String::new(),
                                    Err(error) => error,
                                };
                            }
                            let toggle_label = if current_opt_running { tr(strings, "btn_stop", "⏹ Stop") } else { tr(strings, "btn_start", "▶ Start") };
                            if ui.button(egui::RichText::new(toggle_label).size(12.0).strong()).clicked() {
                                commands.push(crate::command::Command::SetOptimizerRunning(!current_opt_running));
                            }
                        });
                    });

                    ui.label(
                        egui::RichText::new(tr(strings, "txt_app_subtitle", "APO e RAPO para jogos Steam: aprendizado em tempo real, núcleos físicos, afinidade, threads e SMT/Hyper-Threading via APIs públicas do Windows"))
                            .size(12.0)
                            .color(COLOR_MUTED),
                    );
                    ui.add_space(10.0);

                    // Conflito de afinidade com qualquer outro agendador.
                    if let Some(conflict) = self.pinner.as_ref().and_then(|p| p.conflict()) {
                        notmeter_card(ui, |ui| {
                            ui.horizontal(|ui| {
                                notmeter_badge(ui, tr(strings, "badge_conflict", "DISPUTA DE AFINIDADE DETECTADA"), COLOR_DANGER);
                                ui.colored_label(
                                    COLOR_DANGER,
                                    format!(
                                        "Thread {}:{} afinidade alterada externamente para {:#x} (Máscara aplicada: {:#x}).",
                                        conflict.pid, conflict.tid, conflict.external_mask, conflict.our_mask
                                    ),
                                );
                            });
                        });
                        ui.add_space(8.0);
                    }

                    if !self.error.is_empty() {
                        notmeter_card(ui, |ui| {
                            ui.colored_label(COLOR_DANGER, &self.error);
                        });
                        ui.add_space(8.0);
                    }
                    for alert in [&self.qos_error, &self.qos_sweep_error] {
                        if !alert.is_empty() {
                            notmeter_card(ui, |ui| {
                                ui.colored_label(COLOR_DANGER, alert);
                            });
                            ui.add_space(8.0);
                        }
                    }

                    // HERO CARD: PROCESSO SENDO OTIMIZADO ATIVAMENTE
                    let pin_status = self.pinner.as_ref().and_then(Pinner::status);
                    let active_proc = self.running.iter().find(|p| {
                        self.optimizer.boosted(p.pid)
                            || self.applied_qos.contains_key(&p.pid)
                            || pin_status.is_some_and(|(pid, _, _)| pid == p.pid)
                    });

                    notmeter_hero_card(ui, active_proc.is_some() || self.training.as_ref().is_some_and(|t| !t.paused), |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        if let Some(training) = &self.training {
                            let candidate = training.winner.map_or_else(|| training.search.current(), |winner| winner.0);
                            ui.label(egui::RichText::new(if training.paused {
                                tr(strings, "badge_rapo_paused", "RAPO PAUSADO — candidato preservado")
                            } else if candidate.bits() == 0 {
                                tr(strings, "badge_rapo_baseline", "RAPO ATIVO — medindo referência sem ajustes")
                            } else {
                                tr(strings, "badge_rapo_candidate", "RAPO ATIVO — medindo candidato")
                            }).size(16.0).strong().color(if training.paused { COLOR_YELLOW } else { COLOR_ACCENT }));
                            ui.label(format!("{} · PID {} · {} medições confirmadas", training.exe.display(), training.pid, training.search.completed()));
                            ui.label(format!("Prioridade {} · afinidade {} · até {} núcleos físicos · até {} threads críticas · SMT {} · somente P-cores {} · memória/EcoQoS {}", candidate.priority, candidate.pin, candidate.cores, candidate.threads, candidate.smt, candidate.p_only, candidate.qos));
                            if training.paused {
                                ui.label(tr(strings, "txt_rapo_paused_note", "Sem captura e sem ajustes de teste no alvo. Volte ao mesmo jogo e mantenha a otimização em Start para retomar este candidato."));
                            }
                        } else if let Some(directory) = &self.rapo_game {
                            ui.label(egui::RichText::new(tr(strings, "badge_rapo_armed", "RAPO ARMADO — aguardando o jogo em foco")).size(16.0).strong().color(COLOR_YELLOW));
                            ui.label(directory.display().to_string());
                        } else {
                        match active_proc {
                            Some(proc) => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(tr(strings, "badge_optimizing", "🟢 PROCESSO EM OTIMIZAÇÃO ATIVA"))
                                            .strong()
                                            .color(COLOR_ACCENT),
                                    );
                                    ui.label(
                                        egui::RichText::new(&proc.game)
                                            .size(18.0)
                                            .strong()
                                            .color(COLOR_TEXT),
                                    );
                                    notmeter_badge(ui, &format!("PID {}", proc.pid), COLOR_CYAN);
                                });
                                ui.add_space(6.0);

                                let has_cache_qos = self.applied_qos.contains_key(&proc.pid);

                                ui.horizontal_wrapped(|ui| {
                                    if self.optimizer.boosted(proc.pid) {
                                        notmeter_badge(ui, tr(strings, "badge_priority_boost", "PRIORIDADE ACIMA DO NORMAL"), COLOR_ACCENT);
                                    }
                                    if let Some((pid, tid, ms)) = pin_status {
                                        if pid == proc.pid {
                                            let count = self.pinner.as_ref().map_or(1, |p| p.pinned_count(proc.pid));
                                            notmeter_badge(ui, &format!("AFINIDADE ATIVA ({count} THREADS · TID {tid} · {ms}ms)"), COLOR_YELLOW);
                                        }
                                    }
                                    if has_cache_qos {
                                        notmeter_badge(ui, tr(strings, "badge_qos_memory", "PRIORIDADE DE MEMÓRIA / ECOQOS"), COLOR_CYAN);
                                    }
                                    if self.windowed_mode {
                                        notmeter_badge(ui, tr(strings, "badge_background_mode", "MODO SEGUNDO PLANO"), COLOR_MUTED);
                                    }
                                });

                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(format!("Caminho: {}", proc.exe.display())).size(11.0).color(COLOR_MUTED_2));
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(tr(strings, "badge_rapo_disabled", "RAPO DESATIVADO — nenhum perfil em teste")).size(14.0).strong().color(COLOR_MUTED));
                                    if !self.running.is_empty() {
                                        ui.label(egui::RichText::new(tr(strings, "txt_rapo_hint_running", "— Marque RAPO ao lado de Perfil APO na biblioteca para iniciar o aprendizado")).size(12.0).color(COLOR_MUTED_2));
                                    } else {
                                        ui.label(egui::RichText::new(tr(strings, "txt_rapo_hint_idle", "— Marque RAPO na biblioteca; o aprendizado aguardará o jogo em foco")).size(12.0).color(COLOR_MUTED_2));
                                    }
                                });
                            }
                        }
                        }
                    });

                    ui.add_space(10.0);

                    let tm = &self.telemetry.metrics;

                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(if tm.has_target {
                                    format!("TELEMETRIA DO PROGRAMA / JOGO EM EXECUÇÃO: {}", tm.target_name)
                                } else {
                                    "TELEMETRIA DO PROGRAMA / JOGO (MODO GLOBAL — AGUARDANDO JOGO)".to_string()
                                })
                                .size(14.0)
                                .strong()
                                .color(COLOR_TEXT),
                            );
                            if tm.has_target {
                                notmeter_badge(ui, &format!("PID {}", tm.target_pid), COLOR_ACCENT);
                            } else {
                                notmeter_badge(ui, tr(strings, "badge_telemetry_global", "SISTEMA GLOBAL"), COLOR_MUTED);
                            }
                        });
                        ui.add_space(8.0);
                        let current_fps = self.fps.current_fps();
                        let avg_fps = self.fps.avg_fps();
                        let p1_low_fps = self.fps.p1_low_fps();
                        ui.horizontal(|ui| {
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_ACCENT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_fps_ui", "FPS DA INTERFACE")).size(10.0).strong().color(COLOR_ACCENT));
                                        ui.label(egui::RichText::new(format!("{:.0} FPS", current_fps)).size(22.0).strong().color(COLOR_TEXT));
                                        ui.label(egui::RichText::new(format!("Média {:.0} · 1% baixo {:.0}", avg_fps, p1_low_fps)).size(10.0).color(COLOR_MUTED));
                                    });
                                });
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {

                            // 2. Uso de CPU Total
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_cpu_total", "USO DE CPU TOTAL")).size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(format!("{:.1}%", tm.cpu_usage_pct)).size(22.0).strong().color(COLOR_ACCENT));
                                        ui.label(egui::RichText::new(format!("Sistema: {:.1}%", tm.sys_cpu_pct)).size(10.0).color(COLOR_MUTED_2));
                                    });
                                });

                            // 3. Uso de GPU
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_gpu_usage", "USO DE GPU")).size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(format!("{:.1}%", tm.gpu_usage_pct)).size(22.0).strong().color(COLOR_CYAN));
                                        ui.label(egui::RichText::new("Engines PDH (estimativa)").size(10.0).color(COLOR_MUTED_2));
                                    });
                                });

                            // 4. Uso de Memória
                            let mem_str = crate::view::memory_label(tm.mem_used_mb);
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_mem_usage", "USO DE MEMÓRIA")).size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(mem_str).size(22.0).strong().color(COLOR_TEXT));
                                        ui.label(egui::RichText::new(format!("{:.1}% da RAM", tm.mem_usage_pct)).size(10.0).color(COLOR_MUTED_2));
                                    });
                                });
                        });
                        ui.horizontal(|ui| {

                            // 5. Leitura de Disco (MB/s)
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(if tm.has_target { tr(strings, "lbl_disk_read_proc", "I/O DE LEITURA (PROCESSO)") } else { tr(strings, "lbl_disk_read", "LEITURA DE DISCO (SISTEMA)") }).size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(format!("{:.2} MB/s", tm.disk_read_mbs)).size(22.0).strong().color(COLOR_YELLOW));
                                        let disk_sub = if tm.has_target {
                                            format!("Total: {:.2} GB de I/O lido (inclui cache)", tm.disk_total_read_gb)
                                        } else {
                                            "Leitura do disco físico".to_string()
                                        };
                                        ui.label(egui::RichText::new(disk_sub).size(10.0).color(COLOR_MUTED_2));
                                    });
                                });

                            // 6. Uso de Internet (KB/s)
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_net_total", "REDE TOTAL (SISTEMA)")).size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(format!("{:.1} KB/s", tm.net_usage_kbs)).size(22.0).strong().color(COLOR_CYAN));
                                        ui.label(egui::RichText::new("Recebidos + enviados").size(10.0).color(COLOR_MUTED_2));
                                    });
                                });

                            // 7. Ping (ms)
                            let ping_color = if tm.ping_ms == 0 {
                                COLOR_MUTED
                            } else if tm.ping_ms < 60 {
                                COLOR_ACCENT
                            } else if tm.ping_ms < 120 {
                                COLOR_YELLOW
                            } else {
                                COLOR_DANGER
                            };
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(tr(strings, "lbl_rtt_tcp", "RTT TCP (EXTERNO)")).size(10.0).strong().color(COLOR_MUTED));
                                        let ping_str = if tm.ping_ms > 0 { format!("{} ms", tm.ping_ms) } else { "-- ms".into() };
                                        ui.label(egui::RichText::new(ping_str).size(22.0).strong().color(ping_color));
                                        ui.label(egui::RichText::new("TCP até DNS público; não é ping do jogo").size(10.0).color(COLOR_MUTED_2));
                                    });
                                });
                        });
                    });

                    ui.add_space(10.0);

                    // USO DE PROCESSADORES LÓGICOS (ESTILO HARDWARE MATRIX NOTMETER)
                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(tr(strings, "txt_cpu_matrix_title", "USO POR NÚCLEO LÓGICO")).size(14.0).strong().color(COLOR_TEXT));
                            ui.label(egui::RichText::new(tr(strings, "txt_cpu_matrix_sub", "TOPOLOGIA DETECTADA PELO WINDOWS")).size(11.0).color(COLOR_MUTED));
                        });
                        ui.add_space(8.0);

                        egui::Grid::new("notmeter_cpu_grid")
                            .num_columns(4)
                            .spacing([14.0, 8.0])
                            .show(ui, |ui| {
                                for (index, &load) in self.cpu.load.iter().enumerate() {
                                    let is_p_core = index < usize::BITS as usize
                                        && self.pinner.as_ref().is_some_and(|p| p.mask & (1usize << index) != 0);
                                    let load_color = if load > 0.85 {
                                        COLOR_DANGER
                                    } else if load > 0.50 {
                                        COLOR_YELLOW
                                    } else {
                                        COLOR_ACCENT
                                    };

                                    ui.horizontal(|ui| {
                                        ui.monospace(
                                            egui::RichText::new(format!("{index:02}"))
                                                .size(11.0)
                                                .color(if is_p_core { COLOR_CYAN } else { COLOR_MUTED }),
                                        );

                                        // Micro-gauge Notmeter style
                                        let (response, painter) = ui.allocate_painter(egui::vec2(100.0, 14.0), egui::Sense::hover());
                                        let rect = response.rect;
                                        painter.rect_filled(rect, 3.0, COLOR_SURFACE_ELEVATED);
                                        let fill_w = rect.width() * load.clamp(0.0, 1.0);
                                        painter.rect_filled(
                                            egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height())),
                                            3.0,
                                            load_color,
                                        );

                                        ui.monospace(
                                            egui::RichText::new(format!("{:.0}%", load * 100.0))
                                                .size(11.0)
                                                .strong()
                                                .color(COLOR_TEXT),
                                        );
                                    });

                                    if index % 4 == 3 {
                                        ui.end_row();
                                    }
                                }
                            });
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            for lvl in &self.cache_topo.levels {
                                notmeter_badge(
                                    ui,
                                    &format!("L{} {} {}KB ({}x)", lvl.level, lvl.cache_type, lvl.size_kb, lvl.count),
                                    COLOR_MUTED,
                                );
                            }
                            if self.cache_topo.total_l3_kb > 0 {
                                notmeter_badge(
                                    ui,
                                    &format!("L3 SMART CACHE {:.0}MB", self.cache_topo.total_l3_kb as f32 / 1024.0),
                                    COLOR_CYAN,
                                );
                            }
                        });
                    });

                    ui.add_space(10.0);

                    // PAINEL DE CONTROLES E OPÇÕES EM TEMPO REAL
                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.label(egui::RichText::new(tr(strings, "txt_options_title", "OPÇÕES E DIRETRIZES DE AGENDAMENTO")).size(14.0).strong().color(COLOR_TEXT));
                        ui.add_space(6.0);

                        ui.horizontal_wrapped(|ui| {
                            let mut auto_yield = self.pinner.as_ref().is_none_or(|p| p.auto_yield);
                            if ui.checkbox(&mut auto_yield, egui::RichText::new(tr(strings, "chk_auto_yield", "Auto-Yield (afinidade)")).color(COLOR_TEXT))
                                .on_hover_text(tr(strings, "chk_auto_yield_tooltip", "Cede afinidade se outro programa mudar a máscara de uma thread já fixada"))
                                .changed()
                            {
                                if let Some(pinner) = &mut self.pinner {
                                    pinner.auto_yield = auto_yield;
                                }
                            }

                            ui.checkbox(&mut self.windowed_mode, egui::RichText::new(tr(strings, "chk_windowed_mode", "Manter ajustes em segundo plano")).color(COLOR_TEXT))
                                .on_hover_text(tr(strings, "chk_windowed_mode_tooltip", "Mantém prioridade e afinidade do jogo mesmo fora de foco; pode prejudicar outros aplicativos"));


                            let mut min_tray = self.tray.is_minimize_to_tray();
                            if ui.checkbox(&mut min_tray, egui::RichText::new(tr(strings, "chk_minimize_tray", "Minimizar para a Barra de Ícones (Tray)")).color(COLOR_TEXT))
                                .on_hover_text(tr(strings, "chk_minimize_tray_tooltip", "Ao minimizar a janela, oculta da barra de tarefas e mantém o ícone interativo na bandeja do relógio (com menu Start/Stop)"))
                                .changed()
                            {
                                self.tray.set_minimize_to_tray(min_tray);
                            }

                            if ui.button(tr(strings, "btn_reopen_admin", "Reabrir como administrador"))
                                .on_hover_text(tr(strings, "btn_reopen_admin_tooltip", "Solicita UAC somente agora. As opções deverão ser selecionadas novamente; processos protegidos ainda podem negar acesso."))
                                .clicked()
                            {
                                match windows::relaunch_elevated() {
                                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                    Err(error) => self.error = error,
                                }
                            }
                        });
                    });



                    // JOGOS RODANDO (LISTA NOTMETER STYLE)
                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{}: {}", tr(strings, "txt_running_games_title", "JOGOS DETECTADOS EM EXECUÇÃO"), self.running.len())).size(14.0).strong().color(COLOR_TEXT));
                        });
                        ui.add_space(8.0);

                        if self.running.is_empty() {
                            ui.label(egui::RichText::new(tr(strings, "txt_running_empty", "Nenhum processo dentro das pastas dos jogos Steam instalado em execução.")).size(12.0).color(COLOR_MUTED));
                        }

                        for process in &self.running {
                            let enabled = self.optimizer.enabled(process.pid);
                            let boosted = self.optimizer.boosted(process.pid);
                            let has_qos = self.applied_qos.contains_key(&process.pid);
                            let is_pinned = pin_status.is_some_and(|(pid, _, _)| pid == process.pid);

                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let training_paused = self.training.as_ref().filter(|t| windows::same_executable(&t.exe, &process.exe) && t.pid == process.pid).map(|t| t.paused);
                                        let state = crate::view::process_state(training_paused, boosted, is_pinned, has_qos, enabled);
                                        let tone_color = match state.tone() {
                                            crate::view::Tone::Accent => COLOR_ACCENT,
                                            crate::view::Tone::Yellow => COLOR_YELLOW,
                                            crate::view::Tone::Danger => COLOR_DANGER,
                                            crate::view::Tone::Muted => COLOR_MUTED,
                                        };
                                        notmeter_badge(ui, state.label(), tone_color);
                                        ui.label(egui::RichText::new(&process.game).size(15.0).strong().color(COLOR_TEXT));
                                        notmeter_badge(ui, &format!("PID {}", process.pid), COLOR_CYAN);

                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if self.auto_games.contains(&process.directory) {
                                                notmeter_badge(ui, "AUTO ATIVO", COLOR_ACCENT);
                                            } else if ui.add_enabled(
                                                self.training.is_none() && self.tray.is_optimizer_running(),
                                                egui::Button::new(if enabled { tr(strings, "btn_proc_restore", "Restaurar") } else { tr(strings, "btn_proc_optimize", "Otimizar") }),
                                            ).clicked() {
                                                self.runtime_profiles.retain(|exe, _| !windows::same_executable(exe, &process.exe));
                                                if let Err(e) = self.optimizer.toggle(process) {
                                                    self.error = e;
                                                } else if let Some(e) = self.optimizer.update(self.windowed_mode).into_iter().next() {
                                                    self.error = e;
                                                }
                                            }

                                            if has_qos {
                                                notmeter_badge(ui, "QOS MEMÓRIA", COLOR_CYAN);
                                            }
                                        });
                                    });
                                    ui.add_space(2.0);
                                    ui.label(egui::RichText::new(process.exe.display().to_string()).size(11.0).color(COLOR_MUTED_2));
                                });
                            ui.add_space(6.0);
                        }
                    });

                    ui.add_space(10.0);

                    // BIBLIOTECA STEAM COM FILTRO DE BUSCA
                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{}: {}", tr(strings, "txt_library_title", "BIBLIOTECA STEAM"), self.games.len())).size(14.0).strong().color(COLOR_TEXT));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.search)
                                        .hint_text(tr(strings, "hint_search_game", "Buscar jogo..."))
                                        .desired_width(220.0),
                                );
                            });
                        });
                        ui.add_space(8.0);

                        let mut needs_refresh = false;
                        let mut manual_directories = Vec::new();
                        let mut rapo_change = None;

                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for (game, search_name) in
                                self.games.iter().zip(&self.game_search_names)
                            {
                                if crate::view::library_matches(&self.search, search_name) {
                                    egui::Frame::new()
                                        .fill(COLOR_SURFACE_ELEVATED)
                                        .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                        .corner_radius(egui::CornerRadius::same(6))
                                        .inner_margin(egui::Margin::symmetric(12, 8))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(egui::RichText::new(&game.name).size(14.0).strong().color(COLOR_TEXT));
                                                if game.is_aion2 {
                                                    notmeter_badge(ui, "PERFIL APO UE5", COLOR_CYAN);
                                                    if !self.aion2_status.is_empty() {
                                                        notmeter_badge(ui, &self.aion2_status, COLOR_ACCENT);
                                                    }
                                                }
                                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                    ui.add_enabled_ui(self.training.is_none(), |ui| {
                                                    let mut qos = self.qos_games.contains(&game.directory);
                                                    if ui.checkbox(&mut qos, egui::RichText::new("Prioridade de memória / EcoQoS").color(COLOR_TEXT)).changed() {
                                                        manual_directories.push(game.directory.clone());
                                                        if qos {
                                                            self.qos_games.insert(game.directory.clone());
                                                        } else {
                                                            self.qos_games.remove(&game.directory);
                                                        }
                                                        needs_refresh = true;
                                                    }

                                                    // Toggle P-cores
                                                    let mut pin = self.pin_games.contains(&game.directory);
                                                    if ui.checkbox(&mut pin, egui::RichText::new(tr(strings, "chk_game_pcores", "P-cores")).color(COLOR_TEXT)).changed() {
                                                        manual_directories.push(game.directory.clone());
                                                        needs_refresh = true;
                                                        if pin {
                                                            if self.pinner.is_none() {
                                                                match Pinner::new() {
                                                                    Ok(pinner) => self.pinner = Some(pinner),
                                                                    Err(error) => self.error = error,
                                                                }
                                                            }
                                                            if self.pinner.is_some() {
                                                                self.pin_games.insert(game.directory.clone());
                                                            }
                                                        } else {
                                                            self.pin_games.remove(&game.directory);
                                                            if let Some(pinner) = &mut self.pinner {
                                                                if let Err(error) = pinner.update(&self.running, &self.pin_games, self.windowed_mode, &self.runtime_profiles) {
                                                                    self.error = error;
                                                                }
                                                            }
                                                        }
                                                    }

                                                    // Toggle Auto Prioridade
                                                    let mut auto = self.auto_games.contains(&game.directory);
                                                    if ui.checkbox(&mut auto, egui::RichText::new(tr(strings, "chk_game_auto_priority", "Auto Prioridade")).color(COLOR_TEXT)).changed() {
                                                        manual_directories.push(game.directory.clone());
                                                        if auto {
                                                            self.auto_games.insert(game.directory.clone());
                                                            if self.tray.is_optimizer_running() {
                                                                for process in &self.running {
                                                                    if process.directory == game.directory && !self.optimizer.enabled(process.pid) {
                                                                        if let Err(error) = self.optimizer.toggle(process) { self.error = error; }
                                                                    }
                                                                }
                                                                if let Some(error) = self.optimizer.update(self.windowed_mode).into_iter().next() { self.error = error; }
                                                            }
                                                        } else {
                                                            self.auto_games.remove(&game.directory);
                                                            for process in &self.running {
                                                                if process.directory == game.directory && self.optimizer.enabled(process.pid) {
                                                                    if let Err(error) = self.optimizer.toggle(process) { self.error = error; }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    });
                                                    let mut rapo = self.rapo_game.as_ref() == Some(&game.directory);
                                                    if ui.checkbox(&mut rapo, tr(strings, "chk_rapo_library", "RAPO"))
                                                        .on_hover_text(tr(strings, "chk_rapo_tooltip", "Aprendizado adaptativo; pausa ao sair e retoma o mesmo candidato ao voltar. Um jogo por vez."))
                                                        .changed() {
                                                        rapo_change = Some(if rapo { Some(game.directory.clone()) } else { None });
                                                    }
                                                    if game.is_aion2 && ui.add_enabled(self.training.is_none(), egui::Button::new(tr(strings, "btn_apo_profile", "Perfil APO"))).on_hover_text(tr(strings, "btn_apo_profile_tooltip", "Ativa Auto Prioridade, P-cores (4 threads críticas UE5, 5%, 10s), Prioridade de memória / EcoQoS e prefetch somente leitura")).clicked() {
                                                        manual_directories.push(game.directory.clone());
                                                        if self.pinner.is_none() {
                                                            if let Ok(pinner) = Pinner::new() {
                                                                self.pinner = Some(pinner);
                                                            }
                                                        }
                                                        self.auto_games.insert(game.directory.clone());
                                                        if self.pinner.is_some() {
                                                            self.pin_games.insert(game.directory.clone());
                                                        }
                                                        self.qos_games.insert(game.directory.clone());
                                                        self.aion2_prefetched = false;
                                                        needs_refresh = true;
                                                    }
                                                });
                                            });
                                        });
                                    ui.add_space(4.0);
                                }
                            }
                        });
                        for directory in manual_directories {
                            self.clear_runtime_profiles(&directory);
                        }
                        if let Some(directory) = rapo_change {
                            self.select_rapo(directory);
                            needs_refresh = true;
                        }

                        if needs_refresh {
                            self.refresh();
                        }
                    });
                });
            });
            for command in commands {
                self.apply(command);
            }
        }
    }

    impl App {
        fn apply(&mut self, command: crate::command::Command) {
            match command {
                crate::command::Command::SetOptimizerRunning(running) => {
                    self.tray.set_optimizer_running(running);
                    self.refresh();
                }
            }
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
fn main() -> eframe::Result {
    #[cfg(windows)]
    if std::env::args_os().any(|arg| arg == "--webview-preview") {
        let header = r#"{"optimizer_running":true,"dtt":"stopped","default_config_enabled":true,"strings":{"txt_app_title":"RINTEL RAPO","badge_scheduling":"AGENDAMENTO WINDOWS","badge_status_active":"ATIVO (START)","badge_status_paused":"PAUSADO (STOP)","badge_dtt_stopped":"INTEL DTT DETECTADO","btn_refresh":"Atualizar","btn_scan_steam":"Reexaminar Steam","btn_default_config":"Configuração padrão","btn_stop":"Stop"}}"#;
        if let Err(error) = webview::run_preview(header) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
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
    let (rgba, width, height) = tray::build_window_icon_rgba(true);
    eframe::run_native(
        "Rintel RAPO",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 840.0])
                .with_min_inner_size([960.0, 680.0])
                .with_title("Rintel RAPO")
                .with_icon(std::sync::Arc::new(eframe::egui::IconData { rgba, width, height })),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(app::App::new()))),
    )
}

#[cfg(not(windows))]
fn main() {
    eprintln!("A interface e os controles de processos exigem Windows.");
}
