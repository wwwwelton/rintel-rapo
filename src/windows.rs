use crate::steam::Game;
use std::{
    collections::HashMap,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SW_SHOWNORMAL};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetPriorityClass, OpenProcess, QueryFullProcessImageNameW, SetPriorityClass,
            ABOVE_NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SET_INFORMATION,
        },
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

pub struct Running {
    pub pid: u32,
    pub game: String,
    pub directory: PathBuf,
    pub exe: PathBuf,
}

pub fn steam_root() -> Option<PathBuf> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Valve\\Steam")
        .ok()?;
    let path: String = key.get_value("SteamPath").ok()?;
    Some(PathBuf::from(path))
}

pub fn relaunch_elevated() -> Result<(), String> {
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> *mut std::ffi::c_void;
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("Não foi possível localizar o executável: {error}"))?;
    let path: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let runas = [b'r' as u16, b'u' as u16, b'n' as u16, b'a' as u16, b's' as u16, 0];
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            runas.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err(format!("Não foi possível reabrir como administrador: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

fn process_path(pid: u32) -> Option<PathBuf> {
    if pid <= 4 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
    }
}

fn game_for_path<'a>(exe: &Path, games: &'a [Game]) -> Option<&'a Game> {
    let exe_str = exe.to_string_lossy();
    let exe_bytes = exe_str.as_bytes();
    games.iter().find(|game| {
        let dir_str = game.directory.to_string_lossy();
        let dir_bytes = dir_str.as_bytes();
        exe_bytes.len() > dir_bytes.len()
            && exe_bytes[..dir_bytes.len()].eq_ignore_ascii_case(dir_bytes)
            && matches!(exe_bytes[dir_bytes.len()], b'\\' | b'/')
    })
}

pub fn running(games: &[Game]) -> Result<Vec<Running>, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Não foi possível enumerar processos: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Vec::new();
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if let Some(exe) = process_path(entry.th32ProcessID) {
                    if let Some(game) = game_for_path(&exe, games) {
                        found.push((
                            entry.cntThreads,
                            Running {
                                pid: entry.th32ProcessID,
                                game: game.name.clone(),
                                directory: game.directory.clone(),
                                exe,
                            },
                        ));
                    }
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        // Sort by thread count descending so the main 3D game engine process is always first
        found.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(found.into_iter().map(|(_, r)| r).collect())
    }
}

struct Managed {
    exe: PathBuf,
    original: u32,
    boosted: bool,
}

pub struct Optimizer {
    originals: HashMap<u32, Managed>,
}

impl Optimizer {
    pub fn new() -> Self {
        Self {
            originals: HashMap::new(),
        }
    }
    pub fn enabled(&self, pid: u32) -> bool {
        self.originals.contains_key(&pid)
    }
    pub fn boosted(&self, pid: u32) -> bool {
        self.originals.get(&pid).is_some_and(|m| m.boosted)
    }

    pub fn toggle(&mut self, process: &Running) -> Result<(), String> {
        let pid = process.pid;
        if process_path(pid).as_deref() != Some(process.exe.as_path()) {
            return Err("O processo mudou; atualize a lista".into());
        }
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION,
                0,
                pid,
            );
            if handle.is_null() {
                return Err(format!(
                    "Sem acesso ao processo {pid}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let result = if let Some(managed) = self.originals.get(&pid) {
                if managed.exe != process.exe {
                    Err("O PID foi reutilizado; atualize a lista".into())
                } else if managed.boosted
                    && GetPriorityClass(handle) == ABOVE_NORMAL_PRIORITY_CLASS
                    && SetPriorityClass(handle, managed.original) == 0
                {
                    Err(format!(
                        "Falha ao restaurar: {}",
                        std::io::Error::last_os_error()
                    ))
                } else {
                    self.originals.remove(&pid);
                    Ok(())
                }
            } else {
                let original = GetPriorityClass(handle);
                if original == 0 {
                    Err(format!(
                        "Falha ao ler prioridade: {}",
                        std::io::Error::last_os_error()
                    ))
                } else if original != windows_sys::Win32::System::Threading::NORMAL_PRIORITY_CLASS {
                    Err("Prioridade já personalizada; nenhuma alteração aplicada".into())
                } else {
                    self.originals.insert(
                        pid,
                        Managed {
                            exe: process.exe.clone(),
                            original,
                            boosted: false,
                        },
                    );
                    Ok(())
                }
            };
            CloseHandle(handle);
            result
        }
    }

    pub fn update(&mut self, keep_windowed: bool) -> Vec<String> {
        let mut errors = Vec::new();
        unsafe {
            let hwnd = GetForegroundWindow();
            let mut foreground_pid = 0;
            GetWindowThreadProcessId(hwnd, &mut foreground_pid);
            if foreground_pid == 0 && !hwnd.is_null() {
                let root = windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOTOWNER,
                );
                if !root.is_null() {
                    GetWindowThreadProcessId(root, &mut foreground_pid);
                }
            }
            self.originals.retain(|&pid, managed| {
                if process_path(pid).as_deref() != Some(managed.exe.as_path()) {
                    return false;
                }
                let should_boost = pid == foreground_pid || keep_windowed;
                if should_boost == managed.boosted {
                    return true;
                }
                let handle = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION,
                    0,
                    pid,
                );
                if handle.is_null() {
                    errors.push(format!("PID {pid}: {}", std::io::Error::last_os_error()));
                    return true;
                }
                let current = GetPriorityClass(handle);
                if current == 0 {
                    CloseHandle(handle);
                    return true;
                }
                let target = if should_boost {
                    ABOVE_NORMAL_PRIORITY_CLASS
                } else {
                    managed.original
                };
                if current != target {
                    if SetPriorityClass(handle, target) != 0 {
                        managed.boosted = should_boost;
                    } else {
                        errors.push(format!("PID {pid}: {}", std::io::Error::last_os_error()));
                    }
                } else {
                    managed.boosted = should_boost;
                }
                CloseHandle(handle);
                true
            });
        }
        errors
    }
}

impl Drop for Optimizer {
    fn drop(&mut self) {
        for (pid, managed) in self.originals.drain() {
            if !managed.boosted || process_path(pid).as_deref() != Some(managed.exe.as_path()) {
                continue;
            }
            unsafe {
                let handle = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION,
                    0,
                    pid,
                );
                if !handle.is_null() {
                    if GetPriorityClass(handle) == ABOVE_NORMAL_PRIORITY_CLASS {
                        SetPriorityClass(handle, managed.original);
                    }
                    CloseHandle(handle);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn game_match_respects_directory_boundary() {
        let games = vec![Game {
            name: "Game".into(),
            directory: PathBuf::from(r"C:\Steam\common\Game"),
        }];
        assert!(game_for_path(Path::new(r"C:\Steam\common\Game\bin\game.exe"), &games).is_some());
        assert!(game_for_path(Path::new(r"C:\Steam\common\Game 2\game.exe"), &games).is_none());
    }
}
