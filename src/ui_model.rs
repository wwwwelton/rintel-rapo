//! UI state snapshots. Pure logic, no platform calls, so it runs under `cargo test` on any OS.

use crate::intel_dtt::IntelDttStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DttBadge {
    Running,
    Stopped,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderModel {
    pub optimizer_running: bool,
    pub dtt: DttBadge,
    /// "Configuração padrão" is disabled while any tuned game is running.
    pub default_config_enabled: bool,
}

pub fn dtt_badge(status: &IntelDttStatus) -> DttBadge {
    match status {
        IntelDttStatus::ActiveRunning => DttBadge::Running,
        IntelDttStatus::InstalledStopped => DttBadge::Stopped,
        IntelDttStatus::NotInstalled => DttBadge::NotInstalled,
    }
}

pub fn header_model(optimizer_running: bool, dtt: &IntelDttStatus, running_games: usize) -> HeaderModel {
    HeaderModel {
        optimizer_running,
        dtt: dtt_badge(dtt),
        default_config_enabled: running_games == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtt_status_maps_to_badge() {
        assert_eq!(dtt_badge(&IntelDttStatus::ActiveRunning), DttBadge::Running);
        assert_eq!(dtt_badge(&IntelDttStatus::InstalledStopped), DttBadge::Stopped);
        assert_eq!(dtt_badge(&IntelDttStatus::NotInstalled), DttBadge::NotInstalled);
    }

    #[test]
    fn default_config_requires_no_running_games() {
        assert!(header_model(true, &IntelDttStatus::NotInstalled, 0).default_config_enabled);
        assert!(!header_model(true, &IntelDttStatus::NotInstalled, 1).default_config_enabled);
    }

    #[test]
    fn optimizer_flag_passes_through() {
        assert!(header_model(true, &IntelDttStatus::NotInstalled, 0).optimizer_running);
        assert!(!header_model(false, &IntelDttStatus::NotInstalled, 0).optimizer_running);
    }
}
