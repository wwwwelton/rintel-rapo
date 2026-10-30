use serde::Serialize;

/// Decimal Mbps -> integer bits/s, without float rounding, NaN, or saturating casts.
pub fn bits_per_second(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (whole, fractional) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) || fractional.len() > 6
        || !fractional.bytes().all(|c| c.is_ascii_digit()) { return Err("Taxa deve ser Mbps decimal positivo, até 6 casas, sem unidade/expoente.".into()); }
    let whole: u64 = whole.parse().map_err(|_| "Taxa excede UInt64 bits/s")?;
    let fraction: u64 = if fractional.is_empty() { 0 } else { fractional.parse::<u64>().map_err(|_| "Taxa inválida")? * 10_u64.pow(6 - fractional.len() as u32) };
    let rate = whole.checked_mul(1_000_000).and_then(|v| v.checked_add(fraction)).filter(|v| *v > 0)
        .ok_or("Taxa deve ser positiva e caber em UInt64 bits/s")?;
    Ok(rate)
}
#[derive(Clone, Serialize)]
pub struct RateView {
    pub exe: String,
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
    pub struct Policy { pub view: RateView, proof: Option<serde_json::Value>, pid: u32, dscp: Option<i32>, attempted: bool }
    pub fn executable(pid: u32, expected: &Path) -> Result<(), String> {
        if !expected.is_absolute() || expected.extension().is_none_or(|v| !v.eq_ignore_ascii_case("exe")) || !expected.is_file() { return Err("Alvo deve ser caminho completo de um .exe existente.".into()); }
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
    impl Policy {
        pub fn check_target(pid: u32, exe: &Path) -> Result<(), String> { executable(pid, exe) }
        pub fn intent(pid: u32, exe: &Path, bps: u64) -> Result<Self, String> {
            executable(pid, exe)?;
            if bps == 0 { return Err("Taxa zero não autorizada".into()); }
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
            let name = format!("RintelRAPO-Rate-{}-{nonce}", std::process::id());
            Ok(Self { view: RateView { exe: exe.to_string_lossy().into_owned(), policy: name, bps, confirmed: false, conflict: false }, proof: None, pid, dscp: None, attempted: false })
        }
        fn payload(&self, action: &str) -> serde_json::Value {
            serde_json::json!({ "action": action, "name": self.view.policy, "exe": self.view.exe, "bps": self.view.bps.to_string(), "proof": self.proof, "dscp": self.dscp })
        }
        pub fn create(&mut self) -> Result<(), String> {
            executable(self.pid, Path::new(&self.view.exe))?;
            self.create_with(invoke)
        }
        fn create_with(&mut self, mut invoke: impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            let preparation = invoke(self.payload("prepare"))?;
            self.dscp = Some(preparation.get("dscp").and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok()).filter(|v| (-1..=63).contains(v)).ok_or("DSCP anterior não pôde ser confirmado")?);
            self.attempted = true;
            let result = invoke(self.payload("create"))?;
            self.proof = Some(result.get("proof").filter(|v| !v.is_null()).ok_or("QoS criada sem prova de configuração; ownership mantido")?.clone());
            self.view.confirmed = true; Ok(())
        }
        pub fn validate(&mut self) -> Result<(), String> {
            executable(self.pid, Path::new(&self.view.exe))?;
            let result = invoke(self.payload("validate"));
            if result.is_err() { self.view.conflict = true; }
            result.map(|_| ())
        }
        pub fn remove(&mut self) -> Result<(), String> {
            self.remove_with(invoke)
        }
        fn remove_with(&mut self, invoke: impl FnOnce(serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
            if !self.attempted { return Ok(()); }
            let result = invoke(self.payload("remove"));
            if result.is_err() { self.view.conflict = true; }
            result.map(|_| ())
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn policy() -> Policy {
            Policy { view: RateView { exe: "C:\\game\\a';&.exe".into(), policy: "RintelRAPO-Rate-7-123".into(), bps: 1_250_000, confirmed: false, conflict: false }, proof: None, pid: 7, dscp: None, attempted: false }
        }
        #[test]
        fn interrupted_create_keeps_intent_and_expected_dscp_for_safe_cleanup() {
            let mut policy = policy();
            assert!(policy.create_with(|request| {
                if request["action"] == "prepare" { Ok(serde_json::json!({ "dscp": 46 })) } else { Err("timeout after possible creation".into()) }
            }).is_err());
            assert!(policy.attempted); assert!(!policy.view.confirmed);
            assert_eq!(policy.dscp, Some(46));
            assert!(policy.remove_with(|request| {
                assert_eq!(request["exe"], "C:\\game\\a';&.exe");
                assert_eq!(request["dscp"], 46);
                assert_eq!(request["bps"], "1250000");
                Err("admin denied".into())
            }).is_err());
            assert!(policy.view.conflict); assert!(policy.attempted);
            policy.remove_with(|_| Ok(serde_json::json!({ "ok": true }))).unwrap();
        }
        #[test]
        fn cleanup_conflict_keeps_full_creation_proof_and_prepare_failure_never_mutates() {
            let mut policy = policy();
            assert!(policy.create_with(|_| Err("access denied before create".into())).is_err());
            assert!(!policy.attempted);
            policy.remove_with(|_| panic!("No possible creation needs no OS deletion")).unwrap();
            let proof = serde_json::json!([["Name", "unique"], ["DSCPAction", 46], ["ThrottleRateAction", 1250000]]);
            policy.create_with(|request| Ok(if request["action"] == "prepare" { serde_json::json!({ "dscp": 46 }) } else { serde_json::json!({ "proof": proof }) })).unwrap();
            assert!(policy.view.confirmed);
            assert!(policy.remove_with(|request| {
                assert_eq!(request["proof"], proof);
                Err("attributes changed externally; preserved".into())
            }).is_err());
            assert_eq!(policy.proof, Some(proof)); assert!(policy.view.conflict);
        }
        #[test]
        fn rejects_relative_non_executable_and_wrong_pid_targets() {
            for path in ["game.exe", "C:\\game\\config.json", "C:\\missing\\game.exe"] {
                assert!(Policy::check_target(0, Path::new(path)).is_err());
            }
            let exe = std::env::current_exe().unwrap();
            assert!(Policy::check_target(0, &exe).is_err());
            Policy::check_target(std::process::id(), &exe).unwrap();
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
    }
}
