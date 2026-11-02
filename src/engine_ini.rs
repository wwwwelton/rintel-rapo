//! Opt-in AION 2 Engine.ini overrides. Every option starts off; only the options the user enables are
//! written, inside a marked block that RAPO removes when the last option is unchecked or RAPO closes.
//! Lines outside the block keep their bytes.

use std::{fs, io, path::{Path, PathBuf}};

/// AION 2 on RTX 5070 12 GB at 4K, aimed at input latency and FPS: `(key, value, label)`.
/// Reflex low-latency keeps the render queue empty; `r.GTSyncType=1` and `r.OneFrameThreadLag=0` stop the
/// game/render threads from running frames ahead (Epic "Low Latency Frame Syncing"); a 120 FPS cap below
/// the GPU limit avoids GPU-bound queuing; motion blur/depth of field are full-screen 4K passes.
/// Resolution, upscaler (DLSS/FSR), VSync and texture pool stay under the game's own menu.
/// The last three are cosmetic full-screen passes that cost GPU time and add no gameplay information.
pub const ENTRIES: [(&str, &str, &str); 10] = [
    ("t.MaxFPS", "120", "Limitar FPS em 120 (evita fila na GPU e reduz latência)"),
    ("t.Streamline.Reflex.Enable", "1", "Ativar NVIDIA Reflex"),
    ("t.Streamline.Reflex.Mode", "1", "Reflex em modo baixa latência (fila de renderização vazia)"),
    ("r.GTSyncType", "1", "Sem fila de frames: game thread espera a apresentação (RHI)"),
    ("r.OneFrameThreadLag", "0", "Sem frame extra da render thread (menos latência; pode reduzir FPS)"),
    ("r.MotionBlurQuality", "0", "Desligar motion blur (mais FPS; muda o visual)"),
    ("r.DepthOfFieldQuality", "0", "Desligar profundidade de campo (mais FPS; muda o visual)"),
    ("r.SceneColorFringeQuality", "0", "Desligar aberração cromática (imagem mais nítida; muda o visual)"),
    ("r.FilmGrain", "0", "Desligar granulação de filme (imagem mais limpa; muda o visual)"),
    ("r.LensFlareQuality", "0", "Desligar lens flare (menos brilho na tela; muda o visual)"),
];
const BEGIN: &str = "; RAPO BEGIN - removido automaticamente ao fechar o RAPO";
const END: &str = "; RAPO END";
const SECTION: &str = "[SystemSettings]";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Option_ {
    pub key: String,
    pub value: String,
    pub label: String,
    pub enabled: bool,
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
    /// Every option RAPO has ever written, saved across sessions. The game rewrites Engine.ini on exit
    /// without RAPO's markers, so copies of these exact lines are removed even outside the block.
    owned: [bool; ENTRIES.len()],
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
        if let Ok(Some((selected, active, owned))) = loaded { ini.selected = selected; ini.active = active; ini.owned = owned; }
        ini.sync(ini.active);
        if let Err(error) = loaded { ini.error = format!("Perfil do Engine.ini: {error}"); }
        ini
    }

    pub fn at(path: Option<PathBuf>) -> Self {
        Self { path, selected: [false; ENTRIES.len()], active: false, written: [false; ENTRIES.len()], owned: [false; ENTRIES.len()], persist: false, override_fps: None, error: String::new() }
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

    fn save(&mut self) {
        if !self.persist { return; }
        if let Err(error) = save_profile(&self.selected, self.active, &self.owned) { self.error = format!("Salvar perfil do Engine.ini: {error}"); }
    }

    /// Makes the file hold `selected` when `on`, and no RAPO line otherwise.
    fn sync(&mut self, on: bool) {
        let Some(path) = self.path.clone() else { return };
        let wanted: [bool; ENTRIES.len()] = std::array::from_fn(|i| on && self.selected[i]);
        let any = wanted.contains(&true);
        let lines: Vec<String> = ENTRIES.iter().zip(wanted).filter(|(_, w)| *w).map(|((k, v, _), _)| format!("{k}={v}")).collect();
        let owned: [bool; ENTRIES.len()] = std::array::from_fn(|i| self.written[i] || self.owned[i]);
        let written: Vec<(&str, &str)> = ENTRIES.iter().zip(owned).filter(|(_, w)| *w).map(|((k, v, _), _)| (*k, *v)).collect();
        let result = (|| {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound && !any => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("Engine.ini não encontrado; abra o AION 2 uma vez para criá-lo".into()),
                Err(error) => return Err(format!("Ler Engine.ini: {error}")),
            };
            let base = remove_text(&text, &written);
            let changed = if any { Some(apply_text(base.as_deref().unwrap_or(&text), &lines)) } else { base };
            let changed = changed.filter(|new| *new != text);
            if let Some(new) = &changed { write_replacing(&path, new).map_err(|error| format!("Gravar Engine.ini: {error}"))?; }
            Ok(Some(changed.unwrap_or(text)))
        })();
        match result {
            Ok(text) => {
                self.written = if text.is_some() { wanted } else { [false; ENTRIES.len()] };
                let owned: [bool; ENTRIES.len()] = std::array::from_fn(|i| self.owned[i] || self.written[i]);
                if owned != self.owned { self.owned = owned; self.save(); }
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
            options: ENTRIES.iter().zip(self.selected).map(|((key, value, label), enabled)| Option_ { key: key.to_string(), value: value.to_string(), label: label.to_string(), enabled }).collect(),
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

fn keys(flags: &Flags) -> String {
    ENTRIES.iter().zip(flags).filter(|(_, on)| **on).map(|((k, _, _), _)| *k).collect::<Vec<_>>().join(";")
}

fn flags(keys: &str) -> Flags {
    let keys: Vec<&str> = keys.split(';').collect();
    std::array::from_fn(|i| keys.contains(&ENTRIES[i].0))
}

/// Profile format: `Active` DWORD, `Options` = enabled keys and `Owned` = keys RAPO ever wrote, joined by `;`.
/// Unknown keys are ignored.
#[cfg(windows)]
fn load_profile() -> Result<Option<(Flags, bool, Flags)>, String> {
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
    Ok(Some((flags(&options), active != 0, flags(&owned))))
}

#[cfg(windows)]
fn save_profile(selected: &Flags, active: bool, owned: &Flags) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(PROFILE_KEY).map_err(|e| e.to_string())?;
    key.set_value("Options", &keys(selected)).map_err(|e| e.to_string())?;
    key.set_value("Owned", &keys(owned)).map_err(|e| e.to_string())?;
    key.set_value("Active", &u32::from(active)).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn load_profile() -> Result<Option<(Flags, bool, Flags)>, String> { Ok(None) }

#[cfg(not(windows))]
fn save_profile(_: &Flags, _: bool, _: &Flags) -> Result<(), String> { Ok(()) }

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
        ini.set(5, true);
        assert_eq!(fs::read_to_string(&path).unwrap(), GAME, "checked options wait for the switch");
        ini.set_active(true);
        let applied = fs::read_to_string(&path).unwrap();
        assert!(applied.starts_with(GAME), "game lines must be untouched");
        assert!(applied.ends_with("[SystemSettings]\r\nt.MaxFPS=120\r\nr.MotionBlurQuality=0\r\n; RAPO END\r\n"));
        ini.set(0, false);
        assert!(!fs::read_to_string(&path).unwrap().contains("t.MaxFPS"), "unchecking while On removes the line");
        ini.set_active(false);
        assert_eq!(fs::read_to_string(&path).unwrap(), GAME, "Off restores the file");
        let state = ini.state();
        assert!(!state.applied && state.options[5].enabled, "Off keeps the saved selection");
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
        let rewritten = "[SystemSettings]\nt.MaxFPS=120\nt.MaxFPS=120\nr.VSync=1\n; RAPO BEGIN - removido automaticamente ao fechar o RAPO\nt.MaxFPS=120\n; RAPO END\n[Other]\nt.MaxFPS=120\n";
        assert_eq!(remove_text(rewritten, &[("t.MaxFPS", "120")]).as_deref(), Some("[SystemSettings]\nr.VSync=1\n[Other]\nt.MaxFPS=120\n"));
        let path = temp("owned", "[SystemSettings]\nt.MaxFPS=120\nt.MaxFPS=120\n");
        let mut ini = EngineIni::at(Some(path.clone()));
        ini.owned[0] = true;
        ini.set(0, true);
        ini.set_active(true);
        assert_eq!(fs::read_to_string(&path).unwrap().matches("t.MaxFPS=120").count(), 1, "on writes exactly one copy");
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
}
