use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub priority: bool,
    pub pin: bool,
    pub qos: bool,
    pub threads: u8,
    pub min_percent: u8,
    pub cores: u8,
    pub smt: bool,
    pub p_only: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            priority: false,
            pin: false,
            qos: false,
            threads: 4,
            min_percent: 5,
            cores: 64,
            smt: true,
            p_only: true,
        }
    }
}
impl Settings {
    pub fn bits(self) -> u8 {
        self.priority as u8 | ((self.pin as u8) << 1) | ((self.qos as u8) << 2)
    }
    fn valid(self) -> bool {
        (1..=64).contains(&self.threads)
            && (1..=20).contains(&self.min_percent)
            && (1..=64).contains(&self.cores)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Score {
    pub fps: f64,
    pub low_fps: f64,
}
impl Score {
    fn valid(self) -> bool {
        self.fps.is_finite()
            && self.low_fps.is_finite()
            && self.fps > 0.0
            && self.low_fps > 0.0
            && self.low_fps <= self.fps
    }
    pub(crate) fn rank(self) -> f64 {
        2.0 / (1.0 / self.fps + 1.0 / self.low_fps)
    }
    pub(crate) fn improves(self, previous: Self) -> bool {
        self.fps >= previous.fps
            && self.low_fps >= previous.low_fps
            && self.rank() > previous.rank() * 1.01
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    pub threads: u8,
    pub cores: u8,
    pub p_cores: u8,
    pub smt: bool,
}

struct Observation {
    settings: Settings,
    count: usize,
    mean: Score,
    consecutive_gains: u8,
    rejected: bool,
}
impl Observation {
    fn new(settings: Settings) -> Self {
        Self { settings, count: 0, mean: Score::default(), consecutive_gains: 0, rejected: false }
    }
}
pub struct Search {
    observations: Vec<Observation>,
    current: usize,
    reference: usize,
    reference_score: Option<Score>,
    pending: Option<(usize, Score)>,
    completed: usize,
    pairs: usize,
    random: u64,
    limits: Limits,
}
impl Search {
    pub fn new(seed: u64, limits: Limits, reference: Settings) -> Self {
        let mut search = Self {
            observations: vec![Observation::new(Settings::default())],
            current: 0, reference: 0, reference_score: None, pending: None,
            completed: 0, pairs: 0, random: seed.max(1), limits,
        };
        let reference = search.normalize(reference);
        if reference != Settings::default() {
            search.observations.push(Observation::new(reference));
            search.reference = 1;
            search.current = 1;
        }
        search
    }
    fn normalize(&self, mut settings: Settings) -> Settings {
        settings.pin &= self.limits.threads > 0;
        if settings.pin {
            let cores = if settings.p_only { self.limits.p_cores } else { self.limits.cores };
            settings.cores = settings.cores.clamp(1, cores.max(1));
            settings.smt &= self.limits.smt;
            settings.threads = settings.threads.clamp(1, self.limits.threads.max(1));
            settings.min_percent = settings.min_percent.clamp(1, 20);
            if !settings.smt {
                settings.threads = settings.threads.min(settings.cores);
            }
        } else {
            settings.threads = 4;
            settings.min_percent = 5;
            settings.cores = 64;
            settings.smt = true;
            settings.p_only = true;
        }
        settings
    }
    pub fn current(&self) -> Settings {
        self.observations[self.current].settings
    }
    pub fn completed(&self) -> usize { self.completed }
    pub fn explored(&self) -> usize { self.observations.len() }
    pub fn reject_current(&mut self) {
        self.observations[self.current].rejected = true;
        self.pending = None;
        if self.current == self.reference {
            self.reference = 0;
            self.reference_score = None;
            self.reset_comparisons();
        }
        self.current = self.reference;
    }
    fn random(&mut self) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random
    }
    pub fn best(&self) -> Option<(Settings, Score)> {
        self.reference_score.map(|score| (self.observations[self.reference].settings, score))
    }
    fn reset_comparisons(&mut self) {
        for observation in &mut self.observations {
            observation.count = 0;
            observation.mean = Score::default();
            observation.consecutive_gains = 0;
        }
    }
    fn choose_next(&mut self, promising: Option<usize>) {
        if let Some(index) = promising {
            self.current = index;
            return;
        }
        if self.pairs % 7 == 0 && self.reference != 0 && !self.observations[0].rejected {
            self.current = 0;
            return;
        }
        if self.pairs % 3 == 0 {
            if let Some((index, _)) = self.observations.iter().enumerate()
                .filter(|(index, observation)| *index != self.reference && !observation.rejected)
                .max_by(|(_, a), (_, b)| {
                    let bound = |o: &Observation| o.mean.rank()
                        + 0.25 * (((self.pairs + 1) as f64).ln() / o.count.max(1) as f64).sqrt();
                    bound(a).total_cmp(&bound(b))
                }) {
                self.current = index;
                return;
            }
        }
        for _ in 0..24 {
            let mut candidate = self.observations[self.reference].settings;
            if self.random() % 4 == 0 {
                // Whole candidates escape combinations that single-setting hill climbing cannot reach.
                let bits = self.random();
                candidate.priority = bits & 1 != 0;
                candidate.qos = bits & 2 != 0;
                candidate.pin = bits & 4 != 0;
                candidate.smt = bits & 8 != 0;
                candidate.p_only = bits & 16 != 0;
                candidate.cores = 1 + (self.random() % u64::from(self.limits.cores.max(1))) as u8;
                candidate.threads = 1 + (self.random() % u64::from(self.limits.threads.max(1))) as u8;
                candidate.min_percent = 1 + (self.random() % 20) as u8;
            } else {
                match self.random() % if self.limits.threads > 0 { 8 } else { 2 } {
                    0 => candidate.priority = !candidate.priority,
                    1 => candidate.qos = !candidate.qos,
                    2 => candidate.pin = !candidate.pin,
                    3 => { candidate.pin = true; candidate.threads = 1 + (self.random() % u64::from(self.limits.threads)) as u8; }
                    4 => { candidate.pin = true; candidate.min_percent = 1 + (self.random() % 20) as u8; }
                    5 => { candidate.pin = true; candidate.cores = 1 + (self.random() % u64::from(self.limits.cores.max(1))) as u8; }
                    6 if self.limits.smt => { candidate.pin = true; candidate.smt = !candidate.smt; }
                    7 if self.limits.p_cores < self.limits.cores => { candidate.pin = true; candidate.p_only = !candidate.p_only; }
                    _ => continue,
                }
            }
            let candidate = self.normalize(candidate);
            if !self.observations.iter().any(|o| o.settings == candidate) {
                self.observations.push(Observation::new(candidate));
                self.current = self.observations.len() - 1;
                return;
            }
        }
        self.current = self.observations.iter().enumerate()
            .filter(|(index, o)| *index != self.reference && !o.rejected)
            .min_by_key(|(_, o)| o.count)
            .map_or(self.reference, |(index, _)| index);
    }
    pub fn record(&mut self, score: Score) -> Result<Option<(Settings, Score)>, String> {
        if !score.valid() { return Err("Medição inválida".into()); }
        self.completed += 1;
        if self.current != self.reference {
            self.pending = Some((self.current, score));
            self.current = self.reference;
            return Ok(None);
        }
        self.reference_score = Some(score);
        let mut improvement = None;
        let mut promising = None;
        if let Some((index, trial)) = self.pending.take() {
            self.pairs += 1;
            let ratio = Score { fps: trial.fps / score.fps, low_fps: trial.low_fps / score.low_fps };
            let observation = &mut self.observations[index];
            observation.count = (observation.count + 1).min(5);
            observation.mean.fps += (ratio.fps - observation.mean.fps) / observation.count as f64;
            observation.mean.low_fps += (ratio.low_fps - observation.mean.low_fps) / observation.count as f64;
            if ratio.improves(Score { fps: 1.0, low_fps: 1.0 }) {
                observation.consecutive_gains += 1;
                if observation.consecutive_gains >= 3
                    && observation.mean.improves(Score { fps: 1.0, low_fps: 1.0 }) {
                    improvement = Some((observation.settings, trial));
                    self.reference = index;
                    self.reference_score = Some(trial);
                    self.reset_comparisons();
                } else {
                    promising = Some(index);
                }
            } else {
                observation.consecutive_gains = 0;
            }
        }
        self.choose_next(promising);
        Ok(improvement)
    }
}
#[derive(Clone, Debug)]
pub struct Profile {
    pub exe: PathBuf,
    pub settings: Settings,
    pub score: Score,
}

#[cfg(windows)]
pub fn save_profile(profile: &Profile) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    if !profile.score.valid() || !profile.settings.valid() {
        return Err("Perfil inválido".into());
    }
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\RintelRAPO\\LearnedProfiles")
        .map_err(|e| e.to_string())?;
    let value = format!(
        "3|{}|{}|{}|{}|{}|{}",
        profile.settings.bits()
            | ((profile.settings.smt as u8) << 3)
            | ((profile.settings.p_only as u8) << 4),
        profile.settings.threads,
        profile.settings.min_percent,
        profile.settings.cores,
        profile.score.fps,
        profile.score.low_fps
    );
    key.set_value(profile.exe.as_os_str(), &value)
        .map_err(|e| e.to_string())
}
#[cfg(windows)]
pub fn load_profiles() -> Result<Vec<Profile>, String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let key = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\RintelRAPO\\LearnedProfiles")
    {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    key.enum_values()
        .map(|entry| {
            let (exe, _) = entry.map_err(|e| e.to_string())?;
            let text: String = key.get_value(&exe).map_err(|e| e.to_string())?;
            decode(PathBuf::from(exe), &text)
        })
        .collect()
}
fn decode(exe: PathBuf, text: &str) -> Result<Profile, String> {
    let invalid = || "Perfil salvo inválido".to_string();
    let parts: Vec<_> = text.split('|').collect();
    let legacy = parts.len() == 6 && parts[0] == "2";
    if !legacy && !(parts.len() == 7 && parts[0] == "3") {
        return Err(invalid());
    }
    let bits: u8 = parts[1].parse().map_err(|_| invalid())?;
    let settings = Settings {
        priority: bits & 1 != 0,
        pin: bits & 2 != 0,
        qos: bits & 4 != 0,
        threads: parts[2].parse().map_err(|_| invalid())?,
        min_percent: parts[3].parse().map_err(|_| invalid())?,
        cores: if legacy {
            64
        } else {
            parts[4].parse().map_err(|_| invalid())?
        },
        smt: legacy || bits & 8 != 0,
        p_only: legacy || bits & 16 != 0,
    };
    let score = Score {
        fps: parts[if legacy { 4 } else { 5 }]
            .parse()
            .map_err(|_| invalid())?,
        low_fps: parts[if legacy { 5 } else { 6 }]
            .parse()
            .map_err(|_| invalid())?,
    };
    if bits > if legacy { 7 } else { 31 } || !settings.valid() || !score.valid() {
        return Err(invalid());
    }
    Ok(Profile {
        exe,
        settings,
        score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_search_discovers_joint_improvements_without_single_axis_gain() {
        let mut search = Search::new(42, Limits::default(), Settings::default());
        for _ in 0..2000 {
            let settings = search.current();
            let fps = if settings.priority && settings.qos { 130.0 } else { 100.0 };
            search.record(Score { fps, low_fps: fps * 0.75 }).unwrap();
        }
        let (settings, score) = search.best().unwrap();
        assert!(settings.priority && settings.qos);
        assert_eq!(score.fps, 130.0);
    }

    #[test]
    fn paired_search_does_not_attribute_a_scene_change_to_settings() {
        let mut search = Search::new(42, Limits::default(), Settings::default());
        for window in 0..100 {
            let fps = if window < 2 { 80.0 } else { 120.0 };
            if let Some((settings, _)) = search.record(Score { fps, low_fps: fps * 0.75 }).unwrap() {
                assert_eq!(settings.bits(), 0, "identical performance must not promote an adjustment");
            }
        }
    }

    #[test]
    fn paired_search_replaces_a_historical_winner_when_current_conditions_change() {
        let mut search = Search::new(42, Limits::default(), Settings::default());
        let mut replacement = None;
        for window in 0..1000 {
            let settings = search.current();
            let fps = if window < 300 {
                if settings.priority { 120.0 } else { 100.0 }
            } else if settings.priority { 60.0 } else if settings.qos { 90.0 } else { 70.0 };
            if let Some(winner) = search.record(Score { fps, low_fps: fps * 0.75 }).unwrap() {
                if window >= 300 { replacement = Some(winner); }
            }
        }
        let (settings, score) = replacement.expect("current improvements must replace an obsolete absolute record");
        assert!(settings.qos && !settings.priority);
        assert_eq!(score.fps, 90.0);
    }

    #[test]
    fn learns_and_confirms_better_measured_configuration() {
        let mut search = Search::new(
            42,
            Limits {
                threads: 16,
                cores: 8,
                p_cores: 4,
                smt: true,
            },
            Settings::default(),
        );
        let mut result = None;
        for _ in 0..100 {
            let settings = search.current();
            let faster = settings.priority || settings.pin || settings.qos;
            result = search
                .record(Score {
                    fps: if faster { 120.0 } else { 80.0 },
                    low_fps: if faster { 90.0 } else { 60.0 },
                })
                .unwrap();
            if result.is_some_and(|(settings, _)| settings.bits() != 0) {
                break;
            }
        }
        let (settings, score) = result.or_else(|| search.best()).unwrap();
        assert_ne!(settings.bits(), 0);
        assert_eq!(score.fps, 120.0);
    }
    #[test]
    fn higher_average_does_not_promote_worse_one_percent_low() {
        let mut search = Search::new(42, Limits::default(), Settings::default());
        for _ in 0..100 {
            let changed = search.current().bits() != 0;
            if let Some((settings, _)) = search
                .record(Score {
                    fps: if changed { 200.0 } else { 80.0 },
                    low_fps: if changed { 55.0 } else { 60.0 },
                })
                .unwrap()
            {
                assert_eq!(settings.bits(), 0);
            }
        }
        assert_eq!(search.best().unwrap().0.bits(), 0);
    }
    #[test]
    fn stable_ties_keep_baseline_and_invalid_scores_do_not_advance() {
        let mut search = Search::new(
            123,
            Limits {
                threads: 2,
                cores: 2,
                p_cores: 2,
                smt: false,
            },
            Settings::default(),
        );
        assert!(search
            .record(Score {
                fps: f64::NAN,
                low_fps: 1.0
            })
            .is_err());
        assert_eq!(search.completed(), 0);
        let mut improvements = 0;
        for _ in 0..200 {
            if let Some((settings, _)) = search
                .record(Score {
                    fps: 60.0,
                    low_fps: 40.0,
                })
                .unwrap()
            {
                assert_eq!(settings, Settings::default());
                improvements += 1;
            }
        }
        assert_eq!(improvements, 0);
        assert_eq!(search.completed(), 200);
    }
    #[test]
    fn candidates_follow_available_processors() {
        for limit in [0, 1, 2, 16, 64] {
            let mut search = Search::new(
                42,
                Limits {
                    threads: limit,
                    cores: limit,
                    p_cores: limit,
                    smt: false,
                },
                Settings::default(),
            );
            for _ in 0..1000 {
                let current = search.current();
                if current.pin {
                    assert!(current.threads >= 1 && current.threads <= limit);
                }
                search
                    .record(Score {
                        fps: 60.0,
                        low_fps: 40.0,
                    })
                    .unwrap();
            }
        }
    }
    #[test]
    fn explores_cores_affinity_threads_and_smt_on_hybrid_cpu() {
        let limits = Limits {
            threads: 12,
            cores: 8,
            p_cores: 4,
            smt: true,
        };
        let mut search = Search::new(42, limits, Settings::default());
        let (mut fewer_cores, mut fewer_threads, mut smt_off, mut all_cores, mut affinity_on) =
            (false, false, false, false, false);
        for _ in 0..2000 {
            let current = search.current();
            if current.pin {
                affinity_on = true;
                assert!(current.cores > 0 && current.cores <= if current.p_only { 4 } else { 8 });
                assert!(current.threads > 0 && current.threads <= 12);
                if !current.smt {
                    assert!(current.threads <= current.cores);
                }
                fewer_cores |= current.cores < 4;
                fewer_threads |= current.threads < 4;
                smt_off |= !current.smt;
                all_cores |= !current.p_only;
            }
            search
                .record(Score {
                    fps: 60.0,
                    low_fps: 40.0,
                })
                .unwrap();
        }
        assert!(affinity_on && fewer_cores && fewer_threads && smt_off && all_cores);
    }

    #[test]
    fn profiles_validate_stored_boundary() {
        assert_eq!(
            decode(PathBuf::from("遊戲.exe"), "2|5|3|8|120|80")
                .unwrap()
                .settings
                .threads,
            3
        );
        let extended = decode(PathBuf::from("game.exe"), "3|3|2|5|2|120|80").unwrap();
        assert_eq!(extended.settings.cores, 2);
        assert!(!extended.settings.smt);
        assert!(!extended.settings.p_only);
        for bad in [
            "3|0|4|5|60|40",
            "2|8|4|5|60|40",
            "2|0|0|5|60|40",
            "2|0|4|21|60|40",
            "2|0|4|5|NaN|40",
            "2|0|4|5|40|60",
            "3|32|4|5|2|60|40",
            "3|3|4|5|0|60|40",
        ] {
            assert!(decode(PathBuf::new(), bad).is_err());
        }
    }
    #[cfg(windows)]
    #[test]
    fn saved_profile_roundtrips_through_windows_registry() {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        let exe = PathBuf::from(format!("C:\\RAPO-test-{}\\遊戲.exe", std::process::id()));
        let profile = Profile {
            exe: exe.clone(),
            settings: Settings {
                priority: true,
                pin: true,
                threads: 3,
                min_percent: 8,
                cores: 2,
                smt: false,
                p_only: false,
                ..Settings::default()
            },
            score: Score {
                fps: 123.0,
                low_fps: 81.0,
            },
        };
        save_profile(&profile).unwrap();
        let loaded = load_profiles()
            .unwrap()
            .into_iter()
            .find(|p| p.exe == exe)
            .unwrap();
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(
                "Software\\RintelRAPO\\LearnedProfiles",
                winreg::enums::KEY_SET_VALUE,
            )
            .unwrap();
        key.delete_value(exe.as_os_str()).unwrap();
        assert_eq!(loaded.settings, profile.settings);
        assert_eq!(loaded.score.fps, 123.0);
        assert_eq!(loaded.score.low_fps, 81.0);
    }
}
