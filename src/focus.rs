//! AION2 focus mode. The MMCSS "Games" tweaks are applied when RAPO starts and held while it runs,
//! independent of the game; AION2's process priority is raised only while its window is focused. Everything
//! is undone when the mode is switched off, on exit and, via on-disk backups, after a crash. The power plan
//! is handled separately by `power.rs`.
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Write, path::{Path, PathBuf}};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{
        GetPriorityClass, OpenProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    },
};
use winreg::{
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ},
    RegKey,
};

const PROFILE: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile";
const GAMES: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile\Tasks\Games";

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
enum Val { Dword(u32), Sz(String) }

/// Original values, one slot per entry of `tweaks()` in the same order; `None` means the value did not
/// exist. Keys and names are never read from disk, so a tampered backup cannot redirect the writes.
type Saved = Vec<Option<Val>>;

/// MMCSS values applied for the whole RAPO session (Microsoft Learn, "Multimedia Class Scheduler Service").
/// SystemResponsiveness 10 is the lowest effective reserve: values below 10 are clamped back to the default 20.
/// NetworkThrottlingIndex 0xFFFFFFFF disables the multimedia network throttle. `Priority` is ignored for
/// Scheduling Category High and `GPU Priority`/`SFIO Priority` are documented as unused; they stay for parity
/// with the original AION2-Focus script and are harmless.
fn tweaks() -> Vec<(&'static str, &'static str, Val)> {
    let sz = |s: &str| Val::Sz(s.into());
    vec![
        (PROFILE, "SystemResponsiveness", Val::Dword(10)),
        (PROFILE, "NetworkThrottlingIndex", Val::Dword(u32::MAX)),
        (GAMES, "Scheduling Category", sz("High")),
        (GAMES, "SFIO Priority", sz("High")),
        (GAMES, "Background Only", sz("False")),
        (GAMES, "GPU Priority", Val::Dword(8)),
        (GAMES, "Priority", Val::Dword(2)),
    ]
}

/// Human-readable list of everything focus mode changes, for the UI info box.
pub fn settings() -> Vec<String> {
    let mut lines: Vec<String> = tweaks().into_iter().map(|(path, name, value)| {
        let value = match value { Val::Dword(u32::MAX) => "0xFFFFFFFF".into(), Val::Dword(v) => v.to_string(), Val::Sz(v) => v };
        format!("{} · {name} = {value}", if path == GAMES { "MMCSS Tasks\\Games" } else { "MMCSS SystemProfile" })
    }).collect();
    lines.push("Prioridade de AION2.exe · Alta somente enquanto em foco (apenas se estava Normal)".into());
    lines
}

fn data_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA ausente")?;
    Ok(PathBuf::from(base).join("Rintel RAPO"))
}
fn backup_path() -> Result<PathBuf, String> { data_dir().map(|dir| dir.join("mmcss-backup.json")) }
fn priority_path() -> Result<PathBuf, String> { data_dir().map(|dir| dir.join("priority-backup.json")) }

fn read(key: &RegKey, name: &str) -> Result<Option<Val>, String> {
    let raw = match key.get_raw_value(name) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{name}: {e}")),
    };
    let decoded = if raw.vtype == REG_DWORD { key.get_value(name).map(Val::Dword) }
        else if raw.vtype == REG_SZ { key.get_value(name).map(Val::Sz) }
        else { return Err(format!("{name}: tipo de registro {:?} não suportado", raw.vtype)) };
    decoded.map(Some).map_err(|e| format!("{name}: {e}"))
}

fn write(key: &RegKey, name: &str, value: &Val) -> std::io::Result<()> {
    match value {
        Val::Dword(v) => key.set_value(name, v),
        Val::Sz(v) => key.set_value(name, v),
    }
}

/// Best effort: every value is attempted; returns the first failure.
fn restore(saved: &Saved) -> Result<(), String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut first = None;
    for ((key_path, name, _), old) in tweaks().iter().zip(saved) {
        let result = hklm.open_subkey_with_flags(key_path, KEY_SET_VALUE).and_then(|key| match old {
            Some(old) => write(&key, name, old),
            None => match key.delete_value(name) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        });
        if let Err(e) = result { first.get_or_insert(format!("Restaurar {name}: {e}")); }
    }
    first.map_or(Ok(()), Err)
}

