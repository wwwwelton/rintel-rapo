#![cfg_attr(not(test), windows_subsystem = "windows")]

pub mod cache;
pub mod hardware;
pub mod intel_dtt;
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
mod app {
    use super::{
        cache::{self, MemoryQosState},
        cpu::Monitor,
        hardware::{self, CacheTopology},
        intel_dtt::{self, IntelDttReport, IntelDttStatus},
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
        sync::mpsc::Receiver,
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
        aion2_root: String,
        aion2_status: String,
        aion2_worker: Option<Receiver<Result<u64, String>>>,
        cpu_error: String,
        error: String,
        last_refresh: Instant,
        dtt_report: IntelDttReport,
        cache_topo: CacheTopology,
        telemetry: TelemetryCollector,
        tray: TrayController,
        last_icon_active: bool,
        theme_applied: bool,
    }

    impl App {
        pub fn new() -> Self {
            let mut app = Self {
                games: Vec::new(),
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
                aion2_root: String::new(),
                aion2_status: String::new(),
                aion2_worker: None,
                cpu: Monitor::new(),
                cpu_error: String::new(),
                last_refresh: Instant::now() - Duration::from_secs(3),
                dtt_report: intel_dtt::detect_intel_dtt(),
                cache_topo: hardware::get_cache_topology(),
                telemetry: TelemetryCollector::new(),
                tray: TrayController::new(),
                last_icon_active: true,
                theme_applied: false,
            };
            app.scan();
            app.refresh();
            app
        }

