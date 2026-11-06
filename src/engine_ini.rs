//! Opt-in AION 2 Engine.ini presets and options from the Astral Vision pack. At most one preset plus any number
//! of extra options are written inside a marked block at the end of the file. The block is removed when the
//! switch is Off or RAPO closes; lines outside the block keep their bytes.

use std::{collections::HashSet, fs, io, path::{Path, PathBuf}, sync::OnceLock};

pub struct Asset {
    pub key: &'static str,
    pub label: &'static str,
    pub text: &'static str,
    /// Presets are mutually exclusive; the other options combine freely.
    pub preset: bool,
}

pub const OPTIONS: [Asset; 7] = [
    Asset { key: "very-low", label: "1- Very Low", text: include_str!("../assets/engine-ini/very-low.ini"), preset: true },
    Asset { key: "default", label: "2- Default", text: include_str!("../assets/engine-ini/default.ini"), preset: true },
    Asset { key: "normal", label: "3- Normal", text: include_str!("../assets/engine-ini/normal.ini"), preset: true },
    Asset { key: "high", label: "4- High", text: include_str!("../assets/engine-ini/high.ini"), preset: true },
    Asset { key: "very-high", label: "5- Very High", text: include_str!("../assets/engine-ini/very-high.ini"), preset: true },
    Asset { key: "additions", label: "Additions", text: include_str!("../assets/engine-ini/additions.ini"), preset: false },
    Asset { key: "reduce-stutters", label: "Reduce Stutters", text: include_str!("../assets/engine-ini/reduce-stutters.ini"), preset: false },
];

const N: usize = OPTIONS.len();
type Flags = [bool; N];

const BEGIN: &str = "; RAPO BEGIN - removido automaticamente ao fechar o RAPO";
const END: &str = "; RAPO END";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Option_ {
    pub key: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct State {
    pub path: String,
    /// On/Off switch of the saved profile.
    pub active: bool,
    /// True while the file holds the RAPO block.
    pub applied: bool,
    pub has_backup: bool,
    pub options: Vec<Option_>,
    pub error: String,
}

pub struct EngineIni {
    path: Option<PathBuf>,
    /// Saved profile: which options the user checked. Survives RAPO restarts.
    selected: Flags,
    /// Saved On/Off switch; the file holds `selected` only while this is on.
    active: bool,
    applied: bool,
    /// False in tests: the profile is not read from or saved to the registry.
    persist: bool,
    error: String,
}

impl EngineIni {
    /// `%LOCALAPPDATA%\AION2\Saved_Steam\Config\Windows\Engine.ini` with the profile saved in
    /// `HKCU\Software\RintelRAPO\EngineIni`. A new user starts Off with nothing checked; a block left
    /// by a crashed session is replaced by the saved profile (or removed when it is Off).
    pub fn new() -> Self {
        let path = std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join(r"AION2\Saved_Steam\Config\Windows\Engine.ini"));
        let mut ini = Self::at(path);
        ini.persist = true;
        if ini.path.is_none() { ini.error = "LOCALAPPDATA ausente; Engine.ini do AION 2 não localizado".into(); }
        let loaded = load_profile();
        if let Ok(Some(saved)) = &loaded { ini.selected = saved.selected; ini.active = saved.active; }
        ini.sync(ini.active);
        if let Err(error) = loaded { ini.error = format!("Perfil do Engine.ini: {error}"); }
        ini
    }

    pub fn at(path: Option<PathBuf>) -> Self {
        Self { path, selected: [false; N], active: false, applied: false, persist: false, error: String::new() }
    }

    /// Checks or unchecks one option. Checking a preset unchecks the other presets.
    pub fn set(&mut self, index: usize, on: bool) {
        let Some(asset) = OPTIONS.get(index) else { return };
        if on && asset.preset {
            for (i, other) in OPTIONS.iter().enumerate() { if other.preset { self.selected[i] = false; } }
        }
        self.selected[index] = on;
        self.sync(self.active);
        self.save();
    }

    /// On writes the checked options; Off removes RAPO's block and keeps the profile saved.
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
        self.sync(on);
        self.save();
    }

    fn save(&mut self) {
        if !self.persist { return; }
        let saved = Saved { selected: self.selected, active: self.active };
        if let Err(error) = save_profile(&saved) { self.error = format!("Salvar perfil do Engine.ini: {error}"); }
    }

    /// Makes the file hold the checked options when `on`, and no RAPO block otherwise.
    fn sync(&mut self, on: bool) {
        let Some(path) = self.path.clone() else { return };
        let block: Vec<&str> = if on {
            OPTIONS.iter().zip(self.selected).filter(|(_, on)| *on).flat_map(|(asset, _)| asset.text.lines()).filter(|line| !line.trim().is_empty()).collect()
        } else {
            Vec::new()
        };
        match rewrite(&path, &block) {
            Ok(applied) => { self.applied = applied; self.error.clear(); }
            // The file keeps its previous content; `applied` keeps reporting what it really holds.
            Err(error) => self.error = error,
        }
    }

    pub fn backup_path(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_extension("ini.rapo-bak"))
    }

    pub fn backup(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else { return Err("Caminho do Engine.ini não localizado".into()); };
        let Some(bak) = self.backup_path() else { return Err("Caminho do backup inválido".into()); };
        if !path.exists() {
            let err = "Engine.ini não encontrado para criar backup".to_string();
            self.error = err.clone();
            return Err(err);
        }
        fs::copy(path, &bak).map_err(|e| {
            let err = format!("Criar backup: {e}");
            self.error = err.clone();
            err
        })?;
        self.error.clear();
        Ok(())
    }

    pub fn restore(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else { return Err("Caminho do Engine.ini não localizado".into()); };
        let Some(bak) = self.backup_path() else { return Err("Caminho do backup inválido".into()); };
        if !bak.exists() {
            let err = "Nenhum backup encontrado para restaurar".to_string();
            self.error = err.clone();
            return Err(err);
        }
        let text = fs::read_to_string(&bak).map_err(|e| {
            let err = format!("Ler backup: {e}");
            self.error = err.clone();
            err
        })?;
        let cleaned = clean_rapo_artifacts(&text);
        write_replacing(path, &cleaned).map_err(|e| {
            let err = format!("Restaurar Engine.ini: {e}");
            self.error = err.clone();
            err
        })?;
        self.active = false;
        self.applied = false;
        self.save();
        self.error.clear();
        Ok(())
    }

    pub fn set_error(&mut self, err: String) {
        self.error = err;
    }

    pub fn state(&self) -> State {
        State {
            path: self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            active: self.active,
            applied: self.applied,
            has_backup: self.backup_path().is_some_and(|p| p.exists()),
            options: OPTIONS.iter().zip(self.selected).map(|(asset, enabled)| Option_ { key: asset.key.into(), label: asset.label.into(), enabled }).collect(),
            error: self.error.clone(),
        }
    }
}

