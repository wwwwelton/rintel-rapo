//! Read-only UI snapshot. Built from plain inputs so the egui render and, later,
//! the webview consume the same data. Commands go the other way (not here).

use serde::Serialize;

use crate::intel_dtt::IntelDttStatus;
use crate::ui_model::{header_model, HeaderModel};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub header: HeaderModel,
}

impl Snapshot {
    pub fn new(optimizer_running: bool, dtt: &IntelDttStatus, running_games: usize) -> Self {
        Self {
            header: header_model(optimizer_running, dtt, running_games),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_serializes_with_snake_case_badge() {
        let snapshot = Snapshot::new(false, &IntelDttStatus::InstalledStopped, 0);
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["header"]["dtt"], "stopped");
        assert_eq!(json["header"]["optimizer_running"], false);
    }

    #[test]
    fn default_config_disabled_while_games_run() {
        let idle = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 0);
        let busy = Snapshot::new(false, &IntelDttStatus::ActiveRunning, 2);
        assert!(idle.header.default_config_enabled);
        assert!(!busy.header.default_config_enabled);
    }
}
