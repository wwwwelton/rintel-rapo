use crate::{
    capture::Capture,
    learning::{Limits, Profile, Score, Search, Settings},
};

#[path = "power.rs"]
mod power;

struct Training {
    pid: u32,
    exe: PathBuf,
    had_override: bool,
    original: Settings,
    original_manual: Vec<u32>,
    search: Search,
    capture: Option<Capture>,
    started: Instant,
    capture_started: Instant,
    winner: Option<(Settings, Score)>,
    saved: Option<Settings>,
    paused: bool,
    power: power::Power,
}
fn foreground_pid() -> u32 {
    unsafe {
        let window = windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        let mut pid = 0;
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(window, &mut pid);
        pid
    }
}

impl App {
    fn select_rapo(&mut self, directory: Option<PathBuf>) {
        if let Some(training) = self.training.take() {
            self.restore_training(
                training,
                "RAPO parado; último perfil salvo preservado.".into(),
            );
        }
        self.rapo_game = directory;
    }

    fn sync_training(&mut self) {
        let foreground = foreground_pid();
        if self.training.is_none() {
            if self.tray.is_optimizer_running() {
                if let Some(index) = self.running.iter().position(|p| {
                    p.pid == foreground && self.rapo_game.as_ref() == Some(&p.directory)
                }) {
                    self.training_target = index;
                    self.start_training();
                }
            }
            return;
        }
        let mut training = self.training.take().unwrap();
        let target = self
            .running
            .iter()
            .find(|p| p.pid == foreground && windows::same_executable(&p.exe, &training.exe));
        if !self.tray.is_optimizer_running() || target.is_none() {
            training.capture = None;
            if !training.paused {
                if let Err(error) = self.set_settings(&training.exe, Settings::default()) {
                    self.restore_training(training, error);
                    return;
                }
                training.paused = true;
                self.training_message = "RAPO pausado: aguardando o jogo em foco; candidato e medições confirmadas preservados.".into();
            }
            if let Err(error) = training.power.update(false) {
                self.training_message.push_str(&format!("; {error}"));
            }
        } else if let Some(target) = target {
            let pid = target.pid;
            if training.paused || training.pid != pid {
                training.pid = pid;
                training.capture = None;
                let settings = training
                    .winner
                    .map_or_else(|| training.search.current(), |winner| winner.0);
                if let Err(error) = self.set_settings(&training.exe, settings) {
                    self.restore_training(training, error);
                    return;
                }
                training.paused = false;
                training.capture_started = Instant::now();
                self.training_message =
                    "RAPO retomado com o mesmo candidato; medição reiniciada após estabilização."
                        .into();
                if let Err(error) = training.power.update(true) {
                    self.training_message.push_str(&format!("; {error}"));
                }
            }
        }
        self.training = Some(training);
    }

