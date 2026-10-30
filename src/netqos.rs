use std::{collections::HashMap, io::{Read, Write}, os::windows::process::CommandExt, path::{Path, PathBuf}, process::{Command, Stdio}, sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc}, time::{Duration, Instant}};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Policy { name: String, exe: PathBuf, dscp: u8, confirmed: bool }
#[derive(Default)]
struct State { applied: HashMap<PathBuf, u8>, error: String }
pub struct NetQos {
    sender: mpsc::SyncSender<Vec<(PathBuf, u8)>>,
    receiver: mpsc::Receiver<State>,
    done: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    desired: Option<Vec<(PathBuf, u8)>>,
    state: State,
}

fn desired(active: &[&Path]) -> Vec<(PathBuf, u8)> {
    // Only the game executable gets DSCP 46. No background process policies.
    active.iter().map(|p| (p.to_path_buf(), 46u8)).collect()
}

fn reconcile(owned: &mut Vec<Policy>, wanted: &[(PathBuf, u8)], sequence: &mut u64, session: &str) -> State {
    if owned.is_empty() && wanted.is_empty() { return State::default(); }
    for (exe, dscp) in wanted {
        if !owned.iter().any(|p| crate::windows::same_executable(&p.exe, exe) && p.dscp == *dscp) {
            *sequence += 1;
            owned.push(Policy { name: format!("RintelRAPO-DSCP-{session}-{sequence}"), exe: exe.clone(), dscp: *dscp, confirmed: false });
        }
    }
    let request = serde_json::json!({"owned": owned, "desired": wanted});
    let result = invoke(&request).and_then(|value| {
        let policies: Vec<Policy> = serde_json::from_value(value["owned"].clone()).map_err(|e| e.to_string())?;
        *owned = policies;
        Ok(value["error"].as_str().unwrap_or("Resposta DSCP inválida").to_owned())
    });
    let error = result.unwrap_or_else(|e| {
        for p in owned.iter_mut() { p.confirmed = false; }
        e
    });
    State { applied: owned.iter().filter(|p| p.confirmed).map(|p| (p.exe.clone(), p.dscp)).collect(), error }
}

fn invoke(request: &serde_json::Value) -> Result<serde_json::Value, String> {
    let binary = PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot indisponível")?).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut child = Command::new(binary).args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", include_str!("netqos.ps1")])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x08000000).spawn().map_err(|e| e.to_string())?;
    let input = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if let Err(e) = child.stdin.take().unwrap().write_all(&input) { let _ = child.kill(); let _ = child.wait(); return Err(e.to_string()); }
    let stdout = child.stdout.take().unwrap(); let stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || { let mut bytes = Vec::new(); stdout.take(1_048_576).read_to_end(&mut bytes).map(|_| bytes) });
    let errors = std::thread::spawn(move || { let mut bytes = Vec::new(); let _ = stderr.take(65536).read_to_end(&mut bytes); bytes });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            other => { let _ = child.kill(); let _ = child.wait(); return Err(format!("DSCP interrompido; intenção de limpeza mantida: {other:?}")); }
        }
    }
    let bytes = output.join().map_err(|_| "Leitor DSCP falhou")?.map_err(|e| e.to_string())?;
    let stderr = errors.join().unwrap_or_default();
    serde_json::from_slice(&bytes).map_err(|_| format!("DSCP sem confirmação JSON: {}", String::from_utf8_lossy(&stderr)))
}

impl Default for NetQos {
    fn default() -> Self {
        let (sender, requests) = mpsc::sync_channel::<Vec<(PathBuf, u8)>>(1);
        let (results, receiver) = mpsc::channel(); let (finished, done) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false)); let stopping = stop.clone();
        let worker = std::thread::Builder::new().name("rapo-dscp".into()).spawn(move || {
            let session = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
            let mut owned = Vec::new(); let mut sequence = 0;
            while !stopping.load(Ordering::Acquire) {
                let wanted = match requests.recv_timeout(Duration::from_secs(2)) {
                    Ok(wanted) => wanted,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let _ = results.send(reconcile(&mut owned, &wanted, &mut sequence, &session));
            }
            if !owned.is_empty() {
                let state = reconcile(&mut owned, &[], &mut sequence, &session);
                if !state.error.is_empty() { eprintln!("Limpeza DSCP falhou: {}", state.error); }
            }
            let _ = finished.send(());
        });
        Self { sender, receiver, done, stop, desired: None, state: State { error: worker.err().map_or_else(String::new, |e| e.to_string()), ..State::default() } }
    }
}
impl NetQos {
    pub fn applied(&self) -> &HashMap<PathBuf, u8> { &self.state.applied }
    /// Only sends to the worker when desired state actually changes. Never spams PowerShell.
    pub fn sync(&mut self, active: &[&Path]) -> Result<(), String> {
        while let Ok(state) = self.receiver.try_recv() { self.state = state; }
        let wanted = desired(active);
        if self.desired.as_ref() == Some(&wanted) {
            return if self.state.error.is_empty() { Ok(()) } else { Err(self.state.error.clone()) };
        }
        match self.sender.try_send(wanted.clone()) {
            Ok(()) => { self.desired = Some(wanted); }
            Err(mpsc::TrySendError::Full(_)) => (),
            Err(mpsc::TrySendError::Disconnected(_)) => return Err("Worker DSCP indisponível".into()),
        }
        if self.state.error.is_empty() { Ok(()) } else { Err(self.state.error.clone()) }
    }
    pub fn reset(&mut self) -> Result<(), String> { self.desired = None; self.sync(&[]) }
}
impl Drop for NetQos {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if self.done.recv_timeout(Duration::from_secs(25)).is_err() { eprintln!("Limpeza DSCP pendente; encerramento forçado pode deixar políticas até reiniciar."); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_game_gets_dscp46() {
        let game = Path::new(r"C:\Games\Game.exe");
        assert!(desired(&[]).is_empty());
        let result = desired(&[game]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, 46);
    }
    #[test]
    #[ignore = "Elevated ActiveStore DSCP smoke; creates policies only for temporary test executables"]
    fn native_dscp_roundtrip_and_game_switch() {
        let root = std::env::temp_dir().join(format!("rapo-dscp-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let game = root.join("game.exe");
        let current = std::env::current_exe().unwrap();
        std::fs::copy(&current, &game).unwrap();
        let mut qos = NetQos::default();
        let wait = |qos: &mut NetQos, active: &[&Path], expected: usize| {
            let deadline = Instant::now() + Duration::from_secs(25);
            loop {
                qos.sync(active).unwrap();
                if qos.applied().len() == expected { break; }
                assert!(Instant::now() < deadline, "DSCP state did not converge: {:?}", qos.applied());
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        wait(&mut qos, &[&game], 1);
        assert_eq!(*qos.applied().get(&game).unwrap(), 46);
        wait(&mut qos, &[], 0);
        drop(qos);
        std::fs::remove_dir_all(root).unwrap();
    }
}
