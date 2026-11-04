//! Opt-in AION 2 Engine.ini overrides. Every option starts off; only the options the user enables are
//! written, inside a marked block that RAPO removes when the last option is unchecked or RAPO closes.
//! Lines outside the block keep their bytes.

use std::{fs, io, path::{Path, PathBuf}};

/// AION 2 Unreal Engine `[SystemSettings]` overrides: `(key, value, label)`.
pub const ENTRIES: [(&str, &str, &str); 24] = [
    ("t.MaxFPS", "0", "Desbloquear limite de FPS (t.MaxFPS=0)"),
    ("t.Streamline.Reflex.Enable", "1", "Ativar NVIDIA Reflex"),
    ("t.Streamline.Reflex.Mode", "1", "Reflex em modo baixa latência (fila de renderização vazia)"),
    ("r.GTSyncType", "1", "Sem fila de frames: game thread espera a apresentação (RHI)"),
    ("r.OneFrameThreadLag", "0", "Sem frame extra da render thread (menos latência; pode reduzir FPS)"),
    ("r.VSync", "0", "Desativar VSync (r.VSync=0)"),
    ("r.FinishCurrentFrame", "0", "Não forçar sincronização de cada frame na CPU (mais FPS; r.FinishCurrentFrame=0)"),
    ("r.RHICmdBypass", "0", "Usar thread RHI dedicada em paralelo (r.RHICmdBypass=0)"),
    ("r.ShaderPipelineCache.Enabled", "1", "Ativar cache de pipeline de shaders (reduz engasgos)"),
    ("r.CreateShadersOnLoad", "1", "Compilar shaders no carregamento (evita stutter em combate)"),
    ("r.Streaming.PoolSize", "6144", "Pool de streaming de texturas em MB (editável; padrão 6144 = 6 GB)"),
    ("r.Streaming.LimitPoolSizeToVRAM", "1", "Limitar pool de texturas à VRAM física disponível"),
    ("r.Streaming.MaxTempMemoryAllowed", "512", "Memória temporária de streaming em 512 MB"),
    ("r.Streaming.NumStaticComponentsProcessedPerFrame", "64", "Processar 64 componentes estáticos por frame no streaming"),
    ("sg.ShadowQuality", "2", "Qualidade de sombras: Médio (sg.ShadowQuality=2)"),
    ("sg.EffectsQuality", "2", "Qualidade de efeitos: Médio (sg.EffectsQuality=2)"),
    ("sg.ReflectionQuality", "1", "Qualidade de reflexos: Baixo (sg.ReflectionQuality=1)"),
    ("sg.PostProcessQuality", "1", "Qualidade de pós-processamento: Baixo (sg.PostProcessQuality=1)"),
    ("sg.FoliageQuality", "2", "Qualidade de vegetação: Médio (sg.FoliageQuality=2)"),
    ("r.MotionBlurQuality", "0", "Desligar motion blur (mais FPS; muda o visual)"),
    ("r.DepthOfFieldQuality", "0", "Desligar profundidade de campo (mais FPS; muda o visual)"),
    ("r.SceneColorFringeQuality", "0", "Desligar aberração cromática (imagem mais nítida; muda o visual)"),
    ("r.FilmGrain", "0", "Desligar granulação de filme (imagem mais limpa; muda o visual)"),
    ("r.LensFlareQuality", "0", "Desligar lens flare (menos brilho na tela; muda o visual)"),
];
/// Reflex (index 1) already keeps the frame queue empty; on top of it these frame-sync options only cost FPS,
/// so they stay saved but are not written while Reflex is checked.
const REFLEX: usize = 1;
const SUPERSEDED_BY_REFLEX: [usize; 2] = [3, 4];

fn superseded(selected: &[bool; ENTRIES.len()], index: usize) -> bool {
    selected[REFLEX] && SUPERSEDED_BY_REFLEX.contains(&index)
}

/// Options whose number the user may change: `(index, min, max)`. The texture pool is in MB.
const POOL: usize = 10;
const EDITABLE: [(usize, u32, u32); 1] = [(POOL, 256, 32768)];
const MAX_OWNED: usize = 128;