impl Drop for EngineIni {
    /// Closing RAPO removes its block from the file but keeps the saved profile and On/Off switch.
    fn drop(&mut self) { self.sync(false); }
}

/// Writes the block (empty = none) and cleans RAPO lines. Returns whether the file now holds the block.
fn rewrite(path: &Path, block: &[&str]) -> Result<bool, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound && block.is_empty() => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("Engine.ini não encontrado; abra o AION 2 uma vez para criá-lo".into()),
        Err(error) => return Err(format!("Ler Engine.ini: {error}")),
    };
    let base = clean_rapo_artifacts(&text);
    let new = if block.is_empty() { base } else { apply_block(&base, block) };
    if new != text { write_replacing(path, &new).map_err(|error| format!("Gravar Engine.ini: {error}"))?; }
    Ok(new.contains(BEGIN))
}

#[cfg(any(windows, test))]
fn keys(selected: &Flags) -> String {
    OPTIONS.iter().zip(selected).filter(|(_, on)| **on).map(|(asset, _)| asset.key).collect::<Vec<_>>().join(";")
}

#[cfg(any(windows, test))]
fn flags(keys: &str) -> Flags { std::array::from_fn(|i| keys.split(';').any(|k| k == OPTIONS[i].key)) }

struct Saved { selected: Flags, active: bool }

/// Profile format: `Active` DWORD, `Options` = enabled keys.
#[cfg(windows)]
fn load_profile() -> Result<Option<Saved>, String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(PROFILE_KEY) {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let options: String = key.get_value("Options").unwrap_or_default();
    let active: u32 = key.get_value("Active").unwrap_or(0);
    Ok(Some(Saved { selected: flags(&options), active: active != 0 }))
}

#[cfg(windows)]
const PROFILE_KEY: &str = r"Software\RintelRAPO\EngineIni";

