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
    // SAFETY: both pointers/lengths describe live Vec<u16> buffers.
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

pub(crate) fn process_created(handle: HANDLE) -> Result<u64, String> {
    // SAFETY: FILETIME is plain integers valid when zeroed; the handle is caller-owned and the out-pointers are valid locals.
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
    // SAFETY: the handle is null-checked, only borrowed by the calls below, and closed before leaving the block.
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
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
    let created = process_created(unsafe { GetCurrentProcess() })?;
    let parameters: Vec<u16> = format!("--wait-parent {} --wait-parent-created {created}", std::process::id())
        .encode_utf16().chain([0]).collect();
    let path: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let runas = [b'r' as u16, b'u' as u16, b'n' as u16, b'a' as u16, b's' as u16, 0];
    // SAFETY: every string is NUL-terminated UTF-16 outliving the call; null owner/directory are allowed.
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
    // SAFETY: `buffer` is valid for `length` UTF-16 units and `length` is updated by the call to the number written (<= capacity).
    unsafe {
        let mut buffer = [0u16; 32_768];
        let mut length = buffer.len() as u32;
        (QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) != 0)
            .then(|| PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize])))
    }
}

fn process_path(pid: u32) -> Option<PathBuf> {
    if pid <= 4 { return None; }
    // SAFETY: the handle is null-checked, borrowed by `process_path_handle`, and closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() { return None; }
        let path = process_path_handle(handle);
        CloseHandle(handle);
        path
    }
}

/// Image paths of processes seen in the previous snapshot, so each process is opened once instead of
/// every second. Keyed by PID plus image name: a PID reused between snapshots by another program misses.
type PathKey = (u32, Vec<u16>);
static PATH_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<PathKey, Option<PathBuf>>>> = std::sync::LazyLock::new(Default::default);

fn path_key(entry: &PROCESSENTRY32W) -> PathKey {
    let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
    (entry.th32ProcessID, entry.szExeFile[..len].to_vec())
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
    // SAFETY: the snapshot handle is checked and closed on every path; PROCESSENTRY32W is plain data valid when zeroed and `dwSize` is set before the first call.
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
        let mut cache = PATH_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut seen = HashMap::with_capacity(cache.len());
        loop {
            let key = path_key(&entry);
            let path = match cache.remove(&key) {
                Some(path) => path,
                None => process_path(entry.th32ProcessID),
            };
            let path = seen.entry(key).or_insert(path).clone();
            if let Some(exe) = path {
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
        // Exited processes drop out; only processes still in this snapshot stay cached.
        *cache = seen;
        drop(cache);
        // Sort by thread count descending so the main 3D game engine process is always first
        found.sort_by_key(|(threads, _)| std::cmp::Reverse(*threads));
        Ok(found.into_iter().map(|(_, r)| r).collect())
    }
}

/// Adds same-image children of a verified game process only for read-only telemetry.
pub(crate) fn telemetry_pids(running: &[Running], target: &Running) -> Vec<u32> {
    let mut pids: Vec<u32> = running.iter().filter(|p| same_executable(&p.directory, &target.directory)).map(|p| p.pid).collect();
    let Some(name) = target.exe.file_name() else { return pids };
    let expected: Vec<u16> = name.encode_wide().collect();
    #[link(name = "kernel32")]
    extern "system" {
        fn CompareStringOrdinal(a: *const u16, a_len: i32, b: *const u16, b_len: i32, ignore_case: i32) -> i32;
    }
    // SAFETY: the snapshot handle is checked and closed below; PROCESSENTRY32W is plain data valid when zeroed; the UTF-16 slices passed to CompareStringOrdinal are valid for their stated lengths.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE { return pids; }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let parent = running.iter().any(|p| p.pid == entry.th32ParentProcessID && same_executable(&p.exe, &target.exe));
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                if parent && !pids.contains(&entry.th32ProcessID)
                    && CompareStringOrdinal(entry.szExeFile.as_ptr(), len as i32, expected.as_ptr(), expected.len() as i32, 1) == 2
                    && process_path(entry.th32ProcessID).is_none_or(|path| same_executable(&path, &target.exe))
                {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 { break; }
            }
        }
        CloseHandle(snapshot);
    }
    pids
}