fn editable(index: usize) -> Option<(u32, u32)> {
    EDITABLE.iter().find(|(i, _, _)| *i == index).map(|(_, min, max)| (*min, *max))
}

const BEGIN: &str = "; RAPO BEGIN - removido automaticamente ao fechar o RAPO";
const END: &str = "; RAPO END";
const SECTION: &str = "[SystemSettings]";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Option_ {
    pub key: String,
    pub value: String,
    pub label: String,
    pub enabled: bool,
    /// Checked but not written: Reflex already covers it.
    pub superseded: bool,
    /// The user may replace `value` with a number between `min` and `max`.
    pub editable: bool,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct State {
    pub path: String,
    /// On/Off switch of the saved profile.
    pub active: bool,
    /// True while the file holds RAPO lines.
    pub applied: bool,
    pub options: Vec<Option_>,
    /// A `t.OverrideFPS` line found in the file; it replaces frame-time measurement, so RAPO never writes it.
    pub override_fps: Option<String>,
    pub error: String,
}

pub struct EngineIni {
    path: Option<PathBuf>,
    /// Saved profile: which options the user checked. Survives RAPO restarts.
    selected: [bool; ENTRIES.len()],
    /// Saved On/Off switch; the file holds `selected` only while this is on.
    active: bool,
    /// Options present in the file; the fallback cleanup removes only these exact lines.
    written: [bool; ENTRIES.len()],
    /// Every exact `key=value` line RAPO has ever written, saved across sessions. The game rewrites Engine.ini on
    /// exit without RAPO's markers, so copies of these exact lines are removed even outside the block. A line
    /// with an older custom value stays here, so changing a value never leaves the previous one behind.
    owned: Vec<String>,
    /// User-chosen values of the editable options; `None` keeps the default.
    custom: [Option<u32>; ENTRIES.len()],
    /// False in tests: the profile is not read from or saved to the registry.
    persist: bool,
    override_fps: Option<String>,
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
        if let Ok(Some(saved)) = &loaded { ini.selected = saved.selected; ini.active = saved.active; ini.owned = saved.owned.clone(); ini.custom = saved.custom; }
        ini.sync(ini.active);
        if let Err(error) = loaded { ini.error = format!("Perfil do Engine.ini: {error}"); }
        ini
    }

    pub fn at(path: Option<PathBuf>) -> Self {
        Self { path, selected: [false; ENTRIES.len()], active: false, written: [false; ENTRIES.len()], owned: Vec::new(), custom: [None; ENTRIES.len()], persist: false, override_fps: None, error: String::new() }
    }

    /// Checks or unchecks one option of the saved profile.
    pub fn set(&mut self, index: usize, on: bool) {
        let Some(slot) = self.selected.get_mut(index) else { return };
        *slot = on;
        self.sync(self.active);
        self.save();
    }

    /// On writes the checked options; Off removes RAPO's lines and keeps the profile saved.
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
        self.sync(on);
        self.save();
    }

    /// Sets the number of an editable option (for example the texture pool in MB). Out-of-range values change nothing.
    pub fn set_value(&mut self, index: usize, value: u32) {
        let Some((min, max)) = editable(index) else { return };
        if !(min..=max).contains(&value) {
            self.error = format!("{}: use um valor entre {min} e {max}", ENTRIES[index].0);
            return;
        }
        self.custom[index] = Some(value);
        self.sync(self.active);
        self.save();
    }

    fn value(&self, index: usize) -> String { self.custom[index].map_or_else(|| ENTRIES[index].1.to_owned(), |v| v.to_string()) }

    fn save(&mut self) {
        if !self.persist { return; }
        let saved = Saved { selected: self.selected, active: self.active, owned: self.owned.clone(), custom: self.custom };
        if let Err(error) = save_profile(&saved) { self.error = format!("Salvar perfil do Engine.ini: {error}"); }
    }

    /// Makes the file hold `selected` when `on`, and no RAPO line otherwise.
    fn sync(&mut self, on: bool) {
        let Some(path) = self.path.clone() else { return };
        let wanted: [bool; ENTRIES.len()] = std::array::from_fn(|i| on && self.selected[i] && !superseded(&self.selected, i));
        let any = wanted.contains(&true);
        let new_lines: Vec<String> = (0..ENTRIES.len()).filter(|i| wanted[*i]).map(|i| format!("{}={}", ENTRIES[i].0, self.value(i))).collect();
        let owned = self.owned.clone();
        let written: Vec<(&str, &str)> = owned.iter().filter_map(|line| line.split_once('=')).collect();
        let result = (|| {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound && !any => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("Engine.ini não encontrado; abra o AION 2 uma vez para criá-lo".into()),
                Err(error) => return Err(format!("Ler Engine.ini: {error}")),
            };
            let base = remove_text(&text, &written);
            let changed = if any { Some(apply_text(base.as_deref().unwrap_or(&text), &new_lines)) } else { base };
            let changed = changed.filter(|new| *new != text);
            if let Some(new) = &changed { write_replacing(&path, new).map_err(|error| format!("Gravar Engine.ini: {error}"))?; }
            Ok(Some(changed.unwrap_or(text)))
        })();
        match result {
            Ok(text) => {
                self.written = if text.is_some() { wanted } else { [false; ENTRIES.len()] };
                let before = self.owned.len();
                if text.is_some() {
                    for line in new_lines { if !self.owned.contains(&line) { self.owned.push(line); } }
                    // Bounded: only the most recent distinct lines matter for cleaning up after the game.
                    if self.owned.len() > MAX_OWNED { self.owned.drain(..self.owned.len() - MAX_OWNED); }
                }
                if self.owned.len() != before { self.save(); }
                self.override_fps = text.as_deref().and_then(override_fps);
                self.error.clear();
            }
            // The file keeps its previous content; `applied` keeps reporting what it really holds.
            Err(error) => self.error = error,
        }
    }

    pub fn state(&self) -> State {
        State {
            path: self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            active: self.active,
            applied: self.written.contains(&true),
            options: ENTRIES.iter().zip(self.selected).enumerate().map(|(i, ((key, _, label), enabled))| {
                let (min, max) = editable(i).unwrap_or((0, 0));
                Option_ { key: key.to_string(), value: self.value(i), label: label.to_string(), enabled, superseded: enabled && superseded(&self.selected, i), editable: editable(i).is_some(), min, max }
            }).collect(),
            override_fps: self.override_fps.clone(),
            error: self.error.clone(),
        }
    }
}

