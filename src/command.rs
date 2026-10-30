//! UI intents. The render only records them; `App::apply` runs the side effects
//! after the frame, so the render itself stays read-only.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Desired optimizer state, computed from the state the frame was drawn with.
    SetOptimizerRunning(bool),
}
