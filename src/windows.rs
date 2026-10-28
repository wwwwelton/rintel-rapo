use crate::steam::Game;
use std::{
    collections::HashMap,
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SW_SHOWNORMAL};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetCurrentProcess, GetPriorityClass, GetProcessTimes, OpenProcess,
            QueryFullProcessImageNameW, SetPriorityClass, WaitForSingleObject,
            ABOVE_NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SET_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[derive(Clone)]
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

fn path_component_eq(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn CompareStringOrdinal(a: *const u16, a_len: i32, b: *const u16, b_len: i32, ignore_case: i32) -> i32;
    }
    let a: Vec<u16> = a.encode_wide().collect();
    let b: Vec<u16> = b.encode_wide().collect();
    unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) == 2 }
}

fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    match (a, b) {
        (Component::Prefix(a), Component::Prefix(b)) => match (a.kind(), b.kind()) {
            (Prefix::Disk(a) | Prefix::VerbatimDisk(a), Prefix::Disk(b) | Prefix::VerbatimDisk(b)) => a.eq_ignore_ascii_case(&b),
            (Prefix::UNC(a, x) | Prefix::VerbatimUNC(a, x), Prefix::UNC(b, y) | Prefix::VerbatimUNC(b, y)) => {
                path_component_eq(a, b) && path_component_eq(x, y)
            }
            _ => path_component_eq(a.as_os_str(), b.as_os_str()),
        },
        (Component::Normal(a), Component::Normal(b)) => path_component_eq(a, b),
        _ => a == b,
    }
}

/// Lexical Windows path identity, including case and extended-length drive/UNC prefixes.
pub fn same_executable(a: &Path, b: &Path) -> bool {
    let mut a = a.components();
    let mut b = b.components();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if component_eq(a, b) => (),
            _ => return false,
        }
    }
}

/// Matches directory components, never a similarly named sibling directory.
pub fn path_in_directory(exe: &Path, directory: &Path) -> bool {
    let mut exe = exe.components();
    let mut directory = directory.components().peekable();
    if directory.peek().is_none() {
        return false;
    }
    for part in directory {
        if !exe.next().is_some_and(|component| component_eq(component, part)) {
            return false;
        }
    }
    exe.next().is_some()
}

fn process_created(handle: HANDLE) -> Result<u64, String> {
    unsafe {
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = std::mem::zeroed();
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return Err(format!("Não foi possível verificar o processo: {}", std::io::Error::last_os_error()));
        }
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

fn parent_wait_args(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<Option<(u32, u64)>, String> {
    let (mut pid, mut created) = (None, None);
    while let Some(arg) = args.next() {
        if arg == "--wait-parent" {
            if pid.is_some() { return Err("--wait-parent duplicado".into()); }
            pid = Some(args.next().and_then(|value| value.to_str()?.parse::<u32>().ok())
                .filter(|pid| *pid > 4 && *pid != std::process::id())
                .ok_or("PID do processo anterior inválido")?);
        } else if arg == "--wait-parent-created" {
            if created.is_some() { return Err("--wait-parent-created duplicado".into()); }
            created = Some(args.next().and_then(|value| value.to_str()?.parse::<u64>().ok())
                .ok_or("Identidade do processo anterior inválida")?);
        }
    }
    let Some(pid) = pid else {
        return if created.is_none() { Ok(None) } else { Err("Falta --wait-parent".into()) };
    };
    Ok(Some((pid, created.ok_or("Falta a identidade do processo anterior")?)))
}

/// Called before creating App: a failed/expired wait must abort startup.
pub fn wait_for_parent() -> Result<(), String> {
    let Some((pid, created)) = parent_wait_args(std::env::args_os().skip(1))? else { return Ok(()); };
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            // An exited parent needs no wait; access failures must not permit overlapping Apps.
            return if error.raw_os_error() == Some(87) { Ok(()) } else { Err(format!("Falha ao abrir processo anterior: {error}")) };
        }
        let result = (|| {
            if WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 { return Ok(()); }
            if process_created(handle)? != created
                || !process_path_handle(handle).is_some_and(|path| same_executable(&path, &executable))
                || created >= process_created(GetCurrentProcess())?
            {
                return Err("O processo anterior não corresponde a esta instância do RAPO".into());
            }
            match WaitForSingleObject(handle, 30_000) {
                WAIT_OBJECT_0 => Ok(()),
                WAIT_TIMEOUT => Err("O RAPO anterior ainda não terminou; a nova instância não foi iniciada".into()),
                _ => Err(format!("Falha ao aguardar processo anterior: {}", std::io::Error::last_os_error())),
            }
        })();
        CloseHandle(handle);
        result
    }
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
    let created = process_created(unsafe { GetCurrentProcess() })?;
    let parameters: Vec<u16> = format!("--wait-parent {} --wait-parent-created {created}", std::process::id())
        .encode_utf16().chain([0]).collect();
    let path: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let runas = [b'r' as u16, b'u' as u16, b'n' as u16, b'a' as u16, b's' as u16, 0];
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            runas.as_ptr(),
            path.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err(format!("Não foi possível reabrir como administrador: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}
fn process_path_handle(handle: HANDLE) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    unsafe {
        let mut buffer = [0u16; 32_768];
        let mut length = buffer.len() as u32;
        (QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) != 0)
            .then(|| PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize])))
    }
}