impl Drop for EngineIni {
    /// Closing RAPO removes its lines from the file but keeps the saved profile and On/Off switch.
    fn drop(&mut self) { self.sync(false); }
}

#[cfg(windows)]
const PROFILE_KEY: &str = r"Software\RintelRAPO\EngineIni";

type Flags = [bool; ENTRIES.len()];

/// The profile kept in the registry.
struct Saved { selected: Flags, active: bool, owned: Vec<String>, custom: [Option<u32>; ENTRIES.len()] }

fn keys(flags: &Flags) -> String {
    ENTRIES.iter().zip(flags).filter(|(_, on)| **on).map(|((k, _, _), _)| *k).collect::<Vec<_>>().join(";")
}

fn flags(keys: &str) -> Flags {
    let keys: Vec<&str> = keys.split(';').collect();
    std::array::from_fn(|i| keys.contains(&ENTRIES[i].0))
}

/// `Owned` holds exact `key=value` lines joined by `;`. Older profiles stored bare keys: those were written with
/// the default value. Lines of unknown keys are ignored.
#[cfg(any(windows, test))]
fn parse_owned(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for token in text.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        let line = match token.split_once('=') {
            Some((key, value)) => ENTRIES.iter().find(|(k, _, _)| *k == key).map(|_| format!("{key}={value}")),
            None => ENTRIES.iter().find(|(k, _, _)| *k == token).map(|(k, v, _)| format!("{k}={v}")),
        };
        if let Some(line) = line { if !lines.contains(&line) { lines.push(line); } }
    }
    lines
}

/// `Values` holds `key=number` pairs joined by `;`; anything outside the option's range is ignored.
#[cfg(any(windows, test))]
fn parse_values(text: &str) -> [Option<u32>; ENTRIES.len()] {
    let mut custom = [None; ENTRIES.len()];
    for (key, value) in text.split(';').filter_map(|t| t.split_once('=')) {
        let Some(index) = ENTRIES.iter().position(|(k, _, _)| *k == key.trim()) else { continue };
        let (Some((min, max)), Ok(number)) = (editable(index), value.trim().parse::<u32>()) else { continue };
        if (min..=max).contains(&number) { custom[index] = Some(number); }
    }
    custom
}