/// A backup is trusted only if it has one slot per tweak and each stored value has that tweak's type.
fn valid(saved: &Saved) -> bool {
    let specs = tweaks();
    saved.len() == specs.len() && saved.iter().zip(&specs).all(|(old, (_, _, want))| {
        old.as_ref().is_none_or(|old| std::mem::discriminant(old) == std::mem::discriminant(want))
    })
}

/// Pending backup left by a crashed session. An unreadable or invalid file is moved aside so it cannot
/// block future sessions, and reported.
fn load_backup() -> Result<Option<Saved>, String> {
    let path = backup_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    match serde_json::from_slice::<Saved>(&bytes) {
        Ok(saved) if valid(&saved) => Ok(Some(saved)),
        _ => {
            let aside = path.with_extension("json.invalid");
            let _ = std::fs::rename(&path, &aside);
            Err(format!("Backup MMCSS inválido; movido para {}", aside.display()))
        }
    }
}

/// Write-then-rename so a crash mid-write never leaves a truncated file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Records which processes this session raised (pid, creation time) so a crash cannot leave them High
/// unnoticed; an empty set removes the file.
fn persist_priorities(boosted: &HashMap<u32, u64>) -> Result<(), String> {
    let path = priority_path()?;
    if boosted.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    let entries: Vec<(u32, u64)> = boosted.iter().map(|(&pid, &created)| (pid, created)).collect();
    write_atomic(&path, &serde_json::to_vec(&entries).map_err(|e| e.to_string())?)
}

/// Processes a crashed session raised. Restoring only touches a process whose pid, creation time and
/// current High class still match, so a tampered file can at worst lower a High process to Normal.
fn load_priorities() -> HashMap<u32, u64> {
    let Ok(path) = priority_path() else { return HashMap::new() };
    std::fs::read(path).ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<(u32, u64)>>(&bytes).ok())
        .map(|entries| entries.into_iter().collect())
        .unwrap_or_default()
}

pub struct Focus {
    /// Originals of the MMCSS tweaks currently applied (or left by a crash and not yet restored).
    saved: Option<Saved>,
    /// pid -> process creation time, so a recycled pid is never touched.
    boosted: HashMap<u32, u64>,
    /// Failure to apply the tweaks this session; no retry until the mode is switched off and on again,
    /// so a failing write (for example without administrator rights) is not repeated every second.
    latched: Option<String>,
}

