//! Read-only UI snapshot. Built from plain inputs so the egui render and, later,
//! the webview consume the same data. Commands go the other way (not here).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::intel_dtt::IntelDttStatus;
use crate::steam::Game;
use crate::ui_model::{header_model, HeaderModel};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameRow {
    pub name: String,
    pub directory: String,
    pub is_aion2: bool,
    pub auto_priority: bool,
    pub pcores: bool,
    pub qos: bool,
    pub rapo: bool,
    /// False while training runs; the render disables every game control then.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub header: HeaderModel,
    pub games: Vec<GameRow>,
}

impl Snapshot {
    pub fn new(optimizer_running: bool, dtt: &IntelDttStatus, running_games: usize, games: Vec<GameRow>) -> Self {
        Self {
            header: header_model(optimizer_running, dtt, running_games),
            games,
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
        let snapshot = Snapshot::new(false, &IntelDttStatus::InstalledStopped, 0, Vec::new());
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["header"]["dtt"], "stopped");
        assert_eq!(json["header"]["optimizer_running"], false);
    }

    #[test]
    fn default_config_disabled_while_games_run() {
        let idle = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 0, Vec::new());
        let busy = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 2, Vec::new());
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
}
