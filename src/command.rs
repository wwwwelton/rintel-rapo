//! UI intents. The render only records them; `App::apply` runs the side effects
//! after the frame, so the render itself stays read-only.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Desired optimizer state, computed from the state the frame was drawn with.
    SetOptimizerRunning(bool),
    Refresh,
    /// Requests UAC elevation and closes this instance on success.
    RelaunchElevated,
    /// Library filter text, typed into the search box.
    SetSearch(String),
    ScanSteam,
    ResetNetQos,
    SetAutoYield(bool),
    SetMinimizeToTray(bool),
    SetQos { directory: PathBuf, on: bool },
    SetPin { directory: PathBuf, on: bool },
    SetAuto { directory: PathBuf, on: bool },
    SetWindowed(bool),
    /// Optimize or restore one running process, looked up by pid when applied.
    ToggleProcess { pid: u32 },
    /// `None` clears the RAPO game.
    SetRapo(Option<PathBuf>),
    ApplyAion2Profile { directory: PathBuf },
    /// Starts continuous learning on the selected process.
    StartTraining,
    /// Stops learning and keeps the last saved profile.
    StopTraining,
    /// Chooses the process the next session targets, looked up by pid when applied.
    SelectTrainingTarget { pid: u32 },
}

/// Maps a webview event (`data-action`, `data-dir`, checkbox state) to a command.
/// Unknown actions or missing fields yield `None`, so the event is ignored.
pub fn from_ui_event(action: &str, dir: Option<&str>, checked: Option<bool>) -> Option<Command> {
    let directory = || dir.map(PathBuf::from);
    match action {
        "start" => Some(Command::SetOptimizerRunning(true)),
        "stop" => Some(Command::SetOptimizerRunning(false)),
        "refresh" => Some(Command::Refresh),
        "scan" => Some(Command::ScanSteam),
        "reset" => Some(Command::ResetNetQos),
        "elevate" => Some(Command::RelaunchElevated),
        "auto-yield" => Some(Command::SetAutoYield(checked?)),
        "windowed" => Some(Command::SetWindowed(checked?)),
        "minimize-tray" => Some(Command::SetMinimizeToTray(checked?)),
        "qos" => Some(Command::SetQos { directory: directory()?, on: checked? }),
        "pcores" => Some(Command::SetPin { directory: directory()?, on: checked? }),
        "auto" => Some(Command::SetAuto { directory: directory()?, on: checked? }),
        "rapo" => Some(Command::SetRapo(if checked? { directory() } else { None })),
        "apo-profile" => Some(Command::ApplyAion2Profile { directory: directory()? }),
        _ => None,
    }
}


/// Decodes the JSON IPC boundary; invalid fields never become commands.
pub fn from_json(body: &str) -> Option<Command> {
    let event: serde_json::Value = serde_json::from_str(body).ok()?;
    let action = event.get("action")?.as_str()?;
    if action == "search" {
        return Some(Command::SetSearch(event.get("text")?.as_str()?.to_owned()));
    }
    let dir = event.get("dir").and_then(|v| v.as_str());
    let checked = event.get("checked").and_then(|v| v.as_bool());
    let pid = event.get("pid").and_then(|v| v.as_u64()).and_then(|p| u32::try_from(p).ok());
    from_ui_event(action, dir, checked).or_else(|| from_training_event(action, pid))
}
/// Maps training panel events (buttons and target picks by `pid`) to commands.
pub fn from_training_event(action: &str, pid: Option<u32>) -> Option<Command> {
    match action {
        "start_training" => Some(Command::StartTraining),
        "stop_training" => Some(Command::StopTraining),
        "disable_rapo" => Some(Command::SetRapo(None)),
        "toggle-process" => Some(Command::ToggleProcess { pid: pid? }),
        "select_training_target" => Some(Command::SelectTrainingTarget { pid: pid? }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_checkbox_events_with_directory_and_state() {
        assert_eq!(
            from_ui_event("qos", Some("C:/g"), Some(true)),
            Some(Command::SetQos { directory: "C:/g".into(), on: true })
        );
        assert_eq!(
            from_ui_event("pcores", Some("C:/g"), Some(false)),
            Some(Command::SetPin { directory: "C:/g".into(), on: false })
        );
    }

    #[test]
    fn rapo_checkbox_selects_or_clears_the_game() {
        assert_eq!(from_ui_event("rapo", Some("C:/g"), Some(true)), Some(Command::SetRapo(Some("C:/g".into()))));
        assert_eq!(from_ui_event("rapo", Some("C:/g"), Some(false)), Some(Command::SetRapo(None)));
    }

    #[test]
    fn ignores_unknown_actions_and_missing_fields() {
        assert_eq!(from_ui_event("nope", Some("C:/g"), Some(true)), None);
        assert_eq!(from_ui_event("qos", None, Some(true)), None);
        assert_eq!(from_ui_event("qos", Some("C:/g"), None), None);
    }

    #[test]
    fn training_events_map_to_commands() {
        assert_eq!(from_training_event("start_training", None), Some(Command::StartTraining));
        assert_eq!(from_training_event("stop_training", None), Some(Command::StopTraining));
        assert_eq!(from_training_event("disable_rapo", None), Some(Command::SetRapo(None)));
        assert_eq!(from_training_event("select_training_target", Some(7)), Some(Command::SelectTrainingTarget { pid: 7 }));
        assert_eq!(from_training_event("select_training_target", None), None);
        assert_eq!(from_training_event("qos", None), None);
    }

    #[test]
    fn ipc_rejects_invalid_process_ids_and_missing_checkbox_state() {
        for pid in ["-1", "4294967296", "\"7\"", "null", "7.5"] {
            assert_eq!(from_json(&format!(r#"{{"action":"toggle-process","pid":{pid}}}"#)), None);
        }
        assert_eq!(from_json(r#"{"action":"toggle-process","pid":7}"#), Some(Command::ToggleProcess { pid: 7 }));
        assert_eq!(from_json(r#"{"action":"windowed"}"#), None);
        assert_eq!(from_json(r#"{"action":"windowed","checked":false}"#), Some(Command::SetWindowed(false)));
        assert_eq!(from_json(r#"{"action":"search","text":"AION 2"}"#), Some(Command::SetSearch("AION 2".into())));
        assert_eq!(from_json("not JSON"), None);
    }
}