impl Focus {
    /// Rolls back what a previous crashed session left applied; failed rollbacks are retried on every update.
    pub fn new() -> (Self, Option<String>) {
        let mut focus = Self { saved: None, boosted: load_priorities(), latched: None };
        let mut errors = Vec::new();
        match load_backup() {
            Ok(Some(saved)) => { focus.saved = Some(saved); errors.extend(focus.revert().err()); }
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
        errors.extend(focus.unboost().err());
        (focus, if errors.is_empty() { None } else { Some(errors.join("; ")) })
    }

    pub fn active(&self) -> bool { self.saved.is_some() }

    /// `enabled`: MMCSS tweaks held for the session. `focused`: AION2 owns the foreground, so its
    /// processes (`pids`) run at High priority.
    pub fn update(&mut self, enabled: bool, focused: bool, pids: &[u32]) -> Result<(), String> {
        if !enabled {
            self.latched = None;
            let errors: Vec<String> = [self.unboost(), self.revert()].into_iter().filter_map(Result::err).collect();
            return if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) };
        }
        if let Some(error) = &self.latched { return Err(error.clone()); }
        if let Err(error) = self.apply() {
            self.latched = Some(error.clone());
            return Err(error);
        }
        if focused { self.boost(pids) } else { self.unboost() }
    }

    fn apply(&mut self) -> Result<(), String> {
        if self.saved.is_some() { return Ok(()); }
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let path = backup_path()?;
        std::fs::create_dir_all(path.parent().unwrap_or(&path)).map_err(|e| e.to_string())?;
        let specs = tweaks();
        let (mut keys, mut saved) = (Vec::new(), Vec::new());
        // Read every original first so nothing is written unless the whole backup exists on disk.
        for (key_path, name, _) in &specs {
            let (key, _) = hklm.create_subkey_with_flags(key_path, KEY_READ | KEY_SET_VALUE)
                .map_err(|e| format!("{key_path}: {e}"))?;
            saved.push(read(&key, name)?);
            keys.push(key);
        }
        write_atomic(&path, &serde_json::to_vec(&saved).map_err(|e| e.to_string())?)?;
        for ((_, name, value), key) in specs.iter().zip(&keys) {
            if let Err(e) = write(key, name, value) {
                return match restore(&saved) {
                    Ok(()) => { let _ = std::fs::remove_file(&path); Err(format!("{name}: {e}")) }
                    Err(rollback) => { self.saved = Some(saved); Err(format!("{name}: {e}; {rollback}")) }
                };
            }
        }
        self.saved = Some(saved);
        Ok(())
    }

    /// Keeps the backup (and `saved`) when the rollback fails, so the next update retries it.
    fn revert(&mut self) -> Result<(), String> {
        let Some(saved) = &self.saved else { return Ok(()) };
        restore(saved)?;
        self.saved = None;
        match std::fs::remove_file(backup_path()?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    /// Raises Normal-priority AION2 processes to High; anything already customised is left alone. Each is
    /// recorded on disk before it is raised.
    fn boost(&mut self, pids: &[u32]) -> Result<(), String> {
        let mut first = None;
        for &pid in pids {
            if self.boosted.contains_key(&pid) { continue; }
            // SAFETY: the handle is null-checked, only borrowed by the calls below, and closed before the iteration ends.
            unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid);
                if handle.is_null() {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(87) { first.get_or_insert(format!("PID {pid}: {error}")); }
                    continue;
                }
                if GetPriorityClass(handle) == NORMAL_PRIORITY_CLASS {
                    match crate::windows::process_created(handle) {
                        Ok(created) => {
                            self.boosted.insert(pid, created);
                            let raised = persist_priorities(&self.boosted).and_then(|()| {
                                if SetPriorityClass(handle, HIGH_PRIORITY_CLASS) == 0 {
                                    Err(format!("PID {pid}: {}", std::io::Error::last_os_error()))
                                } else {
                                    Ok(())
                                }
                            });
                            if let Err(e) = raised {
                                self.boosted.remove(&pid);
                                let _ = persist_priorities(&self.boosted);
                                first.get_or_insert(e);
                            }
                        }
                        Err(e) => { first.get_or_insert(e); }
                    }
                }
                CloseHandle(handle);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Entries stay until restored or the process is confirmed gone, so a transient failure is retried.
    fn unboost(&mut self) -> Result<(), String> {
        if self.boosted.is_empty() { return Ok(()); }
        let (mut first, before) = (None, self.boosted.len());
        self.boosted.retain(|&pid, &mut created| match restore_priority(pid, created) {
            Ok(()) => false,
            Err(e) => { first.get_or_insert(e); true }
        });
        if self.boosted.len() != before {
            if let Err(e) = persist_priorities(&self.boosted) { first.get_or_insert(e); }
        }
        first.map_or(Ok(()), Err)
    }
}

/// `Ok` once the process is back to Normal, was changed by someone else, exited, or its pid was recycled.
fn restore_priority(pid: u32, created: u64) -> Result<(), String> {
    // SAFETY: the handle is null-checked, only borrowed by the calls below, and closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid);
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(87) { Ok(()) } else { Err(format!("PID {pid}: {error}")) };
        }
        let result = if crate::windows::process_created(handle) == Ok(created) && GetPriorityClass(handle) == HIGH_PRIORITY_CLASS
            && SetPriorityClass(handle, NORMAL_PRIORITY_CLASS) == 0
        {
            Err(format!("PID {pid}: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        };
        CloseHandle(handle);
        result
    }
}

impl Drop for Focus {
    fn drop(&mut self) {
        let _ = self.update(false, false, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_is_trusted_only_with_one_slot_per_tweak_of_the_right_type() {
        let originals: Saved = tweaks().into_iter().map(|(_, _, value)| Some(value)).collect();
        assert!(valid(&originals));
        assert!(valid(&vec![None; tweaks().len()]), "absent values are valid originals");
        assert!(!valid(&originals[1..].to_vec()), "wrong length");
        let mut wrong_type = originals.clone();
        wrong_type[0] = Some(Val::Sz("10".into()));
        assert!(!valid(&wrong_type), "SystemResponsiveness is a DWORD");
    }
}