        fn scan(&mut self) {
            self.dtt_report = intel_dtt::detect_intel_dtt();
            match windows::steam_root() {
                Some(root) => {
                    self.games = steam::discover(&root);
                    if self.aion2_root.trim().is_empty() {
                        if let Some(game) = self.games.iter().find(|game| game.is_aion2) {
                            self.aion2_root = game.directory.display().to_string();
                        }
                    }
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
                    self.error = "Steam não encontrado no registro do usuário".into();
                }
            }
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

            match windows::running(&self.games) {
                Ok(processes) => {
                    self.running = processes;
                    self.error.clear();
                    if optimizer_active {
                        for process in &self.running {
                            if self.auto_games.contains(&process.directory)
                                && !self.optimizer.enabled(process.pid)
                            {
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
                Err(e) => self.error = e,
            }

            if optimizer_active {
                if let Some(error) = self.optimizer.update(self.windowed_mode).into_iter().next() {
                    self.error = error;
                }
            }

            let stale: Vec<u32> = self.applied_qos.keys().copied().filter(|pid| {
                !optimizer_active || !self.running.iter().any(|p|
                    p.pid == *pid && self.qos_games.contains(&p.directory))
            }).collect();
            for pid in stale {
                if let Err(error) = self.restore_qos(pid) {
                    self.error = error;
                }
            }
            if optimizer_active {
                for process in &self.running {
                    if !self.qos_games.contains(&process.directory) {
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
                        if !self.applied_qos.contains_key(&process.pid) {
                            match cache::apply_memory_qos(handle) {
                                Ok(state) => { self.applied_qos.insert(process.pid, state); }
                                Err(error) => self.error = error,
                            }
                        }
                        CloseHandle(handle);
                    }
                }
            }

            if let Some(pinner) = &mut self.pinner {
                if optimizer_active {
                    if let Err(error) = pinner.update(&self.running, &self.pin_games, self.windowed_mode) {
                        self.error = error;
                    }
                } else {
                    let _ = pinner.update(&[], &HashSet::new(), false);
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

            if let Some(rx) = &self.aion2_worker {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(bytes) => {
                            self.aion2_status = format!("Cache aquecido: {:.1} MB lidos (somente leitura)", bytes as f64 / 1048576.0);
                        }
                        Err(error) => {
                            self.aion2_status.clear();
                            self.error = error;
                        }
                    }
                    self.aion2_worker = None;
                }
            }
            self.last_refresh = Instant::now();
        }
    }

    impl Drop for App {
        fn drop(&mut self) {
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
            ctx.request_repaint_after(Duration::from_secs(1));

            if self.last_refresh.elapsed() >= Duration::from_secs(1) {
                self.refresh();
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

            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(8.0);

                    // NOTMETER HEADER BAR
                    ui.horizontal(|ui| {
                        ui.heading(
                            egui::RichText::new("RINTEL RAPO")
                                .size(24.0)
                                .extra_letter_spacing(-0.5)
                                .strong()
                                .color(COLOR_TEXT),
                        );
                        notmeter_badge(ui, "AGENDAMENTO WINDOWS", COLOR_ACCENT);
                        if current_opt_running {
                            notmeter_badge(ui, "🟢 ATIVO (START)", COLOR_ACCENT);
                        } else {
                            notmeter_badge(ui, "⏸ PAUSADO (STOP)", COLOR_YELLOW);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            match self.dtt_report.status {
                                IntelDttStatus::ActiveRunning => {
                                    notmeter_badge(ui, "INTEL DTT EM EXECUÇÃO", COLOR_YELLOW);
                                }
                                IntelDttStatus::InstalledStopped => {
                                    notmeter_badge(ui, "INTEL DTT DETECTADO", COLOR_MUTED);
                                }
                                IntelDttStatus::NotInstalled => {
                                    notmeter_badge(ui, "DTT NÃO DETECTADO", COLOR_ACCENT);
                                }
                            }

                            if ui.button(egui::RichText::new("Atualizar").size(12.0)).clicked() {
                                self.refresh();
                            }
                            if ui.button(egui::RichText::new("Reexaminar Steam").size(12.0)).clicked() {
                                self.scan();
                                self.refresh();
                            }
                            let toggle_label = if current_opt_running { "⏹ Stop" } else { "▶ Start" };
                            if ui.button(egui::RichText::new(toggle_label).size(12.0).strong()).clicked() {
                                self.tray.set_optimizer_running(!current_opt_running);
                                self.refresh();
                            }
                        });
                    });

                    ui.label(
                        egui::RichText::new("Agendamento opcional para jogos Steam: prioridade, afinidade P-core e prioridade de memória (APIs públicas do Windows)")
                            .size(12.0)
                            .color(COLOR_MUTED),
                    );
                    ui.add_space(10.0);

                    // Conflito de afinidade com qualquer outro agendador.
                    if let Some(conflict) = self.pinner.as_ref().and_then(|p| p.conflict()) {
                        notmeter_card(ui, |ui| {
                            ui.horizontal(|ui| {
                                notmeter_badge(ui, "DISPUTA DE AFINIDADE DETECTADA", COLOR_DANGER);
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

                    // HERO CARD: PROCESSO SENDO OTIMIZADO ATIVAMENTE
                    let pin_status = self.pinner.as_ref().and_then(Pinner::status);
                    let active_proc = self.running.iter().find(|p| {
                        self.optimizer.boosted(p.pid)
                            || self.applied_qos.contains_key(&p.pid)
                            || pin_status.is_some_and(|(pid, _, _)| pid == p.pid)
                    });

                    notmeter_hero_card(ui, active_proc.is_some(), |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        match active_proc {
                            Some(proc) => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("🟢 PROCESSO EM OTIMIZAÇÃO ATIVA")
                                            .size(13.0)
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
                                        notmeter_badge(ui, "PRIORIDADE ACIMA DO NORMAL", COLOR_ACCENT);
                                    }
                                    if let Some((pid, tid, ms)) = pin_status {
                                        if pid == proc.pid {
                                            notmeter_badge(ui, &format!("P-CORE ATIVO (TID {tid} · {ms}ms)"), COLOR_YELLOW);
                                        }
                                    }
                                    if has_cache_qos {
                                        notmeter_badge(ui, "PRIORIDADE DE MEMÓRIA / ECOQOS", COLOR_CYAN);
                                    }
                                    if self.windowed_mode {
                                        notmeter_badge(ui, "MODO SEGUNDO PLANO", COLOR_MUTED);
                                    }
                                });

                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(format!("Caminho: {}", proc.exe.display())).size(11.0).color(COLOR_MUTED_2));
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("⚪ Nenhum processo em foco sendo otimizado no momento").size(14.0).strong().color(COLOR_MUTED));
                                    if !self.running.is_empty() {
                                        ui.label(egui::RichText::new("— Coloque a janela do jogo em foco para engajar as otimizações").size(12.0).color(COLOR_MUTED_2));
                                    } else {
                                        ui.label(egui::RichText::new("— Aguardando execução de jogos instalados na biblioteca Steam").size(12.0).color(COLOR_MUTED_2));
                                    }
                                });
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
                                notmeter_badge(ui, "SISTEMA GLOBAL", COLOR_MUTED);
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {

                            // 2. Uso de CPU Total
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new("USO DE CPU TOTAL").size(10.0).strong().color(COLOR_MUTED));
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
                                        ui.label(egui::RichText::new("USO DE GPU").size(10.0).strong().color(COLOR_MUTED));
                                        ui.label(egui::RichText::new(format!("{:.1}%", tm.gpu_usage_pct)).size(22.0).strong().color(COLOR_CYAN));
                                        ui.label(egui::RichText::new("Engines PDH (estimativa)").size(10.0).color(COLOR_MUTED_2));
                                    });
                                });

                            // 4. Uso de Memória
                            let mem_str = if tm.mem_used_mb >= 1024.0 {
                                format!("{:.2} GB", tm.mem_used_mb / 1024.0)
                            } else {
                                format!("{:.0} MB", tm.mem_used_mb)
                            };
                            egui::Frame::new()
                                .fill(COLOR_SURFACE_ELEVATED)
                                .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(14, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new("USO DE MEMÓRIA").size(10.0).strong().color(COLOR_MUTED));
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
                                        ui.label(egui::RichText::new(if tm.has_target { "I/O DE LEITURA (PROCESSO)" } else { "LEITURA DE DISCO (SISTEMA)" }).size(10.0).strong().color(COLOR_MUTED));
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
                                        ui.label(egui::RichText::new("REDE TOTAL (SISTEMA)").size(10.0).strong().color(COLOR_MUTED));
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
                                        ui.label(egui::RichText::new("RTT TCP (EXTERNO)").size(10.0).strong().color(COLOR_MUTED));
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
                            ui.label(egui::RichText::new("USO POR NÚCLEO LÓGICO").size(14.0).strong().color(COLOR_TEXT));
                            ui.label(egui::RichText::new("TOPOLOGIA DETECTADA PELO WINDOWS").size(11.0).color(COLOR_MUTED));
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
                        ui.label(egui::RichText::new("OPÇÕES E DIRETRIZES DE AGENDAMENTO").size(14.0).strong().color(COLOR_TEXT));
                        ui.add_space(6.0);

                        ui.horizontal_wrapped(|ui| {
                            let mut auto_yield = self.pinner.as_ref().map_or(true, |p| p.auto_yield);
                            if ui.checkbox(&mut auto_yield, egui::RichText::new("Auto-Yield (afinidade)").color(COLOR_TEXT))
                                .on_hover_text("Cede afinidade se outro programa mudar a máscara de uma thread já fixada")
                                .changed()
                            {
                                if let Some(pinner) = &mut self.pinner {
                                    pinner.auto_yield = auto_yield;
                                }
                            }

                            ui.checkbox(&mut self.windowed_mode, egui::RichText::new("Manter ajustes em segundo plano").color(COLOR_TEXT))
                                .on_hover_text("Mantém prioridade e afinidade do jogo mesmo fora de foco; pode prejudicar outros aplicativos");


                            let mut min_tray = self.tray.is_minimize_to_tray();
                            if ui.checkbox(&mut min_tray, egui::RichText::new("Minimizar para a Barra de Ícones (Tray)").color(COLOR_TEXT))
                                .on_hover_text("Ao minimizar a janela, oculta da barra de tarefas e mantém o ícone interativo na bandeja do relógio (com menu Start/Stop)")
                                .changed()
                            {
                                self.tray.set_minimize_to_tray(min_tray);
                            }

                            if ui.button("Reabrir como administrador")
                                .on_hover_text("Solicita UAC somente agora. As opções deverão ser selecionadas novamente; processos protegidos ainda podem negar acesso.")
                                .clicked()
                            {
                                match windows::relaunch_elevated() {
                                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                    Err(error) => self.error = error,
                                }
                            }
                        });
                    });

