//! UI intents. The render only records them; `App::apply` runs the side effects
//! after the frame, so the render itself stays read-only.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Desired optimizer state, computed from the state the frame was drawn with.
    SetOptimizerRunning(bool),
    Refresh,
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