#[cfg(windows)]
fn save_profile(saved: &Saved) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(PROFILE_KEY).map_err(|e| e.to_string())?;
    key.set_value("Options", &keys(&saved.selected)).map_err(|e| e.to_string())?;
    key.set_value("Active", &u32::from(saved.active)).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn load_profile() -> Result<Option<Saved>, String> { Ok(None) }

#[cfg(not(windows))]
fn save_profile(_: &Saved) -> Result<(), String> { Ok(()) }

/// Write-then-rename so a crash mid-write cannot truncate the game's config.
fn write_replacing(path: &Path, text: &str) -> io::Result<()> {
    let temp = path.with_extension("ini.rapo-tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path).inspect_err(|_| { let _ = fs::remove_file(&temp); })
}

fn eol(text: &str) -> &'static str { if text.contains("\r\n") { "\r\n" } else { "\n" } }

fn is_header(line: &str) -> bool { line.trim_start().starts_with('[') }

/// Appends the marked block at the end of the file; later keys win, so the checked options come last.
fn apply_block(base: &str, block: &[&str]) -> String {
    let eol = eol(base);
    let mut out = base.to_owned();
    if !out.is_empty() && !out.ends_with('\n') { out.push_str(eol); }
    out.push_str(BEGIN);
    out.push_str(eol);
    for line in block {
        out.push_str(line.trim_end());
        out.push_str(eol);
    }
    out.push_str(END);
    out.push_str(eol);
    out
}

/// Removes every complete `BEGIN..END` block. A lone marker keeps the lines after it.
fn remove_block(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == BEGIN {
            if let Some(end) = lines[i + 1..].iter().position(|line| line.trim() == END) {
                i += end + 2;
                continue;
            }
        }
        out.push_str(lines[i]);
        i += 1;
    }
    out
}

fn rapo_headers() -> &'static HashSet<String> {
    static HEADERS: OnceLock<HashSet<String>> = OnceLock::new();
    HEADERS.get_or_init(|| {
        let mut set: HashSet<String> = OPTIONS.iter()
            .flat_map(|asset| asset.text.lines())
            .filter(|line| is_header(line))
            .map(|line| line.trim().to_ascii_lowercase())
            .collect();
        for h in [
            "[/Script/Engine.Engine]",
            "[ConsoleVariables]",
            "[SystemSettings]",
            "[/Script/Engine.StreamingSettings]",
            "[/Script/Engine.GarbageCollectionSettings]",
            "[Core.Log]",
            "[/Script/Engine.RendererSettings]",
            "[Core.System]",
        ] {
            set.insert(h.to_ascii_lowercase());
        }
        set
    })
}

/// `key=value` pairs of every option, normalized. The game rewrites Engine.ini on exit without RAPO's markers,
/// so these lines are removed wherever they are.
fn rapo_pairs() -> &'static HashSet<String> {
    static PAIRS: OnceLock<HashSet<String>> = OnceLock::new();
    PAIRS.get_or_init(|| {
        let mut set: HashSet<String> = OPTIONS.iter().flat_map(|asset| asset.text.lines())
            .filter(|line| !line.trim_start().starts_with(';') && !is_header(line))
            .filter_map(|line| {
                let trimmed = line.trim();
                trimmed.split_once('=').map(|(k, v)| format!("{}={}", k.trim().trim_start_matches('+'), v.trim()))
            })
            .collect();
        for pair in [
            "gc.TimeBetweenPurgingPendingKillObjects=60",
            "t.OverrideFPS=1",
            "t.MaxFPS=0",
        ] {
            if let Some((k, v)) = pair.split_once('=') {
                set.insert(format!("{}={}", k.trim().trim_start_matches('+'), v.trim()));
            }
        }
        set
    })
}

fn remove_pairs(text: &str) -> String {
    let pairs = rapo_pairs();
    text.split_inclusive('\n').filter(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with(';') || is_header(trimmed) { return true; }
        let Some((k, v)) = trimmed.split_once('=') else { return true };
        let norm_k = k.trim().trim_start_matches('+');
        !pairs.contains(&format!("{}={}", norm_k, v.trim()))
    }).collect()
}

fn remove_empty_rapo_headers(text: &str) -> String {
    let headers = rapo_headers();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if is_header(trimmed) && headers.contains(&trimmed.to_ascii_lowercase()) {
            let mut has_content = false;
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j].trim();
                if is_header(next) { break; }
                if !next.is_empty() && !next.starts_with(';') {
                    has_content = true;
                    break;
                }
                j += 1;
            }
            if !has_content {
                i = j;
                continue;
            }
        }
        out.push_str(lines[i]);
        i += 1;
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{}{}", trimmed, eol(&out))
    }
}