    fn clear_runtime_profiles(&mut self, directory: &std::path::Path) {
        let removed: Vec<_> = self
            .running
            .iter()
            .filter(|p| windows::same_executable(&p.directory, directory) && self.runtime_profile(&p.exe).is_some())
            .map(|p| p.exe.clone())
            .collect();
        self.runtime_profiles
            .retain(|exe, _| !windows::path_in_directory(exe, directory));
        for process in self.running.iter().filter(|p| removed.iter().any(|exe| windows::same_executable(exe, &p.exe))) {
            if self.optimizer.enabled(process.pid) != self.auto_games.iter().any(|dir| windows::same_executable(dir, directory)) {
                if let Err(error) = self.optimizer.toggle(process) {
                    self.error = error;
                }
            }
        }
    }
    fn effective_settings(&self, process: &Running) -> Settings {
        self.runtime_profile(&process.exe)
            .unwrap_or(Settings {
                priority: self.auto_games.iter().any(|dir| windows::same_executable(dir, &process.directory)),
                pin: self.pin_games.iter().any(|dir| windows::same_executable(dir, &process.directory)),
                qos: self.qos_games.iter().any(|dir| windows::same_executable(dir, &process.directory)),
                ..Settings::default()
            })
    }
    fn set_settings(&mut self, exe: &std::path::Path, settings: Settings) -> Result<(), String> {
        if settings.pin && self.pinner.is_none() {
            self.pinner = Some(Pinner::new()?);
        }
        self.runtime_profiles.retain(|path, _| !windows::same_executable(path, exe));
        self.runtime_profiles.insert(exe.to_path_buf(), settings);
        for process in self.running.iter().filter(|p| windows::same_executable(&p.exe, exe)) {
            if self.optimizer.enabled(process.pid) != settings.priority {
                self.optimizer.toggle(process)?;
            }
        }
        Ok(())
    }
    fn apply_learned_profiles(&mut self) {
        self.learned_pids
            .retain(|(pid, exe)| self.running.iter().any(|p| p.pid == *pid && windows::same_executable(&p.exe, exe)));
        let pending: Vec<_> = self
            .running
            .iter()
            .filter(|p| !self.learned_pids.iter().any(|(pid, exe)| *pid == p.pid && windows::same_executable(exe, &p.exe)))
            .filter_map(|p| {
                self.learned_profiles
                    .iter()
                    .find(|profile| windows::same_executable(&profile.exe, &p.exe))
                    .map(|profile| (p.pid, p.exe.clone(), profile.settings))
            })
            .collect();
        for (pid, exe, settings) in pending {
            if self.training.is_some() {
                continue;
            }
            match self.set_settings(&exe, settings) {
                Ok(()) => {
                    self.learned_pids.insert((pid, exe));
                }
                Err(error) => {
                    self.training_message = format!("Não foi possível aplicar perfil: {error}")
                }
            }
        }
    }
    fn restore_training(&mut self, training: Training, message: String) {
        self.rapo_game = None;
        let exe = training.exe.clone();
        let had_override = training.saved.is_some() || training.had_override;
        let original = training.saved.unwrap_or(training.original);
        let manual = if training.saved.is_some() {
            Vec::new()
        } else {
            training.original_manual.clone()
        };
        drop(training);
        self.training_message = message;
        if let Err(error) = self.set_settings(&exe, original) {
            self.training_message
                .push_str(&format!("; restauração: {error}"));
        }
        if !had_override {
            self.runtime_profiles.retain(|path, _| !windows::same_executable(path, &exe));
        }
        for process in self
            .running
            .iter()
            .filter(|p| windows::same_executable(&p.exe, &exe) && manual.contains(&p.pid))
        {
            if !self.optimizer.enabled(process.pid) {
                if let Err(error) = self.optimizer.toggle(process) {
                    self.training_message.push_str(&format!("; {error}"));
                }
            }
        }
    }
    fn selected_process(&self) -> Option<&Running> {
        if let Some((pid, exe)) = &self.training_selected {
            if let Some(process) = self.running.iter().find(|p| p.pid == *pid && windows::same_executable(&p.exe, exe)) {
                return Some(process);
            }
        }
        self.running.first()
    }
    fn start_training(&mut self) {
        let Some(process) = self.selected_process().cloned() else {
            return;
        };
        self.training_selected = Some((process.pid, process.exe.clone()));
        self.rapo_game = Some(process.directory.clone());
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |t| t.as_nanos() as u64);
        let training = Training {
            pid: process.pid,
            exe: process.exe.clone(),
            had_override: self.runtime_profile(&process.exe).is_some(),
            original: self.effective_settings(&process),
            original_manual: self
                .running
                .iter()
                .filter(|p| windows::same_executable(&p.exe, &process.exe) && self.optimizer.enabled(p.pid))
                .map(|p| p.pid)
                .collect(),
            search: Search::new(
                seed,
                self.pinner
                    .as_ref()
                    .map_or_else(Limits::default, Pinner::limits),
                self.effective_settings(&process),
            ),
            capture: None,
            started: Instant::now(),
            capture_started: Instant::now(),
            winner: None,
            saved: None,
            paused: true,
            power: power::Power::default(),
        };
        if let Err(error) = self.set_settings(&training.exe, Settings::default()) {
            self.restore_training(training, error);
            return;
        }
        self.training = Some(training);
        self.training_message = "RAPO preparado; aguardando o jogo em foco para medir. Perder foco ou fechar pausa sem perder o candidato.".into();
    }
    fn queue_winner(&mut self, mut training: Training, winner: (Settings, Score)) {
        training.capture = None;
        if let Err(error) = self.set_settings(&training.exe, winner.0) {
            self.restore_training(training, error);
        } else {
            training.winner = Some(winner);
            training.capture_started = Instant::now();
            self.training = Some(training);
        }
    }
    fn tick_training(&mut self) {
        let Some(mut training) = self.training.take() else {
            return;
        };
        if training.paused {
            self.training = Some(training);
            return;
        }
        let settings = training
            .winner
            .map_or_else(|| training.search.current(), |winner| winner.0);
        let alive = self
            .running
            .iter()
            .any(|p| p.pid == training.pid && windows::same_executable(&p.exe, &training.exe));
        let conflict = settings.pin
            && self.pinner.as_ref().is_some_and(|p| p.conflict().is_some_and(|c| c.pid == training.pid));
        if !alive || !self.tray.is_optimizer_running() || conflict {
            let message = if conflict {
                "Aprendizado interrompido por conflito de afinidade com outro agendador; último perfil salvo preservado.".into()
            } else if !self.tray.is_optimizer_running() {
                "Aprendizado pausado pelo Stop da bandeja; último perfil salvo preservado.".into()
            } else {
                "Processo do jogo encerrou; último perfil salvo preservado.".into()
            };
            self.restore_training(training, message);
            return;
        }
        if let Some((settings, score)) = training.winner {
            if settings.pin
                && self
                    .pinner
                    .as_ref()
                    .is_none_or(|p| p.pinned_count(training.pid) == 0)
            {
                if training.capture_started.elapsed() < Duration::from_secs(5) {
                    self.training = Some(training);
                } else {
                    self.restore_training(
                        training,
                        "Melhor perfil não pôde ser aplicado; perfil salvo anterior preservado."
                            .into(),
                    );
                }
                return;
            }
            if self
                .learned_profiles
                .iter()
                .find(|p| windows::same_executable(&p.exe, &training.exe))
                .is_none_or(|p| score.improves(p.score))
            {
                let profile = Profile {
                    exe: training.exe.clone(),
                    settings,
                    score,
                };
                if let Err(error) = crate::learning::save_profile(&profile) {
                    self.restore_training(training, format!("Perfil não salvo: {error}"));
                    return;
                }
                self.learned_profiles.retain(|p| !windows::same_executable(&p.exe, &profile.exe));
                self.learned_profiles.push(profile);
                training.saved = Some(settings);
                self.training_message = format!("Melhor perfil salvo após {} medições; aprendizado continua. Prioridade {}, afinidade {}, {} núcleos, até {} threads, SMT {}, P-only {}, limiar {}%, memória {}. Média {:.1} FPS; 1% low {:.1} FPS.", training.search.completed(), settings.priority, settings.pin, settings.cores, settings.threads, settings.smt, settings.p_only, settings.min_percent, settings.qos, score.fps, score.low_fps);
            }
            training.winner = None;
            if let Err(error) = self.set_settings(&training.exe, training.search.current()) {
                self.restore_training(training, error);
            } else {
                training.capture_started = Instant::now();
                self.training = Some(training);
            }
            return;
        }
        if training.capture_started.elapsed() > Duration::from_secs(5) {
            if settings.priority && !self.optimizer.boosted(training.pid) {
                self.restore_training(
                    training,
                    "Prioridade não foi aplicada; perfil anterior preservado.".into(),
                );
                return;
            }
            if settings.pin
                && self
                    .pinner
                    .as_ref()
                    .is_none_or(|p| p.pinned_count(training.pid) == 0)
            {
                training.capture = None;
                training.search.reject_current();
                self.training_message = "Candidato sem threads elegíveis descartado; aprendizado continua com outros ajustes.".into();
                if let Err(error) = self.set_settings(&training.exe, training.search.current()) {
                    self.restore_training(training, error);
                } else {
                    training.capture_started = Instant::now();
                    self.training = Some(training);
                }
                return;
            }
        }
        if training.capture.is_none() {
            match Capture::start(training.pid) {
                Ok(capture) => {
                    training.capture = Some(capture);
                    training.capture_started = Instant::now();
                }
                Err(error) => {
                    self.restore_training(training, error);
                    return;
                }
            }
        }
        match training.capture.as_mut().unwrap().poll() {
            Ok(None) => self.training = Some(training),
            Err(error) => self.restore_training(training, error),
            Ok(Some(score)) => {
                training.capture = None;
                match training.search.record(score) {
                    Err(error) => self.restore_training(training, error),
                    Ok(None) => {
                        if let Err(error) =
                            self.set_settings(&training.exe, training.search.current())
                        {
                            self.restore_training(training, error);
                        } else {
                            training.capture_started = Instant::now();
                            match Capture::start(training.pid) {
                                Ok(capture) => {
                                    training.capture = Some(capture);
                                    self.training = Some(training);
                                }
                                Err(error) => self.restore_training(training, error),
                            }
                        }
                    }
                    Ok(Some(winner)) => self.queue_winner(training, winner),
                }
            }
        }
    }
    /// Copies the training state the panel shows; the panel itself never mutates `App`.
    fn training_state(&self) -> crate::snapshot::TrainingState {
        crate::snapshot::TrainingState {
            session: self.training.as_ref().map(|training| {
                let candidate = training.winner.map_or_else(|| training.search.current(), |winner| winner.0);
                crate::snapshot::ActiveSession {
                    paused: training.paused,
                    exe: training.exe.clone(),
                    pid: training.pid,
                    elapsed_secs: training.started.elapsed().as_secs(),
                    completed: training.search.completed(),
                    explored: training.search.explored(),
                    candidate,
                    best: training.search.best().map(|(_, score)| score),
                }
            }),
            armed_for: self.rapo_game.clone(),
            targets: self
                .running
                .iter()
                .map(|p| crate::snapshot::Target { pid: p.pid, game: p.game.clone(), exe: p.exe.clone() })
                .collect(),
            selected_pid: self.selected_process().map(|p| p.pid),
            can_start: !self.running.is_empty() && self.tray.is_optimizer_running(),
            message: self.training_message.clone(),
            cpu_line: self.pinner.as_ref().map(|pinner| {
                let limits = pinner.limits();
                format!("CPU detectada: {} núcleos físicos · {} processadores lógicos · {} núcleos de maior desempenho · SMT disponível {}", limits.cores, limits.threads, limits.p_cores, limits.smt)
            }),
        }
    }
    fn training_panel(
        &self,
        ui: &mut egui::Ui,
        view: &crate::snapshot::TrainingView,
        commands: &mut Vec<crate::command::Command>,
    ) {
        use crate::command::Command;
        notmeter_card(ui, |ui| {
            ui.set_width((ui.ctx().screen_rect().width() - 72.0).max(320.0));
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.label(
                egui::RichText::new(self.tr("txt_learning_panel_title", "Avatarnoia Flow Engine 2.0 · FPS REAL"))
                    .strong()
                    .color(COLOR_TEXT),
            );
            if let Some(cpu) = &view.cpu_line {
                ui.label(cpu);
            }
            if view.active {
                for line in [&view.headline, &view.target_line, &view.progress_line, &view.candidate_line, &view.best_line].into_iter().flatten() {
                    ui.label(line);
                }
                ui.add_sized([ui.available_width().min(720.0), 0.0], egui::Label::new("Testa cores, afinidade, threads críticas e uso dos irmãos SMT/Hyper-Threading conforme a topologia real do grupo 0. Sair do jogo pausa e descarta a janela parcial; voltar reinicia a medição do mesmo candidato. Não altera BIOS nem desativa SMT globalmente.").wrap());
                if ui.button(self.tr("btn_stop_training", "Parar aprendizado e manter último perfil salvo")).clicked() {
                    commands.push(Command::StopTraining);
                }
            } else {
                if let Some(armed) = &view.armed_line {
                    ui.label(armed);
                    if ui.button(self.tr("btn_disable_rapo", "Desativar RAPO")).clicked() {
                        commands.push(Command::SetRapo(None));
                    }
                }
                let selected_text = view
                    .selected_label
                    .clone()
                    .unwrap_or_else(|| self.tr("txt_combobox_idle", "Abra um jogo Steam").into());
                egui::ComboBox::from_id_salt("learning_target")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for target in &view.targets {
                            if ui
                                .selectable_label(target.selected, target.label.as_str())
                                .on_hover_text(target.hint.as_str())
                                .clicked()
                            {
                                commands.push(Command::SelectTrainingTarget { pid: target.pid });
                            }
                        }
                    });
                ui.add_sized([ui.available_width().min(720.0), 0.0], egui::Label::new("RAPO aprende prioridade, memória/EcoQoS, quantidade de núcleos físicos, afinidade P/all, threads críticas e irmãos SMT/Hyper-Threading disponíveis. Um jogo por vez; pausa ao sair e retoma o mesmo candidato.").wrap());
                ui.add_sized([ui.available_width().min(720.0), 0.0], egui::Label::new("ETW de apresentação, sem rastrear entrada ou execução na GPU, sem injeção, driver ou leitura da memória do jogo. Acesso negado interrompe a captura, sem contornar proteções.").wrap());
                if ui
                    .add_enabled(view.can_start, egui::Button::new(self.tr("btn_start_training", "Iniciar aprendizado contínuo")))
                    .clicked()
                {
                    commands.push(Command::StartTraining);
                }
            }
            ui.add_sized([ui.available_width().min(720.0), 0.0], egui::Label::new("O resultado é o melhor perfil medido neste cenário, não garantia de ganho ou de compatibilidade com anti-cheat. Use uma cena repetível.").wrap());
            ui.collapsing("Licença do coletor integrado", |ui| {
                ui.label("PresentData / PresentMon 2.3.0 · Intel · MIT");
                ui.add_sized(
                    [ui.available_width().min(720.0), 0.0],
                    egui::Label::new(include_str!("../vendor/presentmon/LICENSE.txt")).wrap(),
                );
            });
            if !view.message.is_empty() {
                ui.add_sized([ui.available_width().min(720.0), 0.0], egui::Label::new(&view.message).wrap());
            }
        });
    }
}
