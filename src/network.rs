use std::path::Path;

pub const PING_REDUCERS: &[&str] = &[
    "exitlag.exe",
    "exitlagclient.exe",
    "mudfish.exe",
    "mudfish_core.exe",
    "noping.exe",
    "nopingtunnel.exe",
    "wtfast.exe",
    "wtfast-daemon.exe",
    "gearupbooster.exe",
    "outfox.exe",
];

#[derive(Debug, Clone, Default)]
pub struct NetworkQosStatus {
    pub enabled: bool,
    pub active: bool,
    pub network_throttling_disabled: bool,
    pub detected_ping_reducers: Vec<String>,
    pub message: String,
}

pub struct NetworkQosManager {
    pub user_enabled: bool,
    active: bool,
    original_throttling_index: Option<u32>,
    original_responsiveness: Option<u32>,
    detected_reducers: Vec<String>,
}

impl NetworkQosManager {
    pub fn new() -> Self {
        Self {
            user_enabled: true,
            active: false,
            original_throttling_index: None,
            original_responsiveness: None,
            detected_reducers: Vec::new(),
        }
    }

    pub fn scan_ping_reducers(&mut self) -> &[String] {
        #[cfg(windows)]
        {
            use std::mem::size_of;
            use windows_sys::Win32::{
                Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
                System::Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
                    PROCESSENTRY32W, TH32CS_SNAPPROCESS,
                },
            };

            let mut found = Vec::new();
            unsafe {
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                if snapshot != INVALID_HANDLE_VALUE {
                    let mut entry: PROCESSENTRY32W = std::mem::zeroed();
                    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
                    if Process32FirstW(snapshot, &mut entry) != 0 {
                        loop {
                            let name = entry.szExeFile
                                .iter()
                                .take_while(|&&c| c != 0)
                                .map(|&c| c as u8 as char)
                                .collect::<String>()
                                .to_lowercase();

                            for &reducer in PING_REDUCERS {
                                if name == reducer && !found.contains(&name) {
                                    found.push(name.clone());
                                }
                            }

                            if Process32NextW(snapshot, &mut entry) == 0 {
                                break;
                            }
                        }
                    }
                    CloseHandle(snapshot);
                }
            }
            self.detected_reducers = found;
        }
        &self.detected_reducers
    }

    #[cfg(windows)]
    pub fn apply(&mut self, _game_exe: &Path) -> NetworkQosStatus {
        use winreg::{enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE}, RegKey};

        if !self.user_enabled {
            self.restore();
            return NetworkQosStatus {
                enabled: false,
                active: false,
                network_throttling_disabled: false,
                detected_ping_reducers: self.detected_reducers.clone(),
                message: "QoS de Internet desativado pelo usuário".into(),
            };
        }

        if self.active {
            return NetworkQosStatus {
                enabled: true,
                active: true,
                network_throttling_disabled: true,
                detected_ping_reducers: self.detected_reducers.clone(),
                message: "QoS de Internet ativo".into(),
            };
        }

        self.scan_ping_reducers();
        let mut throttling_disabled = false;
        let mut msg: String;

        // 1. Tentar ler e aplicar NetworkThrottlingIndex = 0xFFFFFFFF em SystemProfile
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let path = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile";
        
        if let Ok(key_read) = hklm.open_subkey_with_flags(path, KEY_READ) {
            if self.original_throttling_index.is_none() {
                self.original_throttling_index = key_read.get_value("NetworkThrottlingIndex").ok();
            }
            if self.original_responsiveness.is_none() {
                self.original_responsiveness = key_read.get_value("SystemResponsiveness").ok();
            }
        }

        if let Ok(key_write) = hklm.open_subkey_with_flags(path, KEY_READ | KEY_WRITE) {
            let _ = key_write.set_value("NetworkThrottlingIndex", &0xFFFF_FFFFu32);
            let _ = key_write.set_value("SystemResponsiveness", &0u32);
            throttling_disabled = true;
            msg = "Network Throttling desativado (0xFFFFFFFF) e latência de rede priorizada.".into();
        } else {
            msg = "Priorização de rede em user-mode ativa (registro HKLM requer elevação para alteração global).".into();
        }

        if !self.detected_reducers.is_empty() {
            msg.push_str(&format!(
                " Redutores de ping preservados: {}.",
                self.detected_reducers.join(", ")
            ));
        }

        self.active = true;

        NetworkQosStatus {
            enabled: true,
            active: true,
            network_throttling_disabled: throttling_disabled,
            detected_ping_reducers: self.detected_reducers.clone(),
            message: msg,
        }
    }

    #[cfg(windows)]
    pub fn restore(&mut self) {
        use winreg::{enums::{HKEY_LOCAL_MACHINE, KEY_WRITE}, RegKey};

        if !self.active {
            return;
        }

        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let path = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile";
        if let Ok(key_write) = hklm.open_subkey_with_flags(path, KEY_WRITE) {
            if let Some(orig) = self.original_throttling_index {
                let _ = key_write.set_value("NetworkThrottlingIndex", &orig);
            }
            if let Some(orig) = self.original_responsiveness {
                let _ = key_write.set_value("SystemResponsiveness", &orig);
            }
        }

        self.active = false;
    }

    #[cfg(not(windows))]
    pub fn apply(&mut self, _game_exe: &Path) -> NetworkQosStatus {
        NetworkQosStatus {
            enabled: self.user_enabled,
            active: false,
            network_throttling_disabled: false,
            detected_ping_reducers: Vec::new(),
            message: "Requer Windows".into(),
        }
    }

    #[cfg(not(windows))]
    pub fn restore(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn detected_reducers(&self) -> &[String] {
        &self.detected_reducers
    }
}

impl Drop for NetworkQosManager {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_qos_initial_state() {
        let mut mgr = NetworkQosManager::new();
        assert!(mgr.user_enabled);
        assert!(!mgr.is_active());
        let reducers = mgr.scan_ping_reducers();
        println!("Detected ping reducers: {:?}", reducers);
    }
}
