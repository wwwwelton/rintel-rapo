#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntelDttStatus {
    NotInstalled,
    InstalledStopped,
    ActiveRunning,
}

impl std::fmt::Display for IntelDttStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "Inativo / Não Instalado"),
            Self::InstalledStopped => write!(f, "Instalado (Parado)"),
            Self::ActiveRunning => write!(f, "Ativo em Execução"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntelDttReport {
    pub status: IntelDttStatus,
    pub service_name: Option<String>,
    pub running_processes: Vec<String>,
    pub apo_config_path: Option<String>,
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn query_service_status(service_name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
        SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_STATUS,
    };

    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }

        let wide_name = to_wide(service_name);
        let service = OpenServiceW(scm, wide_name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            CloseServiceHandle(scm);
            return None;
        }

        let mut status: SERVICE_STATUS = std::mem::zeroed();
        let ok = QueryServiceStatus(service, &mut status);
        CloseServiceHandle(service);
        CloseServiceHandle(scm);

        if ok != 0 {
            Some(status.dwCurrentState)
        } else {
            None
        }
    }
}

#[cfg(windows)]
fn check_running_dtt_processes() -> Vec<String> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32First, Process32Next,
            PROCESSENTRY32, TH32CS_SNAPPROCESS,
        },
    };

    let mut found = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }

        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32>() as u32;

        if Process32First(snapshot, &mut entry) != 0 {
            loop {
                let name = entry.szExeFile
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as u8 as char)
                    .collect::<String>()
                    .to_lowercase();

                if (name == "ipfsvc.exe" || name == "ipf_helper.exe" || name == "dttservice.exe")
                    && !found.contains(&name)
                {
                    found.push(name);
                }

                if Process32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    found
}

#[cfg(windows)]
fn find_apo_config() -> Option<String> {
    let win_dir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let search_root = std::path::Path::new(&win_dir).join("System32\\DriverStore\\FileRepository");

    if let Ok(entries) = std::fs::read_dir(search_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(folder_name) = path.file_name().and_then(|n| n.to_str()) {
                if folder_name.to_lowercase().starts_with("dtt_") {
                    let config = path.join("apo.config");
                    if config.exists() {
                        return Some(config.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(windows)]
pub fn detect_intel_dtt() -> IntelDttReport {
    use windows_sys::Win32::System::Services::SERVICE_RUNNING;

    let candidate_services = ["ipfsvc", "IntelCollectorService", "DttService"];
    let mut detected_service = None;
    let mut is_running = false;

    for svc in candidate_services {
        if let Some(state) = query_service_status(svc) {
            detected_service = Some(svc.to_string());
            if state == SERVICE_RUNNING {
                is_running = true;
                break;
            }
        }
    }

    let running_procs = check_running_dtt_processes();
    if !running_procs.is_empty() {
        is_running = true;
    }

    let apo_config = find_apo_config();

    let status = if is_running {
        IntelDttStatus::ActiveRunning
    } else if detected_service.is_some() || apo_config.is_some() {
        IntelDttStatus::InstalledStopped
    } else {
        IntelDttStatus::NotInstalled
    };

    IntelDttReport {
        status,
        service_name: detected_service,
        running_processes: running_procs,
        apo_config_path: apo_config,
    }
}

#[cfg(not(windows))]
pub fn detect_intel_dtt() -> IntelDttReport {
    IntelDttReport {
        status: IntelDttStatus::NotInstalled,
        service_name: None,
        running_processes: Vec::new(),
        apo_config_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intel_dtt_service_detection() {
        let report = detect_intel_dtt();
        println!("Intel DTT Status: {}", report.status);
        if let Some(svc) = &report.service_name {
            println!("  Service: {}", svc);
        }
        for proc in &report.running_processes {
            println!("  Process: {}", proc);
        }
        if let Some(cfg) = &report.apo_config_path {
            println!("  APO Config: {}", cfg);
        }
    }
}