fn process_path(pid: u32) -> Option<PathBuf> {
    if pid <= 4 { return None; }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() { return None; }
        let path = process_path_handle(handle);
        CloseHandle(handle);
        path
    }
}

fn game_for_path<'a>(exe: &Path, games: &'a [Game]) -> Option<&'a Game> {
    games.iter().find(|game| path_in_directory(exe, &game.directory))
}

fn is_aion2_image(image: &[u16]) -> bool {
    image
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| u8::try_from(c).unwrap_or_default().to_ascii_lowercase())
        .eq(b"aion2.exe".iter().copied())
}

pub fn running(games: &[Game]) -> Result<Vec<Running>, String> {
    if games.is_empty() {
        return Ok(Vec::new());
    }
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
        if Process32FirstW(snapshot, &mut entry) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(snapshot);
            return Err(format!("Não foi possível iniciar enumeração de processos: {error}"));
        }
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
        CloseHandle(snapshot);
        // Sort by thread count descending so the main 3D game engine process is always first
        found.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(found.into_iter().map(|(_, r)| r).collect())
    }
}

pub fn aion2_running() -> Result<bool, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!("Não foi possível enumerar processos: {}", std::io::Error::last_os_error()));
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if is_aion2_image(&entry.szExeFile) {
                    found = true;
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        Ok(found)
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
        if !process_path(pid).is_some_and(|exe| same_executable(&exe, &process.exe)) {
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
                if !same_executable(&managed.exe, &process.exe) {
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
                if !process_path(pid).is_some_and(|exe| same_executable(&exe, &managed.exe)) {
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
            if !managed.boosted || !process_path(pid).is_some_and(|exe| same_executable(&exe, &managed.exe)) {
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
            is_aion2: false,
        }];
        assert!(game_for_path(Path::new(r"C:\Steam\common\Game\bin\game.exe"), &games).is_some());
        assert!(game_for_path(Path::new(r"C:\Steam\common\Game 2\game.exe"), &games).is_none());
        assert!(game_for_path(Path::new(r"c:\STEAM\COMMON\gAME\game.exe"), &games).is_some());
        assert!(!path_in_directory(Path::new(r"C:\Steam\common\Game"), &games[0].directory));
        assert!(path_in_directory(Path::new(r"c:/steam/common/game/bin/game.exe"), Path::new(r"C:\Steam\common\Game\")));
        assert!(!path_in_directory(Path::new(r"C:\Steam\common\Game.exe"), &games[0].directory));
        assert!(path_in_directory(Path::new(r"C:\JOGOS\ÄVENTURA\game.exe"), Path::new(r"c:\jogos\äventura")));
    }

    #[test]
    fn executable_identity_ignores_windows_case_not_file_or_directory() {
        assert!(same_executable(Path::new(r"C:\Games\Game.EXE"), Path::new(r"c:\games\game.exe")));
        assert!(same_executable(Path::new(r"\\?\C:\Games\Game.exe"), Path::new(r"c:\games\game.exe")));
        assert!(same_executable(Path::new(r"\\?\UNC\HOST\share\Game.exe"), Path::new(r"\\host\SHARE\game.exe")));
        assert!(!same_executable(Path::new(r"C:\Games\Game.exe"), Path::new(r"C:\Games2\Game.exe")));
        assert!(!same_executable(Path::new(r"C:\Games\Game.exe"), Path::new(r"C:\Games\Game2.exe")));
    }

    #[test]
    fn parent_wait_requires_complete_unambiguous_identity() {
        let parse = |args: &[&str]| parent_wait_args(args.iter().map(|arg| std::ffi::OsString::from(*arg)));
        assert_eq!(parse(&[]).unwrap(), None);
        assert!(parse(&["--wait-parent", "0", "--wait-parent-created", "10"]).is_err());
        assert!(parse(&["--wait-parent", "10"]).is_err());
        assert!(parse(&["--wait-parent-created", "10"]).is_err());
        let pid = (std::process::id() + 1).max(5).to_string();
        assert_eq!(parse(&["--wait-parent", &pid, "--wait-parent-created", "10"]).unwrap(), Some((pid.parse().unwrap(), 10)));
        assert!(parse(&["--wait-parent", &pid, "--wait-parent", &pid, "--wait-parent-created", "10"]).is_err());
        assert!(parse(&["--wait-parent", &pid, "--wait-parent-created", "bad"]).is_err());
    }

    #[test]
    fn aion2_image_matches_only_game_executable() {
        assert!(is_aion2_image(&"Aion2.EXE\0".encode_utf16().collect::<Vec<_>>()));
        assert!(!is_aion2_image(&"AION2.exe.old\0".encode_utf16().collect::<Vec<_>>()));
    }
}