/// One `(pid, exe)` per distinct executable of the game that owns `pid`. Games such as AION2 run several
/// executables (`AION2.exe` twice, `EpicWebHelper.exe`) whose pids change on every launch, so exempting
/// "the" pid would miss siblings. The target pid wins for its own executable; empty if `pid` is gone.
pub fn game_executables(running: &[Running], pid: u32) -> Vec<(u32, PathBuf)> {
    let Some(target) = running.iter().find(|p| p.pid == pid) else { return Vec::new() };
    let mut found: Vec<(u32, PathBuf)> = vec![(target.pid, target.exe.clone())];
    for process in running.iter().filter(|p| same_executable(&p.directory, &target.directory)) {
        if !found.iter().any(|(_, exe)| same_executable(exe, &process.exe)) { found.push((process.pid, process.exe.clone())); }
    }
    found
}

pub fn aion2_running() -> Result<bool, String> {
    aion2_pids().map(|pids| !pids.is_empty())
}

pub fn aion2_pids() -> Result<Vec<u32>, String> {
    // SAFETY: the snapshot handle is checked and closed on every path; PROCESSENTRY32W is plain data valid when zeroed and `dwSize` is set before the first call.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!("Não foi possível enumerar processos: {}", std::io::Error::last_os_error()));
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut pids = Vec::new();
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if is_aion2_image(&entry.szExeFile) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        Ok(pids)
    }
}

/// True while the foreground window belongs to an `AION2.exe` process.
pub fn aion2_foreground() -> bool {
    let mut pid = 0;
    // SAFETY: both calls take a plain window handle or a valid `&mut u32` out-parameter; a null window yields pid 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    process_path(pid).and_then(|exe| exe.file_name().map(|name| name.eq_ignore_ascii_case("aion2.exe"))).unwrap_or(false)
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
        // SAFETY: the handle is null-checked, borrowed only by the calls below, and closed on every path.
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
        // SAFETY: handles are null-checked, each is closed on every path, and the out-pointers are valid locals.
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
            // SAFETY: the handle is null-checked, only borrowed by the calls below, and closed afterwards.
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
    fn telemetry_includes_direct_child_with_same_image() {
        let exe = std::env::current_exe().unwrap();
        let target = Running { pid: std::process::id(), game: "probe".into(), directory: exe.parent().unwrap().to_path_buf(), exe: exe.clone() };
        let mut child = std::process::Command::new(exe).args(["--ignored", "--exact", "windows::tests::telemetry_child_wait"])
            .spawn().unwrap();
        let child_pid = child.id();
        let found = telemetry_pids(std::slice::from_ref(&target), &target).contains(&child_pid);
        let unrelated = Running { pid: 0, ..target.clone() };
        let wrongly_included = telemetry_pids(std::slice::from_ref(&unrelated), &unrelated).contains(&child_pid);
        let _ = child.kill();
        let _ = child.wait();
        assert!(found, "a direct child with the same executable must contribute telemetry");
        assert!(!wrongly_included, "same-name processes without the verified parent must not be attributed");
    }

    #[test]
    #[ignore = "helper process for telemetry_includes_direct_child_with_same_image"]
    fn telemetry_child_wait() { std::thread::sleep(std::time::Duration::from_secs(5)); }
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

    #[test]
    fn game_executables_lists_each_distinct_exe_once_and_keeps_the_target_pid() {
        let process = |pid, exe: &str| Running { pid, game: "G".into(), directory: PathBuf::from(r"C:\S\common\G"), exe: PathBuf::from(exe) };
        let other = Running { pid: 99, game: "H".into(), directory: PathBuf::from(r"C:\S\common\H"), exe: PathBuf::from(r"C:\S\common\H\h.exe") };
        // The launcher (10) and the real game (11) share AION2.exe; the helper runs under another exe.
        let running = vec![process(10, r"C:\S\common\G\AION2.exe"), process(11, r"c:\s\COMMON\g\aion2.EXE"), process(12, r"C:\S\common\G\Engine\EpicWebHelper.exe"), other];
        let listed = game_executables(&running, 11);
        assert_eq!(listed.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [11, 12], "one entry per exe; the target pid represents AION2.exe: {listed:?}");
        assert!(listed.iter().all(|(_, exe)| !exe.ends_with("h.exe")), "another game's executables are never exempted");
        assert_eq!(game_executables(&running, 12).iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [12, 10]);
        assert!(game_executables(&running, 4242).is_empty(), "a vanished target exempts nothing");
    }
}