#[cfg(any(windows, test))]
fn format_values(custom: &[Option<u32>; ENTRIES.len()]) -> String {
    (0..ENTRIES.len()).filter_map(|i| custom[i].map(|v| format!("{}={v}", ENTRIES[i].0))).collect::<Vec<_>>().join(";")
}

/// Profile format: `Active` DWORD, `Options` = enabled keys, `Owned` = lines RAPO ever wrote, `Values` = custom numbers.
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
    // Profiles saved before `Owned` existed: anything that was checked may have been written.
    let owned: String = key.get_value("Owned").unwrap_or_else(|_| options.clone());
    let values: String = key.get_value("Values").unwrap_or_default();
    Ok(Some(Saved { selected: flags(&options), active: active != 0, owned: parse_owned(&owned), custom: parse_values(&values) }))
}

#[cfg(windows)]
fn save_profile(saved: &Saved) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(PROFILE_KEY).map_err(|e| e.to_string())?;
    key.set_value("Options", &keys(&saved.selected)).map_err(|e| e.to_string())?;
    key.set_value("Owned", &saved.owned.join(";")).map_err(|e| e.to_string())?;
    key.set_value("Values", &format_values(&saved.custom)).map_err(|e| e.to_string())?;
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

fn join(lines: &[&str], eol: &str) -> String {
    let mut out = lines.join(eol);
    out.push_str(eol);
    out
}

fn is_header(line: &str) -> bool { line.trim_start().starts_with('[') }

/// Puts the RAPO block at the end of `[SystemSettings]` (later keys win), or appends the section inside the block.
fn apply_text(base: &str, entries: &[String]) -> String {
    let eol = eol(base);
    let mut lines: Vec<&str> = base.lines().collect();
    let mut block = vec![BEGIN];
    let section = lines.iter().position(|l| l.trim().eq_ignore_ascii_case(SECTION));
    if section.is_none() { block.push(SECTION); }
    block.extend(entries.iter().map(String::as_str));
    block.push(END);
    let at = match section {
        Some(start) => {
            let end = lines[start + 1..].iter().position(|l| is_header(l)).map_or(lines.len(), |i| start + 1 + i);
            (start + 1..end).rev().find(|&i| !lines[i].trim().is_empty()).map_or(start + 1, |i| i + 1)
        }
        // Appended as-is: no separator and no trimming, so removing BEGIN..END restores every byte.
        None => lines.len(),
    };
    lines.splice(at..at, block);
    join(&lines, eol)
}

/// Removes the marked block, then every copy of the exact `written` lines under `[SystemSettings]`:
/// the game rewrites Engine.ini on exit without comments, leaving RAPO's lines unmarked.
fn remove_text(text: &str, written: &[(&str, &str)]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = lines.clone();
    // Only a complete BEGIN..END pair is ours; a lone marker must never swallow the rest of the file.
    let begin = lines.iter().position(|l| l.trim() == BEGIN);
    let block = begin.and_then(|b| lines[b..].iter().position(|l| l.trim() == END).map(|e| b..=b + e));
    if let Some(block) = block {
        kept.drain(block);
    }
    let mut section = false;
    kept.retain(|line| {
        let trimmed = line.trim();
        if is_header(trimmed) { section = trimmed.eq_ignore_ascii_case(SECTION); return true; }
        !(section && written.iter().any(|(k, v)| trimmed.split_once('=').is_some_and(|(key, value)| key.trim().eq_ignore_ascii_case(k) && value.trim() == *v)))
    });
    (kept.len() != lines.len()).then(|| join(&kept, eol(text)))
}