                    ui.add_space(10.0);
                    notmeter_card(ui, |ui| {
                        ui.label(egui::RichText::new("PERFIL APO SEGURO — AION 2 (UE5 / SEM HOOK)").size(14.0).strong().color(COLOR_TEXT));
                        ui.label("Baseado nos perfis UE5 de apo.config (HookEnabled=false, IBTSupported=false, AppCompatDatabaseEnabled=false, ECoresEnabled=true, CriticalThreadsAffinitize=4 com limiar 5% e intervalo 10s). O botão abaixo faz prefetch somente leitura de até 4 MB de AION2.exe e 28 MB de .utoc antes de iniciar o jogo.");
                        ui.horizontal_wrapped(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.aion2_root)
                                    .hint_text("Pasta do AION 2 (Steam ou PURPLE)")
                                    .desired_width(420.0),
                            );
                            if ui
                                .add_enabled(
                                    self.aion2_worker.is_none() && !self.aion2_root.trim().is_empty(),
                                    egui::Button::new(if self.aion2_worker.is_some() {
                                        "Aquecendo..."
                                    } else {
                                        "Pré-aquecer AION 2"
                                    }),
                                )
                                .clicked()
                            {
                                match windows::aion2_running() {
                                    Ok(true) => self.error = "Feche o AION 2 antes do prefetch".into(),
                                    Ok(false) => {
                                        let root = PathBuf::from(self.aion2_root.trim());
                                        let (tx, rx) = std::sync::mpsc::channel();
                                        self.error.clear();
                                        self.aion2_status = "Aquecendo índices .utoc e AION2.exe...".into();
                                        self.aion2_worker = Some(rx);
                                        std::thread::spawn(move || {
                                            let _ = tx.send(prefetch::prefetch_aion2(&root, windows::aion2_running));
                                        });
                                    }
                                    Err(error) => self.error = error,
                                }
                            }
                        });
                        if !self.aion2_status.is_empty() {
                            ui.label(egui::RichText::new(&self.aion2_status).size(11.0).color(COLOR_ACCENT));
                        }
                    });
                    ui.add_space(10.0);


                    // JOGOS RODANDO (LISTA NOTMETER STYLE)
                    notmeter_card(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 8.0).max(640.0));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("JOGOS DETECTADOS EM EXECUÇÃO ({})", self.running.len())).size(14.0).strong().color(COLOR_TEXT));
                        });
                        ui.add_space(8.0);

                        if self.running.is_empty() {
                            ui.label(egui::RichText::new("Nenhum processo dentro das pastas dos jogos Steam instalado em execução.").size(12.0).color(COLOR_MUTED));
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
                                        if boosted || is_pinned || has_qos {
                                            notmeter_badge(ui, "OTIMIZANDO AGORA", COLOR_ACCENT);
                                        } else if enabled {
                                            notmeter_badge(ui, "STANDBY", COLOR_YELLOW);
                                        } else {
                                            notmeter_badge(ui, "INATIVO", COLOR_MUTED);
                                        }

                                        ui.label(egui::RichText::new(&process.game).size(15.0).strong().color(COLOR_TEXT));
                                        notmeter_badge(ui, &format!("PID {}", process.pid), COLOR_CYAN);

                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if self.auto_games.contains(&process.directory) {
                                                notmeter_badge(ui, "AUTO ATIVO", COLOR_ACCENT);
                                            } else if ui.add_enabled(
                                                self.tray.is_optimizer_running(),
                                                egui::Button::new(if enabled { "Restaurar" } else { "Otimizar" }),
                                            ).clicked() {
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
                            ui.label(egui::RichText::new(format!("BIBLIOTECA STEAM ({})", self.games.len())).size(14.0).strong().color(COLOR_TEXT));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.search)
                                        .hint_text("Buscar jogo...")
                                        .desired_width(220.0),
                                );
                            });
                        });
                        ui.add_space(8.0);

                        let search = self.search.to_lowercase();
                        let mut needs_refresh = false;

                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for game in &self.games {
                                if game.name.to_lowercase().contains(&search) {
                                    egui::Frame::new()
                                        .fill(COLOR_SURFACE_ELEVATED)
                                        .stroke(egui::Stroke::new(1.0f32, COLOR_LINE_SOFT))
                                        .corner_radius(egui::CornerRadius::same(6))
                                        .inner_margin(egui::Margin::symmetric(12, 8))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(egui::RichText::new(&game.name).size(14.0).strong().color(COLOR_TEXT));

                                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                    let mut qos = self.qos_games.contains(&game.directory);
                                                    if ui.checkbox(&mut qos, egui::RichText::new("Prioridade de memória / EcoQoS").color(COLOR_TEXT)).changed() {
                                                        if qos {
                                                            self.qos_games.insert(game.directory.clone());
                                                        } else {
                                                            self.qos_games.remove(&game.directory);
                                                        }
                                                        needs_refresh = true;
                                                    }

                                                    // Toggle P-cores
                                                    let mut pin = self.pin_games.contains(&game.directory);
                                                    if ui.checkbox(&mut pin, egui::RichText::new("P-cores").color(COLOR_TEXT)).changed() {
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
                                                                if let Err(error) = pinner.update(&self.running, &self.pin_games, self.windowed_mode) {
                                                                    self.error = error;
                                                                }
                                                            }
                                                        }
                                                    }

                                                    // Toggle Auto Prioridade
                                                    let mut auto = self.auto_games.contains(&game.directory);
                                                    if ui.checkbox(&mut auto, egui::RichText::new("Auto Prioridade").color(COLOR_TEXT)).changed() {
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
                                            });
                                        });
                                    ui.add_space(4.0);
                                }
                            }
                        });

                        if needs_refresh {
                            self.refresh();
                        }
                    });
                });
            });
        }
    }
}

#[cfg(windows)]
fn main() -> eframe::Result {
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
