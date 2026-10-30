//! Read-only UI snapshot. Built from plain inputs so the egui render and, later,
//! the webview consume the same data. Commands go the other way (not here).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::intel_dtt::IntelDttStatus;
use crate::steam::Game;
use crate::ui_model::{header_model, HeaderModel};
use crate::learning::{Score, Settings};
use crate::view::candidate_summary;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameRow {
    pub name: String,
    pub directory: String,
    pub is_aion2: bool,
    pub auto_priority: bool,
    pub pcores: bool,
    pub qos: bool,
    pub rapo: bool,
    /// Locks manual settings during training, but not RAPO game selection.
    pub controls_enabled: bool,
}

/// Flags come from the app's sets; `controls_enabled` is false while training runs.
pub fn game_rows(
    games: &[Game],
    auto: &HashSet<PathBuf>,
    pins: &HashSet<PathBuf>,
    qos: &HashSet<PathBuf>,
    rapo: Option<&Path>,
    controls_enabled: bool,
) -> Vec<GameRow> {
    games
        .iter()
        .map(|game| GameRow {
            name: game.name.clone(),
            directory: game.directory.display().to_string(),
            is_aion2: game.is_aion2,
            auto_priority: auto.contains(&game.directory),
            pcores: pins.contains(&game.directory),
            qos: qos.contains(&game.directory),
            rapo: rapo == Some(game.directory.as_path()),
            controls_enabled,
        })
        .collect()
}

/// Plain copies of what the training panel shows, taken from `App` each frame.
#[derive(Debug, Clone, Default)]
pub struct TrainingState {
    /// Set while a session runs; the panel then shows its progress and stop control.
    pub session: Option<ActiveSession>,
    /// RAPO game armed while idle, waiting for it to take focus.
    pub armed_for: Option<PathBuf>,
    pub targets: Vec<Target>,
    pub selected_pid: Option<u32>,
    pub can_start: bool,
    pub message: String,
    pub cpu_line: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub paused: bool,
    pub exe: PathBuf,
    pub pid: u32,
    pub elapsed_secs: u64,
    pub completed: usize,
    pub explored: usize,
    /// Setting under measurement: the confirmed winner, or the search cursor.
    pub candidate: Settings,
    pub best: Option<Score>,
}

/// A running process the training target can point at.
#[derive(Debug, Clone)]
pub struct Target {
    pub pid: u32,
    pub game: String,
    pub exe: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TargetChoice {
    pub pid: u32,
    pub label: String,
    pub hint: String,
    pub selected: bool,
}

/// Pre-formatted training panel content. `None` lines are not shown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrainingView {
    pub cpu_line: Option<String>,
    pub active: bool,
    pub paused: bool,
    pub candidate: Option<Settings>,
    pub headline: Option<String>,
    pub target_line: Option<String>,
    pub progress_line: Option<String>,
    pub candidate_line: Option<String>,
    pub best_line: Option<String>,
    pub armed_line: Option<String>,
    pub targets: Vec<TargetChoice>,
    pub selected_label: Option<String>,
    pub can_start: bool,
    pub message: String,
}

pub fn training_view(state: &TrainingState) -> TrainingView {
    let session = state.session.as_ref();
    TrainingView {
        cpu_line: state.cpu_line.clone(),
        active: session.is_some(),
        paused: session.is_some_and(|s| s.paused),
        candidate: session.map(|s| s.candidate),
        headline: session.map(|s| {
            String::from(if s.paused {
                "RAPO PAUSADO · sem otimização do alvo; aguardando o mesmo jogo em foco"
            } else if s.candidate.bits() == 0 {
                "RAPO ATIVO · medindo referência sem ajustes"
            } else {
                "RAPO ATIVO · medindo o candidato atual"
            })
        }),
        target_line: session.map(|s| format!("{} · PID {}", s.exe.display(), s.pid)),
        progress_line: session.map(|s| {
            format!(
                "{} · {} medições · {} configurações exploradas",
                elapsed_label(s.elapsed_secs),
                s.completed,
                s.explored
            )
        }),
        candidate_line: session.map(|s| candidate_summary(s.candidate)),
        best_line: session
            .and_then(|s| s.best)
            .map(|best| format!("Melhor confirmado: {:.1} FPS · 1% low {:.1} FPS", best.fps, best.low_fps)),
        armed_line: match (session, &state.armed_for) {
            (None, Some(directory)) => {
                Some(format!("RAPO armado para {}; aguardando o jogo em foco.", directory.display()))
            }
            _ => None,
        },
        targets: state
            .targets
            .iter()
            .map(|target| TargetChoice {
                pid: target.pid,
                label: target_label(target),
                hint: target.exe.display().to_string(),
                selected: Some(target.pid) == state.selected_pid,
            })
            .collect(),
        selected_label: state
            .selected_pid
            .and_then(|pid| state.targets.iter().find(|target| target.pid == pid))
            .map(target_label),
        can_start: state.can_start,
        message: state.message.clone(),
    }
}