fn override_fps(text: &str) -> Option<String> {
    text.lines().map(str::trim)
        .find(|l| !l.starts_with(';') && l.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("t.OverrideFPS")))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ends with a blank line, like AION 2's real Engine.ini: it must survive apply/remove.
    const GAME: &str = "[Core.System]\r\nPaths=../../../Engine/Content\r\n\r\n[/Script/Engine.RendererSettings]\r\nr.ngx.dlss.enable=0\r\n\r\n";

    fn temp(name: &str, text: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("rapo-engine-{name}-{}.ini", std::process::id()));
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn switch_writes_only_checked_options_and_off_keeps_the_selection() {
        let path = temp("optin", GAME);
        let mut ini = EngineIni::at(Some(path.clone()));
        assert!(!ini.state().active && ini.state().options.iter().all(|o| !o.enabled), "a new user starts Off with nothing checked");
        ini.set(0, true);
        ini.set(19, true);
        assert_eq!(fs::read_to_string(&path).unwrap(), GAME, "checked options wait for the switch");
        ini.set_active(true);
        let applied = fs::read_to_string(&path).unwrap();
        assert!(applied.starts_with(GAME), "game lines must be untouched");
        assert!(applied.ends_with("[SystemSettings]\r\nt.MaxFPS=0\r\nr.MotionBlurQuality=0\r\n; RAPO END\r\n"));
        ini.set(0, false);
        assert!(!fs::read_to_string(&path).unwrap().contains("t.MaxFPS"), "unchecking while On removes the line");
        ini.set_active(false);
        assert_eq!(fs::read_to_string(&path).unwrap(), GAME, "Off restores the file");
        let state = ini.state();
        assert!(!state.applied && state.options[19].enabled, "Off keeps the saved selection");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn reflex_supersedes_frame_sync_options_without_unchecking_them() {
        let path = temp("reflex", GAME);
        let mut ini = EngineIni::at(Some(path.clone()));
        for i in [1, 3, 4] { ini.set(i, true); }
        ini.set_active(true);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("t.Streamline.Reflex.Enable=1") && !text.contains("r.GTSyncType") && !text.contains("r.OneFrameThreadLag"));
        assert!(ini.state().options[3].enabled && ini.state().options[3].superseded);
        ini.set(1, false);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("r.GTSyncType=1") && text.contains("r.OneFrameThreadLag=0"), "without Reflex they are written again");
        assert!(!ini.state().options[3].superseded);
        drop(ini);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_section_gets_block_last_and_keeps_user_lines() {
        let original = "[SystemSettings]\nr.VSync=1\n\n[Other]\na=1\n";
        let applied = apply_text(original, &["t.MaxFPS=120".into()]);
        assert_eq!(applied, "[SystemSettings]\nr.VSync=1\n; RAPO BEGIN - removido automaticamente ao fechar o RAPO\nt.MaxFPS=120\n; RAPO END\n\n[Other]\na=1\n");
        assert_eq!(remove_text(&applied, &[]).as_deref(), Some(original));
    }

    #[test]
    fn rewritten_file_without_markers_only_loses_lines_rapo_wrote() {
        let rewritten = "[SystemSettings]\nt.MaxFPS=120\nr.MotionBlurQuality=2\n[Other]\nt.MaxFPS=120\n";
        assert_eq!(remove_text(rewritten, &[]), None, "nothing written means nothing is ours");
        assert_eq!(remove_text(rewritten, &[("t.MaxFPS", "120"), ("r.MotionBlurQuality", "0")]).as_deref(),
            Some("[SystemSettings]\nr.MotionBlurQuality=2\n[Other]\nt.MaxFPS=120\n"));
    }

    #[test]
    fn copies_left_by_game_rewrites_are_removed_once_owned() {
        // The game rewrote the file after RAPO closed, three times: unmarked copies plus a fresh block.
        let rewritten = "[SystemSettings]\nt.MaxFPS=0\nt.MaxFPS=0\nr.VSync=1\n; RAPO BEGIN - removido automaticamente ao fechar o RAPO\nt.MaxFPS=0\n; RAPO END\n[Other]\nt.MaxFPS=0\n";
        assert_eq!(remove_text(rewritten, &[("t.MaxFPS", "0")]).as_deref(), Some("[SystemSettings]\nr.VSync=1\n[Other]\nt.MaxFPS=0\n"));
        let path = temp("owned", "[SystemSettings]\nt.MaxFPS=0\nt.MaxFPS=0\n");
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.owned.push("t.MaxFPS=0".into());
        ini.set(0, true);
        ini.set_active(true);
        assert_eq!(fs::read_to_string(&path).unwrap().matches("t.MaxFPS=0").count(), 1, "on writes exactly one copy");
        drop(ini);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[SystemSettings]\n");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn closing_removes_the_block_and_a_crashed_session_block_is_cleaned() {
        let original = format!("{GAME}[Core.Log]\r\nt.OverrideFPS=30\r\n");
        let path = temp("drop", &original);
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.set(1, true);
        ini.set_active(true);
        assert_eq!(ini.state().override_fps.as_deref(), Some("t.OverrideFPS=30"));
        let crashed = fs::read_to_string(&path).unwrap();
        drop(ini);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::write(&path, &crashed).unwrap();
        EngineIni::at(Some(path.clone())).sync(false);
        assert_eq!(fs::read_to_string(&path).unwrap(), original, "an Off session removes a block left by a crash");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn texture_pool_is_prefilled_and_editable_within_bounds() {
        assert_eq!(ENTRIES[POOL].0, "r.Streaming.PoolSize");
        let path = temp("pool", GAME);
        let mut ini = EngineIni::at(Some(path.clone()));
        let state = ini.state();
        assert_eq!((state.options[POOL].value.as_str(), state.options[POOL].editable, state.options[POOL].min, state.options[POOL].max), ("6144", true, 256, 32768));
        assert!(state.options.iter().enumerate().all(|(i, o)| o.editable == (i == POOL)), "only the texture pool is editable");
        ini.set(POOL, true);
        ini.set_active(true);
        assert!(fs::read_to_string(&path).unwrap().contains("r.Streaming.PoolSize=6144\r\n"), "the default is written until the user changes it");
        ini.set_value(POOL, 8192);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("r.Streaming.PoolSize=8192\r\n") && !text.contains("PoolSize=6144"), "the new value replaces the old one: {text}");
        assert_eq!(ini.state().options[POOL].value, "8192");
        for bad in [0, 255, 32769, u32::MAX] {
            ini.set_value(POOL, bad);
            assert_eq!(ini.state().options[POOL].value, "8192", "{bad} is refused and changes nothing");
            assert!(ini.state().error.contains("256 e 32768"));
        }
        ini.set_value(0, 5);
        assert_eq!(ini.state().options[0].value, "0", "a non-editable option ignores a value");
        drop(ini);
        assert_eq!(fs::read_to_string(&path).unwrap(), GAME, "closing removes the custom line too");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn an_older_custom_value_left_by_a_game_rewrite_is_still_removed() {
        // RAPO wrote 6144, then 8192; the game later rewrote the file keeping both unmarked, next to a user line.
        let rewritten = "[SystemSettings]\nr.Streaming.PoolSize=6144\nr.Streaming.PoolSize=8192\nr.Streaming.PoolSize=4096\n";
        let path = temp("oldvalue", rewritten);
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.owned = vec!["r.Streaming.PoolSize=6144".into(), "r.Streaming.PoolSize=8192".into()];
        ini.sync(false);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[SystemSettings]\nr.Streaming.PoolSize=4096\n", "only RAPO's exact lines go; the user's own value stays");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_values_and_old_owned_keys_parse_safely() {
        let mut custom = [None; ENTRIES.len()];
        custom[POOL] = Some(8192);
        assert_eq!(format_values(&custom), "r.Streaming.PoolSize=8192");
        assert_eq!(parse_values("r.Streaming.PoolSize=8192"), custom);
        for bad in ["r.Streaming.PoolSize=10", "r.Streaming.PoolSize=x", "r.Streaming.PoolSize=99999", "t.MaxFPS=5", "nokey=1", "garbage"] {
            assert_eq!(parse_values(bad), [None; ENTRIES.len()], "{bad} is ignored");
        }
        assert_eq!(parse_owned("t.MaxFPS;r.Streaming.PoolSize=8192;unknown.key=1;r.Streaming.PoolSize=8192"),
            vec!["t.MaxFPS=0".to_string(), "r.Streaming.PoolSize=8192".to_string()], "bare keys get their default value; unknown keys and duplicates are dropped");
    }
}
