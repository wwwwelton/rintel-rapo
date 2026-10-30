use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use winreg::{
    enums::{HKEY_LOCAL_MACHINE, KEY_WRITE},
    RegKey,
};

// Group Policy QoS policy per executable. Windows marks matching packets with DSCP 46 (EF).
// Routers and ISPs usually ignore DSCP on public internet paths, so the ping gain is not guaranteed.
const POLICY_ROOT: &str = r"SOFTWARE\Policies\Microsoft\Windows\QoS";
const POLICY_PREFIX: &str = "RintelRAPO-";
const DSCP_EXPEDITED_FORWARDING: &str = "46";
const LOG_FILE: &str = "rintel-rapo-netqos.log";
const ADMIN_REQUIRED: &str = "Tweak de rede exige executar como administrador";

#[derive(Default)]
pub struct NetQos {
    applied: HashSet<PathBuf>,
    // exe -> error. Kept while the exe runs so the alert stays visible and nothing retries in a loop.
    failed: HashMap<PathBuf, String>,
}

impl NetQos {
    pub fn applied(&self) -> &HashSet<PathBuf> {
        &self.applied
    }

    /// Makes the policy set match `active`. Returns the error while any write or removal is failing.
    pub fn sync(&mut self, active: &[&Path]) -> Result<(), String> {
        let keep: HashSet<PathBuf> = active.iter().map(|exe| file_name(exe)).collect();
        for name in &keep {
            if self.applied.contains(name) || self.failed.contains_key(name) {
                continue;
            }
            match write_policy(name) {
                Ok(()) => {
                    log(&format!("APPLIED dscp={DSCP_EXPEDITED_FORWARDING} exe={}", name.display()));
                    self.applied.insert(name.clone());
                }
                Err(error) => {
                    log(&format!("FAILED exe={} error={error}", name.display()));
                    self.failed.insert(name.clone(), error);
                }
            }
        }
        let mut removal_error = None;
        for name in self.applied.difference(&keep).cloned().collect::<Vec<_>>() {
            match remove_policy(&name) {
                Ok(()) => {
                    log(&format!("REMOVED exe={}", name.display()));
                    self.applied.remove(&name);
                }
                Err(error) => {
                    log(&format!("REMOVE_FAILED exe={} error={error}", name.display()));
                    removal_error.get_or_insert(format!("{}: {error}", name.display()));
                }
            }
        }
        self.failed.retain(|name, _| keep.contains(name));
        if let Some((name, error)) = self.failed.iter().next() {
            return Err(format!("{}: {error}", name.display()));
        }
        removal_error.map_or(Ok(()), Err)
    }

    /// Deletes every policy and forgets state; the next sync re-applies running games.
    pub fn reset(&mut self) -> Result<(), String> {
        self.applied.clear();
        self.failed.clear();
        clear_all()
    }
}

impl Drop for NetQos {
    fn drop(&mut self) {
        if let Err(error) = self.sync(&[]) {
            log(&format!("SHUTDOWN error={error}"));
        }
    }
}

/// Deletes every `RintelRAPO-*` policy, including ones left behind by a crash.
pub fn clear_all() -> Result<(), String> {
    let qos = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(POLICY_ROOT) {
        Ok(key) => key,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(access_error(error)),
    };
    let names: Vec<String> = qos
        .enum_keys()
        .filter_map(Result::ok)
        .filter(|name| name.starts_with(POLICY_PREFIX))
        .collect();
    for name in names {
        delete_key(&format!(r"{POLICY_ROOT}\{name}"))?;
        log(&format!("CLEARED {name}"));
    }
    Ok(())
}

fn file_name(exe: &Path) -> PathBuf {
    exe.file_name().map_or_else(|| exe.to_path_buf(), PathBuf::from)
}

fn policy_key(name: &Path) -> String {
    format!(r"{POLICY_ROOT}\{POLICY_PREFIX}{}", name.display())
}

fn access_error(error: std::io::Error) -> String {
    if error.kind() == ErrorKind::PermissionDenied {
        ADMIN_REQUIRED.to_string()
    } else {
        error.to_string()
    }
}

fn write_policy(name: &Path) -> Result<(), String> {
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(policy_key(name), KEY_WRITE)
        .map_err(access_error)?;
    let values = [
        ("Version", "1.0".to_string()),
        ("Application Name", name.display().to_string()),
        ("Protocol", "*".to_string()),
        ("Local Port", "*".to_string()),
        ("Local IP", "*".to_string()),
        ("Local IP Prefix Length", "*".to_string()),
        ("Remote Port", "*".to_string()),
        ("Remote IP", "*".to_string()),
        ("Remote IP Prefix Length", "*".to_string()),
        ("DSCP Value", DSCP_EXPEDITED_FORWARDING.to_string()),
        ("Throttle Rate", "-1".to_string()),
    ];
    for (field, value) in values {
        key.set_value(field, &value).map_err(access_error)?;
    }
    Ok(())
}

fn remove_policy(name: &Path) -> Result<(), String> {
    delete_key(&policy_key(name))
}

fn delete_key(path: &str) -> Result<(), String> {
    match RegKey::predef(HKEY_LOCAL_MACHINE).delete_subkey_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(access_error(error)),
    }
}

fn log(line: &str) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let Some(path) = std::env::current_exe().ok().map(|exe| exe.with_file_name(LOG_FILE)) else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{secs} {line}");
    }
}