fn target_label(target: &Target) -> String {
    format!(
        "{} · {} · PID {}",
        target.game,
        target.exe.file_name().unwrap_or_default().to_string_lossy(),
        target.pid
    )
}

fn elapsed_label(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub header: HeaderModel,
    pub games: Vec<GameRow>,
    pub training: TrainingView,
}

impl Snapshot {
    pub fn new(
        optimizer_running: bool,
        dtt: &IntelDttStatus,
        running_games: usize,
        games: Vec<GameRow>,
        training: TrainingView,
    ) -> Self {
        Self {
            header: header_model(optimizer_running, dtt, running_games),
            games,
            training,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(name: &str, dir: &str, is_aion2: bool) -> Game {
        Game { name: name.to_owned(), directory: PathBuf::from(dir), is_aion2 }
    }

    #[test]
    fn header_serializes_with_snake_case_badge() {
        let snapshot = Snapshot::new(false, &IntelDttStatus::InstalledStopped, 0, Vec::new(), training_view(&TrainingState::default()));
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["header"]["dtt"], "stopped");
        assert_eq!(json["header"]["optimizer_running"], false);
    }

    #[test]
    fn default_config_disabled_while_games_run() {
        let idle = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 0, Vec::new(), training_view(&TrainingState::default()));
        let busy = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 2, Vec::new(), training_view(&TrainingState::default()));
        assert!(idle.header.default_config_enabled);
        assert!(!busy.header.default_config_enabled);
    }

    #[test]
    fn game_rows_reflect_flags_and_single_rapo_choice() {
        let games = [game("AION 2", "/g/aion", true), game("Other", "/g/other", false)];
        let auto: HashSet<PathBuf> = [PathBuf::from("/g/aion")].into_iter().collect();
        let rows = game_rows(&games, &auto, &HashSet::new(), &HashSet::new(), Some(Path::new("/g/other")), false);
        assert!(rows[0].auto_priority && !rows[0].rapo);
        assert!(!rows[1].auto_priority && rows[1].rapo);
        assert!(rows.iter().all(|row| !row.controls_enabled));
    }

    fn session(paused: bool, elapsed_secs: u64, best: Option<Score>) -> ActiveSession {
        ActiveSession {
            paused,
            exe: PathBuf::from("/g/aion.exe"),
            pid: 7,
            elapsed_secs,
            completed: 3,
            explored: 5,
            candidate: Settings::default(),
            best,
        }
    }

    #[test]
    fn active_session_shows_progress_and_hides_picker_and_armed_line() {
        let state = TrainingState {
            session: Some(session(false, 125, None)),
            armed_for: Some(PathBuf::from("/g/aion")),
            targets: vec![Target { pid: 7, game: "AION 2".into(), exe: "/g/aion.exe".into() }],
            can_start: true,
            ..Default::default()
        };
        let view = training_view(&state);
        assert!(view.active);
        assert_eq!(view.progress_line.as_deref(), Some("02:05 · 3 medições · 5 configurações exploradas"));
        assert_eq!(view.armed_line, None);
        assert_eq!(view.best_line, None);
    }

    #[test]
    fn paused_session_and_confirmed_best_are_formatted() {
        let best = Score { fps: 60.04, low_fps: 48.26 };
        let view = training_view(&TrainingState { session: Some(session(true, 0, Some(best))), ..Default::default() });
        assert_eq!(view.headline.as_deref(), Some("RAPO PAUSADO · sem otimização do alvo; aguardando o mesmo jogo em foco"));
        assert_eq!(view.best_line.as_deref(), Some("Melhor confirmado: 60.0 FPS · 1% low 48.3 FPS"));
    }

    #[test]
    fn idle_lists_targets_with_selection_and_armed_game() {
        let state = TrainingState {
            armed_for: Some(PathBuf::from("/g/aion")),
            targets: vec![
                Target { pid: 7, game: "AION 2".into(), exe: "/g/aion.exe".into() },
                Target { pid: 9, game: "Other".into(), exe: "/g/other.exe".into() },
            ],
            selected_pid: Some(9),
            can_start: true,
            ..Default::default()
        };
        let view = training_view(&state);
        assert!(!view.active && view.headline.is_none() && view.progress_line.is_none());
        assert_eq!(view.armed_line.as_deref(), Some("RAPO armado para /g/aion; aguardando o jogo em foco."));
        assert_eq!(view.selected_label.as_deref(), Some("Other · other.exe · PID 9"));
        assert_eq!(view.targets.iter().map(|t| t.selected).collect::<Vec<_>>(), vec![false, true]);
        assert!(view.can_start);
    }
}
