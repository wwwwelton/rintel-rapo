use serde::Serialize;

/// Decimal Mbps -> integer bits/s, without float rounding, NaN, or saturating casts.
pub fn bits_per_second(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (whole, fractional) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) || fractional.len() > 6
        || !fractional.bytes().all(|c| c.is_ascii_digit()) { return Err("Taxa deve ser decimal positiva, até 6 casas, sem unidade/expoente.".into()); }
    let whole: u64 = whole.parse().map_err(|_| "Taxa excede UInt64 bits/s")?;
    let fraction: u64 = if fractional.is_empty() { 0 } else { fractional.parse::<u64>().map_err(|_| "Taxa inválida")? * 10_u64.pow(6 - fractional.len() as u32) };
    let rate = whole.checked_mul(1_000_000).and_then(|v| v.checked_add(fraction)).filter(|v| *v > 0)
        .ok_or("Taxa deve ser positiva e caber em UInt64 bits/s")?;
    Ok(rate)
}

/// Decimal MB/s (megabytes, 10^6 bytes) -> integer bits/s.
pub fn megabytes_per_second(text: &str) -> Result<u64, String> {
    bits_per_second(text)?.checked_mul(8).ok_or_else(|| "Taxa excede UInt64 bits/s".into())
}

#[derive(Clone, Serialize)]
pub struct RateView {
    /// Executables kept outside the system limit (Steam, RAPO, the target game).
    pub exempt: Vec<String>,
    pub policy: String,
    pub bps: u64,
    pub confirmed: bool,
    pub conflict: bool,
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{io::{Read, Write}, path::{Path, PathBuf}, process::{Command, Stdio}, os::windows::process::CommandExt, time::{Duration, Instant}};
    use windows_sys::Win32::{Foundation::{CloseHandle, GetLastError}, System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION}};

    /// Throttle of the exemption rules: far above any link, so exempt traffic is never limited.
    const UNLIMITED_BPS: u64 = 1_000_000_000_000;

    pub fn executable(pid: u32, expected: &Path) -> Result<(), String> {
        existing_exe(expected)?;
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() { return Err(format!("Alvo indisponível/acesso negado (Windows {})", GetLastError())); }
            let mut buffer = vec![0u16; 32768]; let mut length = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length); CloseHandle(process);
            if ok == 0 { return Err("Não foi possível revalidar executável do PID.".into()); }
            use std::os::windows::ffi::OsStringExt;
            let actual = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
            if !crate::windows::same_executable(&actual, expected) { return Err("PID não corresponde mais ao executável autorizado.".into()); }
        }
        Ok(())
    }
    fn existing_exe(path: &Path) -> Result<(), String> {
        if !path.is_absolute() || path.extension().is_none_or(|v| !v.eq_ignore_ascii_case("exe")) || !path.is_file() { return Err("Alvo deve ser caminho completo de um .exe existente.".into()); }
        Ok(())
    }
    fn invoke(payload: serde_json::Value) -> Result<serde_json::Value, String> {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot indisponível")?;
        let binary = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut child = Command::new(binary).args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", include_str!("network_qos.ps1")])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x08000000).spawn().map_err(|e| e.to_string())?;
        let input = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        let sent = child.stdin.take().unwrap().write_all(&input);
        if let Err(error) = sent { let _ = child.kill(); let _ = child.wait(); return Err(error.to_string()); }
        let stdout = child.stdout.take().unwrap(); let stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || { let mut data = Vec::new(); let result = stdout.take(1_048_576).read_to_end(&mut data); (data, result) });
        let err = std::thread::spawn(move || { let mut data = Vec::new(); let _ = stderr.take(65536).read_to_end(&mut data); data });
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                other => { let _ = child.kill(); let _ = child.wait(); return Err(format!("Consulta QoS interrompida/timeout; ownership mantido para inspecionar e remover. {other:?}")); }
            }
        };
        let (bytes, read) = out.join().map_err(|_| "Leitor QoS falhou")?; read.map_err(|e| e.to_string())?;
        let stderr = err.join().unwrap_or_default();
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| format!("QoS sem resposta JSON válida: {}", String::from_utf8_lossy(&stderr)))?;
        if !status.success() || value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(format!("QoS ActiveStore: {} (exige administrador; erro não significa sucesso)", value.get("error").and_then(|v| v.as_str()).unwrap_or("resposta inválida")));
        }
        Ok(value)
    }

    /// One ActiveStore policy owned by this session: the system-wide `-Default` limit, or an exemption for one executable.
    struct Rule { name: String, exe: Option<PathBuf>, pid: Option<u32>, bps: u64, proof: Option<serde_json::Value>, dscp: Option<i32>, attempted: bool }
    impl Rule {
        fn payload(&self, action: &str) -> serde_json::Value {
            serde_json::json!({ "action": action, "scope": if self.exe.is_some() { "app" } else { "default" }, "name": self.name,
                "exe": self.exe.as_ref().map(|e| e.to_string_lossy().into_owned()), "bps": self.bps.to_string(), "proof": self.proof, "dscp": self.dscp })
        }
        fn check(&self) -> Result<(), String> {
            match (&self.exe, self.pid) {
                (Some(exe), Some(pid)) => executable(pid, exe),
                (Some(exe), None) => existing_exe(exe),
                (None, _) => Ok(()),
            }
        }
        fn create_with(&mut self, invoke: &mut impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            let preparation = invoke(self.payload("prepare"))?;
            self.dscp = Some(preparation.get("dscp").and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok()).filter(|v| (-1..=63).contains(v)).ok_or("DSCP anterior não pôde ser confirmado")?);
            // Intent is recorded before the mutation: a timeout must not lose cleanup ownership.
            self.attempted = true;
            let result = invoke(self.payload("create"))?;
            self.proof = Some(result.get("proof").filter(|v| !v.is_null()).ok_or("QoS criada sem prova de configuração; ownership mantido")?.clone());
            Ok(())
        }
        fn remove_with(&mut self, invoke: &mut impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            if !self.attempted { return Ok(()); }
            invoke(self.payload("remove"))?;
            self.attempted = false;
            Ok(())
        }
    }

    /// System-wide outbound limit plus exemptions. A per-executable policy has more match conditions than
    /// the `-Default` one, so Windows applies the exemption to those executables instead of the limit.
    pub struct Policy { pub view: RateView, limit: Rule, exempt: Vec<Rule> }
    impl Policy {
        pub fn check_target(pid: u32, exe: &Path) -> Result<(), String> { executable(pid, exe) }
        /// `game` is the target process, exempted together with Steam and RAPO itself (which relays the game's traffic).
        pub fn intent(game: Option<(u32, &Path)>, bps: u64) -> Result<Self, String> {
            if bps == 0 { return Err("Taxa zero não autorizada".into()); }
            if let Some((pid, exe)) = game { executable(pid, exe)?; }
            let steam = crate::windows::steam_root().map(|root| root.join("steam.exe")).filter(|exe| exe.is_file());
            let rapo = std::env::current_exe().ok();
            Self::with_exemptions(game.map(|(pid, exe)| (pid, exe.to_path_buf())), [steam, rapo].into_iter().flatten().collect(), bps)
        }
        fn with_exemptions(game: Option<(u32, PathBuf)>, others: Vec<PathBuf>, bps: u64) -> Result<Self, String> {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
            let rule = |name: String, exe: Option<PathBuf>, pid: Option<u32>, bps| Rule { name, exe, pid, bps, proof: None, dscp: None, attempted: false };
            let mut exempt: Vec<Rule> = Vec::new();
            for (pid, exe) in game.map(|(pid, exe)| (Some(pid), exe)).into_iter().chain(others.into_iter().map(|exe| (None, exe))) {
                if exempt.iter().any(|r| r.exe.as_ref().is_some_and(|e| crate::windows::same_executable(e, &exe))) { continue; }
                exempt.push(rule(format!("RintelRAPO-Exempt-{}-{nonce}-{}", std::process::id(), exempt.len()), Some(exe), pid, UNLIMITED_BPS));
            }
            let limit = rule(format!("RintelRAPO-Rate-{}-{nonce}", std::process::id()), None, None, bps);
            let view = RateView { exempt: exempt.iter().filter_map(|r| r.exe.as_ref()).map(|e| e.to_string_lossy().into_owned()).collect(),
                policy: limit.name.clone(), bps, confirmed: false, conflict: false };
            Ok(Self { view, limit, exempt })
        }
        pub fn create(&mut self) -> Result<(), String> {
            for rule in &self.exempt { rule.check()?; }
            self.create_with(invoke)
        }
        /// Exemptions first: the limit never exists while an exempt executable is still unprotected.
        fn create_with(&mut self, mut invoke: impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            for rule in &mut self.exempt { rule.create_with(&mut invoke)?; }
            self.limit.create_with(&mut invoke)?;
            self.view.confirmed = true;
            Ok(())
        }
        pub fn validate(&mut self) -> Result<(), String> {
            let result = self.exempt.iter().try_for_each(Rule::check)
                .and_then(|()| self.exempt.iter().chain([&self.limit]).try_for_each(|rule| invoke(rule.payload("validate")).map(drop)));
            if result.is_err() { self.view.conflict = true; }
            result
        }
        pub fn remove(&mut self) -> Result<(), String> { self.remove_with(invoke) }
        /// Limit first, then exemptions; a failure keeps the remaining intents for a retry.
        fn remove_with(&mut self, mut invoke: impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            let result = self.limit.remove_with(&mut invoke).and_then(|()| self.exempt.iter_mut().try_for_each(|rule| rule.remove_with(&mut invoke)));
            if result.is_err() { self.view.conflict = true; }
            result
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn policy() -> Policy {
            Policy::with_exemptions(None, vec![PathBuf::from("C:\\Steam\\steam.exe"), PathBuf::from("C:\\game\\a';&.exe")], 10_000_000).unwrap()
        }
        fn granted(request: &serde_json::Value) -> serde_json::Value {
            if request["action"] == "prepare" { serde_json::json!({ "dscp": -1 }) } else { serde_json::json!({ "proof": [["Name", request["name"].clone()]] }) }
        }
        #[test]
        fn exemptions_exist_before_the_system_limit_and_outlive_it() {
            let mut policy = policy();
            let mut created = Vec::new();
            policy.create_with(|request| { created.push((request["action"].as_str().unwrap().to_owned(), request["scope"].as_str().unwrap().to_owned(), request["exe"].clone())); Ok(granted(&request)) }).unwrap();
            let scopes: Vec<_> = created.iter().filter(|(action, ..)| action == "create").map(|(_, scope, _)| scope.as_str()).collect();
            assert_eq!(scopes, ["app", "app", "default"], "Steam and the game are exempt before the limit starts");
            assert!(created.iter().any(|(_, _, exe)| exe == "C:\\game\\a';&.exe"));
            assert_eq!(policy.view.exempt.len(), 2);
            let mut removed = Vec::new();
            policy.remove_with(|request| { removed.push(request["scope"].as_str().unwrap().to_owned()); Ok(serde_json::json!({ "ok": true })) }).unwrap();
            assert_eq!(removed, ["default", "app", "app"], "the limit goes away before any exemption");
        }
        #[test]
        fn interrupted_create_keeps_every_attempted_rule_for_cleanup_and_never_creates_the_limit_early() {
            let mut policy = policy();
            let mut calls = 0;
            assert!(policy.create_with(|request| { calls += 1; if calls == 4 { Err("timeout after possible creation".into()) } else { Ok(granted(&request)) } }).is_err());
            assert!(!policy.view.confirmed);
            let mut removed = Vec::new();
            assert!(policy.remove_with(|request| { removed.push(request["name"].as_str().unwrap().to_owned()); if request["scope"] == "default" { Ok(serde_json::json!({})) } else { Err("admin denied".into()) } }).is_err());
            assert!(removed.iter().all(|name| !name.starts_with("RintelRAPO-Rate-")), "the limit was never attempted, so it is never deleted: {removed:?}");
            assert!(policy.view.conflict);
            let mut retried = Vec::new();
            policy.remove_with(|request| { retried.push(request["name"].as_str().unwrap().to_owned()); Ok(serde_json::json!({})) }).unwrap();
            assert_eq!(retried.len(), 2, "both attempted exemptions are retried and nothing else: {retried:?}");
        }
        #[test]
        fn duplicate_exemptions_collapse_and_targets_are_revalidated() {
            let steam = PathBuf::from("C:\\Steam\\steam.exe");
            let policy = Policy::with_exemptions(None, vec![steam.clone(), PathBuf::from("C:\\STEAM\\STEAM.EXE")], 1).unwrap();
            assert_eq!(policy.view.exempt.len(), 1);
            for path in ["game.exe", "C:\\game\\config.json", "C:\\missing\\game.exe"] { assert!(Policy::check_target(0, Path::new(path)).is_err()); }
            let exe = std::env::current_exe().unwrap();
            assert!(Policy::check_target(0, &exe).is_err());
            Policy::check_target(std::process::id(), &exe).unwrap();
            assert!(Policy::intent(None, 0).is_err());
        }
    }
}
#[cfg(windows)]
pub use native::Policy;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_rate_boundary_never_rounds_or_overflows() {
        assert_eq!(bits_per_second("1.25").unwrap(), 1_250_000);
        assert_eq!(bits_per_second("0.000001").unwrap(), 1);
        assert_eq!(bits_per_second("18446744073709.551615").unwrap(), u64::MAX);
        for value in ["0", "-1", "NaN", "inf", "1e3", "1;Remove-NetQosPolicy", "1,5", "1.0000001", "18446744073709.551616", "18446744073709551616", "", ".5"] { assert!(bits_per_second(value).is_err(), "{value}"); }
        assert_eq!(megabytes_per_second("1.5").unwrap(), 12_000_000, "MB/s are megabytes");
        assert!(megabytes_per_second("18446744073709.551615").is_err(), "×8 must not overflow silently");
    }
}
