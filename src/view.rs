//! Pure derivations used by the UI. Mirrors the egui render logic exactly so the
//! webview port can reuse them without depending on `App`.

use crate::learning::Settings;

/// Visual tone of a status badge or metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Yellow,
    Danger,
    Muted,
}

/// Per-process state shown in the running-games list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    TrainingPaused,
    Training,
    Optimizing,
    Standby,
    Inactive,
}

impl ProcessState {
    pub fn label(self) -> &'static str {
        match self {
            Self::TrainingPaused => "RAPO PAUSADO",
            Self::Training => "RAPO MEDINDO",
            Self::Optimizing => "OTIMIZANDO AGORA",
            Self::Standby => "STANDBY",
            Self::Inactive => "INATIVO",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Self::TrainingPaused => Tone::Yellow,
            Self::Training | Self::Optimizing => Tone::Accent,
            Self::Standby => Tone::Yellow,
            Self::Inactive => Tone::Muted,
        }
    }
}

/// `training_paused` is `Some(paused)` only when the active training targets this process.
pub fn process_state(
    training_paused: Option<bool>,
    boosted: bool,
    pinned: bool,
    has_qos: bool,
    enabled: bool,
) -> ProcessState {
    match training_paused {
        Some(true) => ProcessState::TrainingPaused,
        Some(false) => ProcessState::Training,
        None if boosted || pinned || has_qos => ProcessState::Optimizing,
        None if enabled => ProcessState::Standby,
        None => ProcessState::Inactive,
    }
}

/// Headline state of the RAPO hero card while a training session exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrainingStatus {
    Paused,
    Baseline,
    Candidate,
}

impl TrainingStatus {
    /// `candidate` is the setting currently under measurement (winner or search cursor).
    pub fn of(paused: bool, candidate: Settings) -> Self {
        if paused {
            Self::Paused
        } else if candidate.bits() == 0 {
            Self::Baseline
        } else {
            Self::Candidate
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Self::Paused => Tone::Yellow,
            Self::Baseline | Self::Candidate => Tone::Accent,
        }
    }
}

/// One-line summary of the candidate settings, identical to the RAPO hero line.
pub fn candidate_summary(candidate: Settings) -> String {
    format!(
        "Candidato: prioridade {} · afinidade {} · até {} núcleos físicos · até {} threads críticas · SMT {} · somente P-cores {} · limiar {}% · memória {}",
        candidate.priority, candidate.pin, candidate.cores, candidate.threads,
        candidate.smt, candidate.p_only, candidate.min_percent, candidate.qos
    )
}

/// Library search: empty query matches everything; otherwise case-insensitive substring.
/// `search_name` must already be lowercase, as stored in `App::game_search_names`.
pub fn library_matches(query: &str, search_name: &str) -> bool {
    let query = query.to_lowercase();
    query.is_empty() || search_name.contains(&query)
}

/// Memory label: MB below 1024, otherwise GB with two decimals.
pub fn memory_label(mb: f32) -> String {
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// Per-core load tone: >85% danger, >50% yellow, otherwise accent.
pub fn cpu_load_tone(load: f32) -> Tone {
    if load > 0.85 {
        Tone::Danger
    } else if load > 0.50 {
        Tone::Yellow
    } else {
        Tone::Accent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(priority: bool, pin: bool, qos: bool) -> Settings {
        Settings { priority, pin, qos, ..Settings::default() }
    }

    #[test]
    fn training_state_takes_precedence_over_optimization() {
        assert_eq!(process_state(Some(true), true, true, true, true), ProcessState::TrainingPaused);
        assert_eq!(process_state(Some(false), false, false, false, false), ProcessState::Training);
    }

    #[test]
    fn optimizing_when_boosted_pinned_or_qos() {
        assert_eq!(process_state(None, true, false, false, false), ProcessState::Optimizing);
        assert_eq!(process_state(None, false, true, false, false), ProcessState::Optimizing);
        assert_eq!(process_state(None, false, false, true, false), ProcessState::Optimizing);
    }

    #[test]
    fn standby_when_enabled_but_idle_and_inactive_otherwise() {
        assert_eq!(process_state(None, false, false, false, true), ProcessState::Standby);
        assert_eq!(process_state(None, false, false, false, false), ProcessState::Inactive);
    }

    #[test]
    fn paused_wins_over_candidate_bits() {
        assert_eq!(TrainingStatus::of(true, settings(true, true, true)), TrainingStatus::Paused);
    }

    #[test]
    fn zero_bits_is_baseline_measurement() {
        assert_eq!(TrainingStatus::of(false, settings(false, false, false)), TrainingStatus::Baseline);
        assert_eq!(TrainingStatus::of(false, settings(false, false, true)), TrainingStatus::Candidate);
    }

    #[test]
    fn candidate_summary_lists_every_setting() {
        let s = candidate_summary(Settings { priority: true, pin: false, qos: true, cores: 6, threads: 4, smt: false, p_only: true, ..Settings::default() });
        assert_eq!(
            s,
            "Candidato: prioridade true · afinidade false · até 6 núcleos físicos · até 4 threads críticas · SMT false · somente P-cores true · limiar 5% · memória true"
        );
    }

    #[test]
    fn library_search_is_case_insensitive_substring() {
        assert!(library_matches("", "anything"));
        assert!(library_matches("AION", "aion 2"));
        assert!(!library_matches("zelda", "aion 2"));
    }

    #[test]
    fn memory_label_switches_unit_at_one_gib() {
        assert_eq!(memory_label(512.0), "512 MB");
        assert_eq!(memory_label(1024.0), "1.00 GB");
        assert_eq!(memory_label(1536.0), "1.50 GB");
    }

    #[test]
    fn cpu_tone_boundaries() {
        assert_eq!(cpu_load_tone(0.85), Tone::Yellow);
        assert_eq!(cpu_load_tone(0.851), Tone::Danger);
        assert_eq!(cpu_load_tone(0.50), Tone::Accent);
        assert_eq!(cpu_load_tone(0.501), Tone::Yellow);
    }
}