fn clean_rapo_artifacts(text: &str) -> String {
    let unblocked = remove_block(text);
    let unpaired = remove_pairs(&unblocked);
    remove_empty_rapo_headers(&unpaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("rapo-engine-ini-{}-{name}.ini", std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    fn enabled(ini: &EngineIni) -> Vec<bool> { ini.state().options.iter().map(|o| o.enabled).collect() }

    #[test]
    fn switch_writes_checked_options_and_keeps_other_lines() {
        let path = file("switch", "[SystemSettings]\r\nKeep=1\r\n");
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.set(0, true);
        ini.set(5, true);
        ini.set_active(true);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(BEGIN));
        assert!(text.contains("DisplayGamma=2.5"));
        assert!(text.contains("Keep=1"));
        assert!(ini.state().applied);
        drop(ini);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn off_restores_original_bytes() {
        let original = "[SystemSettings]\r\nKeep=1\r\n";
        let path = file("off", original);
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.set(0, true);
        ini.set(6, true);
        ini.set_active(true);
        ini.set_active(false);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!ini.state().applied);
        drop(ini);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn presets_are_exclusive_and_extras_combine() {
        let mut ini = EngineIni::at(None);
        ini.set(0, true);
        ini.set(3, true);
        ini.set(5, true);
        assert_eq!(enabled(&ini), vec![false, false, false, true, false, true, false]);
    }

    #[test]
    fn game_rewrite_without_markers_is_cleaned_on_off() {
        let path = file("rewrite", "[SystemSettings]\nr.Streaming.PoolSize=768\nKeep=1\n");
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.set(6, true);
        ini.set_active(false);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("r.Streaming.PoolSize"));
        assert!(text.contains("Keep=1"));
        drop(ini);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn keys_round_trip() {
        let selected = [true, false, false, false, false, true, true];
        assert_eq!(keys(&selected), "very-low;additions;reduce-stutters");
        assert_eq!(super::flags(&keys(&selected)), selected);
    }
    #[test]
    fn off_and_clean_removes_unreal_headers_added_by_rapo_when_empty() {
        let dirty = "\
[/Script/Engine.Engine]
r.AOGlobalDFResolution=0
[ConsoleVariables]
r.AOGlobalDFResolution=0
[SystemSettings]
Keep=1
niagara.CreateShadersOnLoad=1
[/Script/Engine.StreamingSettings]
s.AsyncLoadingTimeLimit=4
[/Script/Engine.GarbageCollectionSettings]
gc.MultithreadedDestructionEnabled=1
[Core.Log]
LogLinker=All Off
";
        let cleaned = clean_rapo_artifacts(dirty);
        assert!(!cleaned.contains("[/Script/Engine.Engine]"), "header removed when empty");
        assert!(!cleaned.contains("[ConsoleVariables]"), "header removed when empty");
        assert!(!cleaned.contains("[/Script/Engine.StreamingSettings]"), "header removed when empty");
        assert!(!cleaned.contains("[/Script/Engine.GarbageCollectionSettings]"), "header removed when empty");
        assert!(!cleaned.contains("[Core.Log]"), "header removed when empty");
        assert!(cleaned.contains("[SystemSettings]\nKeep=1"), "user key kept under header");
    }

    #[test]
    fn backup_and_restore_cycle_recovers_original_file_and_cleans_tags() {
        let original = "[SystemSettings]\r\nUserCustom=99\r\n";
        let path = file("bak_cycle", original);
        let mut ini = EngineIni::at(Some(path.clone()));
        assert!(!ini.state().has_backup);
        ini.backup().unwrap();
        assert!(ini.state().has_backup);
        // Aplica preset
        ini.set(0, true);
        ini.set(6, true);
        ini.set_active(true);
        assert!(fs::read_to_string(&path).unwrap().contains("UserCustom=99"));
        assert!(fs::read_to_string(&path).unwrap().contains(BEGIN));
        // Restaura
        ini.restore().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!ini.state().applied);
        assert!(!ini.state().active);
        if let Some(bak) = ini.backup_path() { let _ = fs::remove_file(bak); }
        drop(ini);
        fs::remove_file(path).unwrap();
    }
}
