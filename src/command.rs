//! UI intents. The render only records them; `App::apply` runs the side effects
//! after the frame, so the render itself stays read-only.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Desired optimizer state, computed from the state the frame was drawn with.
    SetOptimizerRunning(bool),
    Refresh,
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
}

/// Maps a webview event (`data-action`, `data-dir`, checkbox state) to a command.
/// Unknown actions or missing fields yield `None`, so the event is ignored.
pub fn from_ui_event(action: &str, dir: Option<&str>, checked: Option<bool>) -> Option<Command> {
    let directory = || dir.map(PathBuf::from);
    match action {
        "qos" => Some(Command::SetQos { directory: directory()?, on: checked? }),
        "pcores" => Some(Command::SetPin { directory: directory()?, on: checked? }),
        "auto" => Some(Command::SetAuto { directory: directory()?, on: checked? }),
        "rapo" => Some(Command::SetRapo(if checked? { directory() } else { None })),
        "apo-profile" => Some(Command::ApplyAion2Profile { directory: directory()? }),
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
}
