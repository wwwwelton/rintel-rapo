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

    // The proxy loads the kernel driver from %SystemRoot%\System32\Drivers itself; only the proxy is bundled.
    static PROXY_EXE: &[u8] = include_bytes!("../vendor/wfp/mudwfp_proxy.exe");

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
            let base = std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| io::Error::other("LOCALAPPDATA ausente"))?;
            let dir = PathBuf::from(base).join("Rintel RAPO").join("wfp");
            fs::create_dir_all(&dir)?;
            Ok(dir)
        }

        fn ensure_files(dir: &Path) -> io::Result<PathBuf> {
            let exe_path = dir.join("mudwfp_proxy.exe");
            if !exe_path.exists() || fs::metadata(&exe_path)?.len() != PROXY_EXE.len() as u64 {
                fs::write(&exe_path, PROXY_EXE)?;
            }
            Ok(exe_path)
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

            let exe_path = match Self::ensure_files(&dir) {
                Ok(p) => p,
                Err(e) => {
                    self.error = format!("Extração WFP: {e}");
                    return Err(e);
                }
            };

            // Process names are matched case-insensitively by the proxy, so one rule covers AION2.exe/Aion2.exe.
            let args = ["-r".to_string(), "-S".to_string(), "-b".to_string(), format!("{proc_name}|B|127.0.0.1:{port}")];
            match Command::new(&exe_path)
                .current_dir(&dir)
                .args(args)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                Ok(child) => {
                    if self.job != 0 {
                        use std::os::windows::io::AsRawHandle;
                        unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.job as _, child.as_raw_handle() as _); }
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
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            Self::restart_driver();
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
            State {
                enabled: self.enabled,
                running: self.child.is_some(),
                target: self.target.clone(),
                error: self.error.clone(),
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
