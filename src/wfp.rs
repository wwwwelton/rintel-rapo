//! Transparent process-level WFP redirector bundled in Rintel RAPO.
//! Intercepts TCP/UDP traffic from a target game process and routes it into the local SOCKS5 relay.

use serde::Serialize;
use std::io;

#[derive(Default, Clone, Serialize)]
pub struct State {
    pub enabled: bool,
    pub running: bool,
    pub target: String,
    pub error: String,
}

#[cfg(any(windows, test))]
fn extract_bundle(dir: &std::path::Path) -> io::Result<std::path::PathBuf> {
    use std::fs;
    fs::create_dir_all(dir.join("DRIVERS"))?;
    for (name, bytes) in [
        ("mudwfp_proxy.exe", &include_bytes!("../vendor/wfp/mudwfp_proxy.exe")[..]),
        ("MUDWFP.sys", &include_bytes!("../vendor/wfp/MUDWFP.sys")[..]),
        ("DRIVERS/MUDWFP.sys", &include_bytes!("../vendor/wfp/MUDWFP.sys")[..]),
        ("mudwfp.inf", &include_bytes!("../vendor/wfp/mudwfp.inf")[..]),
    ] {
        let path = dir.join(name);
        match fs::read(&path) {
            Ok(existing) if existing == bytes => continue,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::write(path, bytes)?;
    }
    Ok(dir.join("mudwfp_proxy.exe"))
}

#[cfg(windows)]
pub use windows_impl::WfpManager;

#[cfg(not(windows))]
pub use mock_impl::WfpManager;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    const PREPARE_DRIVER: &str = include_str!("../scripts/prepare-wfp.ps1");
    fn system_dir() -> io::Result<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let mut path = [0u16; 260];
        let length = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                path.as_mut_ptr(),
                path.len() as u32,
            ) as usize
        };
        if length == 0 { return Err(io::Error::last_os_error()); }
        if length >= path.len() { return Err(io::Error::other("Caminho System32 excede o limite")); }
        Ok(PathBuf::from(std::ffi::OsString::from_wide(&path[..length])))
    }

    fn driver_running() -> io::Result<bool> {
        use windows_sys::Win32::System::Services::*;
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() { return Err(io::Error::last_os_error()); }
            let name = [77u16, 85, 68, 87, 70, 80, 0]; // MUDWFP
            let service = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
            if service.is_null() {
                let error = io::Error::last_os_error();
                CloseServiceHandle(scm);
                return Err(error);
            }
            let mut status: SERVICE_STATUS = std::mem::zeroed();
            let ok = QueryServiceStatus(service, &mut status);
            let error = if ok == 0 { Some(io::Error::last_os_error()) } else { None };
            CloseServiceHandle(service);
            CloseServiceHandle(scm);
            if let Some(error) = error { return Err(error); }
            Ok(status.dwCurrentState == SERVICE_RUNNING)
        }
    }

    /// Job that kills the proxy when RAPO dies for any reason (crash, kill, power loss of the UI).
    /// Without it an orphaned proxy keeps redirecting the game to a dead local port.
    fn kill_on_close_job() -> isize {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() { return 0; }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            job as isize
        }
    }

    pub struct WfpManager {
        child: Option<Child>,
        job: isize,
        target: String,
        enabled: bool,
        error: String,
    }

    impl Default for WfpManager {
        fn default() -> Self {
            Self {
                child: None,
                job: kill_on_close_job(),
                target: String::new(),
                enabled: false,
                error: String::new(),
            }
        }
    }

    impl WfpManager {
        pub fn new() -> Self {
            Self::default()
        }

        fn base_dir() -> io::Result<PathBuf> {
            // mudwfp_proxy.exe is a 32-bit WOW64 binary that creates `.mudwfp_proxy.vsm` in current_dir;
            // using System32 triggers WOW64 redirection to a missing SysWOW64 path (SHM_Init:543 error 0x3).
            let base = std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| io::Error::other("LOCALAPPDATA ausente"))?;
            let dir = PathBuf::from(base).join("Rintel RAPO").join("wfp");
            fs::create_dir_all(&dir)?;
            Ok(dir)
        }

        fn lock_verified_proxy(exe_path: &Path) -> io::Result<fs::File> {
            use std::io::Read;
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 0x00000001;
            let mut file = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(exe_path)?;
            let expected = &include_bytes!("../vendor/wfp/mudwfp_proxy.exe")[..];
            let mut actual = Vec::with_capacity(expected.len());
            file.read_to_end(&mut actual)?;
            if actual != expected {
                return Err(io::Error::other("mudwfp_proxy.exe extraído diverge do bundle verificado"));
            }
            Ok(file)
        }

        fn prepare_driver(dir: &Path) -> io::Result<()> {
            let powershell = system_dir()?.join("WindowsPowerShell").join("v1.0").join("powershell.exe");
            let output = Command::new(powershell)
                .args(["-NoProfile", "-NonInteractive", "-Command", PREPARE_DRIVER])
                .env("RAPO_WFP_DRIVER_SOURCE", dir.join("MUDWFP.sys"))
                .creation_flags(CREATE_NO_WINDOW)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "Preparação do driver MUDWFP falhou; instalação exige administrador: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            if !driver_running()? {
                return Err(io::Error::other("Driver MUDWFP não confirmou estado Running"));
            }
            Ok(())
        }

        pub fn set_enabled(&mut self, enabled: bool, target: &str, port: u16) {
            self.enabled = enabled;
            if !enabled {
                self.stop();
            } else if !target.is_empty() {
                let _ = self.start(target, port);
            }
        }

        pub fn start(&mut self, target_proc: &str, port: u16) -> io::Result<()> {
            self.stop();
            let proc_name = target_proc.trim();
            if proc_name.is_empty() {
                return Ok(());
            }

            let dir = match Self::base_dir() {
                Ok(d) => d,
                Err(e) => {
                    self.error = format!("Diretório WFP: {e}");
                    return Err(e);
                }
            };

            let exe_path = match extract_bundle(&dir) {
                Ok(p) => p,
                Err(e) => {
                    self.error = format!("Extração WFP: {e}");
                    return Err(e);
                }
            };
            let _proxy_lock = match Self::lock_verified_proxy(&exe_path) {
                Ok(lock) => lock,
                Err(e) => {
                    self.error = format!("Verificação do WFP Proxy: {e}");
                    return Err(e);
                }
            };
            if let Err(error) = Self::prepare_driver(&dir) {
                self.error = error.to_string();
                return Err(error);
            }

            // Process names are matched case-insensitively by the proxy, so one rule covers AION2.exe/Aion2.exe.
            let args = ["-r".to_string(), "-S".to_string(), "-b".to_string(), format!("{proc_name}|B|127.0.0.1:{port}")];
            match Command::new(&exe_path)
                .current_dir(&dir)
                .args(args)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                Ok(mut child) => {
                    if self.job != 0 {
                        use std::os::windows::io::AsRawHandle;
                        let ok = unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.job as _, child.as_raw_handle() as _) };
                        if ok == 0 {
                            let _ = child.kill();
                            self.error = format!("Job Object WFP recusado (RAPO dentro de outro job): {}", io::Error::last_os_error());
                            return Err(io::Error::other(self.error.clone()));
                        }
                    }
                    self.child = Some(child);
                    self.target = proc_name.to_string();
                    self.error.clear();
                    Ok(())
                }
                Err(e) => {
                    self.error = format!("Inicialização do WFP Proxy ({proc_name}): {e}");
                    Err(e)
                }
            }
        }

        pub fn stop(&mut self) {
            let had_child = if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                true
            } else { false };
            if had_child {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Self::restart_driver();
            }
            self.target.clear();
        }

        fn restart_driver() {
            let _ = Command::new("net")
                .args(["stop", "mudwfp"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = Command::new("net")
                .args(["start", "mudwfp"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            // Restaura rota normal e limpa cache de destino do Windows
            let _ = Command::new("netsh")
                .args(["interface", "ip", "delete", "destinationcache"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }

        pub fn poll(&mut self) {
            if let Some(child) = &mut self.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.child = None;
                        if !status.success() {
                            self.error = format!("WFP Proxy encerrou com status {status}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.child = None;
                        self.error = format!("WFP poll: {e}");
                    }
                }
            }
        }

        pub fn state(&self) -> State {
            let driver = if self.child.is_some() { driver_running() } else { Ok(false) };
            let error = match &driver {
                Err(error) => format!("Consulta do driver MUDWFP falhou: {error}"),
                Ok(false) if self.child.is_some() => "Driver MUDWFP parado; nenhum tunelamento confirmado".into(),
                _ => self.error.clone(),
            };
            State {
                enabled: self.enabled,
                running: self.child.is_some() && driver.unwrap_or(false),
                target: self.target.clone(),
                error,
            }
        }
    }

    impl Drop for WfpManager {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(not(windows))]
mod mock_impl {
    use super::*;

    #[derive(Default)]
    pub struct WfpManager {
        enabled: bool,
        target: String,
        error: String,
    }

    impl WfpManager {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn set_enabled(&mut self, enabled: bool, target: &str, _port: u16) {
            self.enabled = enabled;
            self.target = if enabled { target.to_string() } else { String::new() };
        }
        pub fn start(&mut self, target_proc: &str, _port: u16) -> io::Result<()> {
            self.target = target_proc.to_string();
            self.error.clear();
            Ok(())
        }
        pub fn stop(&mut self) {
            self.target.clear();
        }
        pub fn poll(&mut self) {}
        pub fn state(&self) -> State {
            State {
                enabled: self.enabled,
                running: self.enabled && !self.target.is_empty(),
                target: self.target.clone(),
                error: self.error.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_extracts_all_files_and_repairs_same_size_corruption() {
        let dir = std::env::temp_dir().join(format!("rapo-wfp-bundle-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let proxy = extract_bundle(&dir).unwrap();
        assert_eq!(std::fs::read(&proxy).unwrap(), include_bytes!("../vendor/wfp/mudwfp_proxy.exe"));
        assert_eq!(std::fs::read(dir.join("mudwfp.inf")).unwrap(), include_bytes!("../vendor/wfp/mudwfp.inf"));
        let driver = dir.join("MUDWFP.sys");
        let original = std::fs::read(&driver).unwrap();
        assert_eq!(original, include_bytes!("../vendor/wfp/MUDWFP.sys"));
        assert_eq!(std::fs::read(dir.join("DRIVERS/MUDWFP.sys")).unwrap(), original);
        let mut damaged = original.clone();
        damaged[0] ^= 0xff;
        std::fs::write(&driver, damaged).unwrap();
        extract_bundle(&dir).unwrap();
        assert_eq!(std::fs::read(&driver).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg_attr(windows, ignore = "Installs/starts a signed kernel driver; run explicitly with administrator consent")]
    fn wfp_state_lifecycle() {
        let mut wfp = WfpManager::new();
        assert!(!wfp.state().running);
        wfp.set_enabled(true, "Aion2.exe", 1080);
        assert_eq!(wfp.state().target, "Aion2.exe");
        wfp.set_enabled(false, "", 1080);
        assert!(!wfp.state().running);
        assert!(wfp.state().target.is_empty());
    }
}
