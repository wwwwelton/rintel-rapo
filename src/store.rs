//! SQLite store for every measured performance profile: CPU/core trials and best profile per game,
//! plus relay connect statistics per destination and authorized exit. Startup reads the best ones back.

use crate::learning::{Profile, Score, Settings};
use crate::secret;
use crate::vpn::{Profile as VpnProfile, Protocol as VpnProtocol};
use rusqlite::{params, Connection};
use std::{net::SocketAddr, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

/// Relay connect statistics for one destination through one authorized exit (`Egress::authorization`,
/// stable across DHCP address changes). Failures count as the full connect timeout in `mean_ms`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathStat {
    pub destination: SocketAddr,
    pub authorization: String,
    pub mean_ms: f64,
    pub samples: u32,
    pub failures: u32,
}

pub struct Store { db: Connection }

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS system_trials (
    id INTEGER PRIMARY KEY, exe TEXT NOT NULL, measured_at INTEGER NOT NULL,
    priority INTEGER NOT NULL, pin INTEGER NOT NULL, qos INTEGER NOT NULL, threads INTEGER NOT NULL,
    min_percent INTEGER NOT NULL, cores INTEGER NOT NULL, smt INTEGER NOT NULL, p_only INTEGER NOT NULL,
    park_cores INTEGER NOT NULL, fps REAL NOT NULL, low_fps REAL NOT NULL);
CREATE INDEX IF NOT EXISTS system_trials_exe ON system_trials(exe);
CREATE TABLE IF NOT EXISTS system_best (
    exe TEXT PRIMARY KEY, updated_at INTEGER NOT NULL,
    priority INTEGER NOT NULL, pin INTEGER NOT NULL, qos INTEGER NOT NULL, threads INTEGER NOT NULL,
    min_percent INTEGER NOT NULL, cores INTEGER NOT NULL, smt INTEGER NOT NULL, p_only INTEGER NOT NULL,
    park_cores INTEGER NOT NULL, fps REAL NOT NULL, low_fps REAL NOT NULL);
CREATE TABLE IF NOT EXISTS store_metadata (key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS deleted_legacy_profiles (exe TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS relay_paths (
    destination TEXT NOT NULL, authorization TEXT NOT NULL, updated_at INTEGER NOT NULL,
    mean_ms REAL NOT NULL, samples INTEGER NOT NULL, failures INTEGER NOT NULL,
    PRIMARY KEY (destination, authorization));
CREATE TABLE IF NOT EXISTS vpn_profiles (
    name TEXT PRIMARY KEY, protocol TEXT NOT NULL, updated_at INTEGER NOT NULL,
    fields TEXT NOT NULL, secrets BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS vpn_active (
    id INTEGER PRIMARY KEY CHECK (id = 1), name TEXT NOT NULL REFERENCES vpn_profiles(name) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS udp_redundancy (
    id INTEGER PRIMARY KEY CHECK (id = 1), enabled INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS udp_redundancy_auto (
    id INTEGER PRIMARY KEY CHECK (id = 1), echo TEXT NOT NULL);";

const SETTINGS: &str = "priority, pin, qos, threads, min_percent, cores, smt, p_only, park_cores";

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64) }
#[cfg(windows)]
fn same_profile_path(a: &Path, b: &Path) -> bool { crate::windows::same_executable(a, b) }
#[cfg(not(windows))]
fn same_profile_path(a: &Path, b: &Path) -> bool { a == b }

fn text(error: rusqlite::Error) -> String { format!("SQLite: {error}") }

impl Store {
    /// `%LOCALAPPDATA%\Rintel RAPO\rapo.sqlite3`.
    pub fn open_default() -> Result<Self, String> {
        let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA ausente")?;
        let dir = PathBuf::from(base).join("Rintel RAPO");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Self::open(&dir.join("rapo.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let db = Connection::open(path).map_err(text)?;
        db.pragma_update(None, "foreign_keys", true).map_err(text)?;
        db.execute_batch(SCHEMA).map_err(text)?;
        Ok(Self { db })
    }

    /// Every measured candidate, including reference measurements, in measurement order.
    pub fn record_trial(&self, exe: &Path, settings: Settings, score: Score) -> Result<(), String> {
        let s = settings;
        self.db.execute(&format!("INSERT INTO system_trials (exe, measured_at, {SETTINGS}, fps, low_fps) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
            params![exe.to_string_lossy(), now(), s.priority, s.pin, s.qos, s.threads, s.min_percent, s.cores, s.smt, s.p_only, s.park_cores, score.fps, score.low_fps])
            .map(drop).map_err(text)
    }

    #[cfg(test)]
    pub fn trial_count(&self, exe: &Path) -> Result<i64, String> {
        self.db.query_row("SELECT COUNT(*) FROM system_trials WHERE exe = ?1", [exe.to_string_lossy()], |row| row.get(0)).map_err(text)
    }

    /// Replaces the stored best profile of `profile.exe`; the caller decides whether it improves.
    pub fn save_best(&self, profile: &Profile) -> Result<(), String> {
        if !profile.settings.valid() || !profile.score.valid() { return Err("Perfil inválido".into()); }
        let (s, score) = (profile.settings, profile.score);
        self.db.execute(&format!("INSERT OR REPLACE INTO system_best (exe, updated_at, {SETTINGS}, fps, low_fps) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
            params![profile.exe.to_string_lossy(), now(), s.priority, s.pin, s.qos, s.threads, s.min_percent, s.cores, s.smt, s.p_only, s.park_cores, score.fps, score.low_fps])
            .map(drop).map_err(text)
    }

    /// Best profile per game. Rows that fail validation are skipped instead of hiding every other profile.
    pub fn best_profiles(&self) -> Result<Vec<Profile>, String> {
        let mut query = self.db.prepare(&format!("SELECT exe, {SETTINGS}, fps, low_fps FROM system_best")).map_err(text)?;
        let rows = query.query_map([], |row| Ok(Profile {
            exe: PathBuf::from(row.get::<_, String>(0)?),
            settings: Settings { priority: row.get(1)?, pin: row.get(2)?, qos: row.get(3)?, threads: row.get(4)?, min_percent: row.get(5)?,
                cores: row.get(6)?, smt: row.get(7)?, p_only: row.get(8)?, park_cores: row.get(9)? },
            score: Score { fps: row.get(10)?, low_fps: row.get(11)? },
        })).map_err(text)?;
        Ok(rows.filter_map(Result::ok).filter(|p| p.settings.valid() && p.score.valid()).collect())
    }

    /// Removes the saved best while retaining the measurement history.
    pub fn delete_best(&self, exe: &Path) -> Result<(), String> {
        let profiles = self.best_profiles()?;
        let tx = self.db.unchecked_transaction().map_err(text)?;
        tx.execute("INSERT OR IGNORE INTO deleted_legacy_profiles (exe) VALUES (?1)", [exe.to_string_lossy()]).map_err(text)?;
        for profile in profiles.iter().filter(|profile| same_profile_path(&profile.exe, exe)) {
            tx.execute("DELETE FROM system_best WHERE exe = ?1", [profile.exe.to_string_lossy()]).map_err(text)?;
        }
        tx.commit().map_err(text)
    }

    /// One-time import of the registry profiles (formats 2-4); never overwrites a profile already in SQLite.
    pub fn import(&self, profiles: &[Profile]) -> Result<usize, String> {
        if self.db.query_row("SELECT EXISTS(SELECT 1 FROM store_metadata WHERE key = 'registry_profiles_imported')", [], |row| row.get::<_, bool>(0)).map_err(text)? {
            return Ok(0);
        }
        let tx = self.db.unchecked_transaction().map_err(text)?;
        let mut known = self.best_profiles()?;
        let mut query = self.db.prepare("SELECT exe FROM deleted_legacy_profiles").map_err(text)?;
        let deleted = query.query_map([], |row| row.get::<_, String>(0)).map_err(text)?
            .collect::<Result<Vec<_>, _>>().map_err(text)?;
        let mut imported = 0;
        for profile in profiles {
            if !known.iter().any(|k| same_profile_path(&k.exe, &profile.exe))
                && !deleted.iter().any(|exe| same_profile_path(Path::new(exe), &profile.exe)) {
                self.save_best(profile)?;
                known.push(profile.clone());
                imported += 1;
            }
        }
        tx.execute("INSERT INTO store_metadata (key) VALUES ('registry_profiles_imported')", []).map_err(text)?;
        tx.commit().map_err(text)?;
        Ok(imported)
    }

    pub fn save_paths(&self, paths: &[PathStat]) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(text)?;
        for p in paths.iter().filter(|p| p.samples > 0 && p.mean_ms.is_finite() && p.mean_ms >= 0.0) {
            tx.execute("INSERT OR REPLACE INTO relay_paths (destination, authorization, updated_at, mean_ms, samples, failures) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![p.destination.to_string(), p.authorization, now(), p.mean_ms, p.samples, p.failures]).map_err(text)?;
        }
        tx.commit().map_err(text)
    }

    pub fn paths(&self) -> Result<Vec<PathStat>, String> {
        let mut query = self.db.prepare("SELECT destination, authorization, mean_ms, samples, failures FROM relay_paths").map_err(text)?;
        let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?, row.get::<_, u32>(3)?, row.get::<_, u32>(4)?)))
            .map_err(text)?;
        Ok(rows.filter_map(Result::ok).filter_map(|(destination, authorization, mean_ms, samples, failures)|
            Some(PathStat { destination: destination.parse().ok()?, authorization, mean_ms, samples, failures })).collect())
    }

    /// Stores or replaces a validated profile. Secrets are sealed (DPAPI) before they reach the database; when
    /// sealing fails the profile is not stored at all, never stored in clear text.
    pub fn save_vpn(&self, profile: &VpnProfile) -> Result<(), String> {
        profile.validate()?;
        let fields = serde_json::to_string(&profile.fields).map_err(|e| e.to_string())?;
        let secrets = secret::seal(serde_json::to_string(&profile.secrets).map_err(|e| e.to_string())?.as_bytes())?;
        self.db.execute("INSERT INTO vpn_profiles (name, protocol, updated_at, fields, secrets) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(name) DO UPDATE SET protocol = ?2, updated_at = ?3, fields = ?4, secrets = ?5",
            params![profile.name, profile.protocol.id(), now(), fields, secrets]).map(drop).map_err(text)
    }

    /// Saves an edit coming from the page. Secrets are never sent back to the page, so one left empty keeps the
    /// stored value of the profile with the same name and protocol; a new profile must supply every required secret.
    pub fn save_vpn_edit(&self, mut profile: VpnProfile) -> Result<(), String> {
        profile.secrets.retain(|_, value| !value.is_empty());
        if let Some(stored) = self.vpn_profiles()?.into_iter().find(|p| p.name == profile.name && p.protocol == profile.protocol) {
            for (key, value) in stored.secrets { profile.secrets.entry(key).or_insert(value); }
        }
        self.save_vpn(&profile)
    }

    /// Every stored profile with its secrets opened. A row whose secrets cannot be decrypted (another Windows
    /// account, or tampering) is skipped instead of failing the whole list.
    pub fn vpn_profiles(&self) -> Result<Vec<VpnProfile>, String> {
        let mut query = self.db.prepare("SELECT name, protocol, fields, secrets FROM vpn_profiles ORDER BY name").map_err(text)?;
        let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?))).map_err(text)?;
        Ok(rows.filter_map(Result::ok).filter_map(|(name, protocol, fields, sealed)| Some(VpnProfile {
            name, protocol: VpnProtocol::parse(&protocol)?,
            fields: serde_json::from_str(&fields).ok()?,
            secrets: serde_json::from_slice(&secret::open(&sealed)?).ok()?,
        })).filter(|p| p.validate().is_ok()).collect())
    }

    /// Removes a profile; if it was the active one, no profile stays active.
    pub fn remove_vpn(&self, name: &str) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(text)?;
        tx.execute("DELETE FROM vpn_active WHERE name = ?1", [name]).map_err(text)?;
        tx.execute("DELETE FROM vpn_profiles WHERE name = ?1", [name]).map_err(text)?;
        tx.commit().map_err(text)
    }

    /// Makes `name` the only active profile, replacing the previous one in a single statement.
    pub fn set_active_vpn(&self, name: Option<&str>) -> Result<(), String> {
        let Some(name) = name else { return self.db.execute("DELETE FROM vpn_active", []).map(drop).map_err(text) };
        self.db.execute("INSERT INTO vpn_active (id, name) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET name = ?1", [name])
            .map(drop).map_err(|_| "Perfil de VPN inexistente".into())
    }

    pub fn active_vpn(&self) -> Result<Option<String>, String> {
        match self.db.query_row("SELECT name FROM vpn_active WHERE id = 1", [], |row| row.get(0)) {
            Ok(name) => Ok(Some(name)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(text(error)),
        }
    }

    pub fn save_redundancy(&self, config: crate::relay::RedundancyConfig) -> Result<(), String> {
        config.validate().map_err(|e| e.to_string())?;
        self.db.execute("INSERT INTO udp_redundancy (id, enabled, x, y) VALUES (1, ?1, ?2, ?3)
            ON CONFLICT(id) DO UPDATE SET enabled = ?1, x = ?2, y = ?3",
            params![if config.enabled { 1 } else { 0 }, config.x, config.y]).map(drop).map_err(text)
    }

    /// Loopback echo for learning-driven ratio selection; `None` when automatic selection is off.
    pub fn save_auto_echo(&self, echo: Option<std::net::SocketAddr>) -> Result<(), String> {
        if echo.is_some_and(|addr| !addr.ip().is_loopback()) {
            return Err("Eco de redundância deve ser loopback".into());
        }
        match echo {
            Some(addr) => self.db.execute("INSERT INTO udp_redundancy_auto (id, echo) VALUES (1, ?1)
                ON CONFLICT(id) DO UPDATE SET echo = ?1", params![addr.to_string()]).map(drop).map_err(text),
            None => self.db.execute("DELETE FROM udp_redundancy_auto WHERE id = 1", []).map(drop).map_err(text),
        }
    }

    pub fn auto_echo(&self) -> Result<Option<std::net::SocketAddr>, String> {
        match self.db.query_row("SELECT echo FROM udp_redundancy_auto WHERE id = 1", [], |row| row.get::<_, String>(0)) {
            Ok(echo) => {
                let addr: std::net::SocketAddr = echo.parse().map_err(|e| format!("SQLite: eco inválido ({e}): {echo}"))?;
                if !addr.ip().is_loopback() {
                    return Err(format!("SQLite: eco de redundância não é loopback: {addr}"));
                }
                Ok(Some(addr))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(text(error)),
        }
    }

    /// AION2 focus mode is on by default; a `focus_disabled` marker records the user's opt-out.
    pub fn focus_enabled(&self) -> Result<bool, String> {
        self.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM store_metadata WHERE key = 'focus_disabled')", [], |row| row.get(0)).map_err(text)
    }

    pub fn set_focus_enabled(&self, on: bool) -> Result<(), String> {
        let sql = if on { "DELETE FROM store_metadata WHERE key = 'focus_disabled'" } else { "INSERT OR IGNORE INTO store_metadata (key) VALUES ('focus_disabled')" };
        self.db.execute(sql, []).map(drop).map_err(text)
    }

    pub fn redundancy(&self) -> Result<crate::relay::RedundancyConfig, String> {
        match self.db.query_row("SELECT enabled, x, y FROM udp_redundancy WHERE id = 1", [], |row| {
            Ok((row.get::<_, i64>(0)? != 0, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?))
        }) {
            Ok((enabled, x, y)) => crate::relay::RedundancyConfig::new(enabled, x, y).map_err(|e| e.to_string()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(crate::relay::RedundancyConfig::default()),
            Err(error) => Err(text(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Store { Store::open(Path::new(":memory:")).unwrap() }
    fn profile(exe: &str, fps: f64) -> Profile {
        Profile { exe: exe.into(), settings: Settings { pin: true, cores: 6, ..Settings::default() }, score: Score { fps, low_fps: fps * 0.8 } }
    }

    #[test]
    fn focus_mode_defaults_on_and_remembers_opt_out() {
        let store = memory();
        assert!(store.focus_enabled().unwrap());
        store.set_focus_enabled(false).unwrap();
        assert!(!store.focus_enabled().unwrap());
        store.set_focus_enabled(true).unwrap();
        assert!(store.focus_enabled().unwrap());
    }

    #[test]
    fn trials_and_best_profiles_survive_reopening_the_database() {
        let path = std::env::temp_dir().join(format!("rapo-store-{}.sqlite3", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            let best = profile("C:\\AION2\\AION2.exe", 118.0);
            store.record_trial(&best.exe, Settings::default(), Score { fps: 100.0, low_fps: 70.0 }).unwrap();
            store.record_trial(&best.exe, best.settings, best.score).unwrap();
            store.save_best(&profile("C:\\AION2\\AION2.exe", 90.0)).unwrap();
            store.save_best(&best).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.trial_count(Path::new("C:\\AION2\\AION2.exe")).unwrap(), 2);
        let saved = store.best_profiles().unwrap();
        assert_eq!(saved.len(), 1, "one best profile per game");
        assert_eq!((saved[0].settings, saved[0].score.fps), (profile("", 0.0).settings, 118.0));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn registry_import_never_replaces_a_sqlite_profile_and_invalid_rows_are_skipped() {
        let store = memory();
        store.save_best(&profile("a.exe", 120.0)).unwrap();
        assert_eq!(store.import(&[profile("a.exe", 60.0), profile("b.exe", 80.0)]).unwrap(), 1);
        assert_eq!(store.best_profiles().unwrap().iter().find(|p| p.exe == Path::new("a.exe")).unwrap().score.fps, 120.0);
        assert!(store.save_best(&profile("c.exe", f64::NAN)).is_err());
        store.db.execute("UPDATE system_best SET threads = 0 WHERE exe = 'b.exe'", []).unwrap();
        assert_eq!(store.best_profiles().unwrap().len(), 1, "a corrupt row must not hide the others");
    }

    #[test]
    fn deleting_best_preserves_trials_and_does_not_reimport_registry() {
        let store = memory();
        let saved = profile("game.exe", 100.0);
        store.import(std::slice::from_ref(&saved)).unwrap();
        store.record_trial(&saved.exe, saved.settings, saved.score).unwrap();
        store.delete_best(&saved.exe).unwrap();
        assert_eq!(store.import(std::slice::from_ref(&saved)).unwrap(), 0);
        assert!(store.best_profiles().unwrap().is_empty());
        assert_eq!(store.trial_count(&saved.exe).unwrap(), 1);
    }

    #[test]
    fn deletion_before_legacy_import_prevents_resurrection() {
        let store = memory();
        let saved = profile("game.exe", 100.0);
        store.save_best(&saved).unwrap();
        store.delete_best(&saved.exe).unwrap();
        assert_eq!(store.import(&[saved]).unwrap(), 0);
        assert!(store.best_profiles().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn deleting_best_removes_equivalent_windows_paths() {
        let store = memory();
        store.save_best(&profile("C:\\Game\\game.exe", 100.0)).unwrap();
        store.save_best(&profile("c:\\game\\GAME.exe", 110.0)).unwrap();
        store.delete_best(Path::new("C:\\GAME\\game.exe")).unwrap();
        assert!(store.best_profiles().unwrap().is_empty());
    }

    #[test]
    fn relay_paths_are_replaced_per_destination_and_exit() {
        let store = memory();
        let a: SocketAddr = "203.0.113.1:443".parse().unwrap();
        let path = |authorization: &str, mean_ms| PathStat { destination: a, authorization: authorization.into(), mean_ms, samples: 4, failures: 0 };
        store.save_paths(&[path("system", 40.0), path("7:2:192.0.2.1%0", 18.0)]).unwrap();
        store.save_paths(&[path("system", 35.0), PathStat { samples: 0, ..path("hy2", 1.0) }]).unwrap();
        let mut saved = store.paths().unwrap();
        saved.sort_by(|x, y| x.authorization.cmp(&y.authorization));
        assert_eq!(saved, vec![path("7:2:192.0.2.1%0", 18.0), path("system", 35.0)], "unmeasured exits are not stored");
    }

    fn vpn_profile(name: &str) -> VpnProfile {
        let mut profile = VpnProfile::new(VpnProtocol::Hysteria2, name);
        profile.fields.insert("server".into(), "vps.example.com:443".into());
        profile.secrets.insert("auth".into(), "s3cret-password".into());
        profile
    }

    #[test]
    fn vpn_secrets_are_never_stored_in_clear_text_and_an_unsealable_profile_is_not_stored() {
        let store = memory();
        let result = store.save_vpn(&vpn_profile("HY2"));
        #[cfg(not(windows))]
        assert!(result.is_err() && store.vpn_profiles().unwrap().is_empty(), "no DPAPI: nothing may be written, not even in clear text");
        #[cfg(windows)]
        {
            result.unwrap();
            let raw: Vec<u8> = store.db.query_row("SELECT secrets FROM vpn_profiles WHERE name = 'HY2'", [], |r| r.get(0)).unwrap();
            assert!(!raw.windows(8).any(|w| w == b"s3cret-p"), "the password must not appear in the stored blob");
            let fields: String = store.db.query_row("SELECT fields FROM vpn_profiles", [], |r| r.get(0)).unwrap();
            assert!(!fields.contains("s3cret"), "the readable fields never carry a secret");
            assert_eq!(store.vpn_profiles().unwrap(), vec![vpn_profile("HY2")], "secrets come back through DPAPI");
        }
    }

    #[test]
    fn invalid_vpn_profile_is_rejected_before_anything_is_stored() {
        let store = memory();
        let mut bad = vpn_profile("HY2");
        bad.fields.insert("server".into(), "no-port".into());
        assert!(store.save_vpn(&bad).is_err());
        assert_eq!(store.db.query_row::<i64, _, _>("SELECT COUNT(*) FROM vpn_profiles", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn only_one_vpn_profile_is_active_and_removing_it_clears_the_activation() {
        let store = memory();
        // Rows are inserted directly: this exercises the activation rules independent of DPAPI.
        for name in ["a", "b"] {
            store.db.execute("INSERT INTO vpn_profiles (name, protocol, updated_at, fields, secrets) VALUES (?1, 'hysteria2', 0, '{}', x'00')", [name]).unwrap();
        }
        assert_eq!(store.active_vpn().unwrap(), None, "nothing is active by default");
        store.set_active_vpn(Some("a")).unwrap();
        store.set_active_vpn(Some("b")).unwrap();
        assert_eq!(store.active_vpn().unwrap().as_deref(), Some("b"), "activating one replaces the other");
        assert_eq!(store.db.query_row::<i64, _, _>("SELECT COUNT(*) FROM vpn_active", [], |r| r.get(0)).unwrap(), 1);
        assert!(store.set_active_vpn(Some("missing")).is_err(), "an unknown profile cannot be activated");
        assert_eq!(store.active_vpn().unwrap().as_deref(), Some("b"), "a refused activation changes nothing");
        store.remove_vpn("a").unwrap();
        assert_eq!(store.active_vpn().unwrap().as_deref(), Some("b"), "removing another profile keeps the active one");
        store.remove_vpn("b").unwrap();
        assert_eq!(store.active_vpn().unwrap(), None, "removing the active profile deactivates it");
    }

    #[cfg(windows)]
    #[test]
    fn editing_from_the_page_keeps_a_stored_secret_when_it_is_sent_empty() {
        let store = memory();
        store.save_vpn_edit(vpn_profile("HY2")).unwrap();
        let mut edit = vpn_profile("HY2");
        edit.fields.insert("server".into(), "other.example.com:443".into());
        edit.secrets.insert("auth".into(), String::new());
        store.save_vpn_edit(edit).unwrap();
        let saved = &store.vpn_profiles().unwrap()[0];
        assert_eq!(saved.fields["server"], "other.example.com:443");
        assert_eq!(saved.secrets["auth"], "s3cret-password", "an empty secret keeps the stored one");
        let mut changed = vpn_profile("HY2");
        changed.secrets.insert("auth".into(), "new-password".into());
        store.save_vpn_edit(changed).unwrap();
        assert_eq!(store.vpn_profiles().unwrap()[0].secrets["auth"], "new-password", "a typed secret replaces it");
        let mut fresh = vpn_profile("Novo");
        fresh.secrets.insert("auth".into(), String::new());
        assert!(store.save_vpn_edit(fresh).is_err(), "a new profile cannot be created without its required secret");
    }

    #[test]
    fn udp_redundancy_persists_and_defaults_cleanly() {
        let store = memory();
        assert_eq!(store.redundancy().unwrap(), crate::relay::RedundancyConfig { enabled: false, x: 20, y: 10 });
        let cfg = crate::relay::RedundancyConfig { enabled: true, x: 5, y: 3 };
        store.save_redundancy(cfg).unwrap();
        assert_eq!(store.redundancy().unwrap(), cfg);
        let updated = crate::relay::RedundancyConfig { enabled: false, x: 1, y: 2 };
        store.save_redundancy(updated).unwrap();
        assert_eq!(store.redundancy().unwrap(), updated);
    }

    #[test]
    fn udp_redundancy_rejects_invalid_and_preserves_previous() {
        let store = memory();
        let valid = crate::relay::RedundancyConfig { enabled: true, x: 5, y: 3 };
        store.save_redundancy(valid).unwrap();
        assert!(store.save_redundancy(crate::relay::RedundancyConfig { enabled: true, x: 0, y: 3 }).is_err());
        assert_eq!(store.redundancy().unwrap(), valid);
        store.db.execute("UPDATE udp_redundancy SET x = 0 WHERE id = 1", []).unwrap();
        assert!(store.redundancy().is_err());
    }

    #[test]
    fn auto_echo_lifecycle_validation_and_corruption_handling() {
        let store = memory();
        assert_eq!(store.auto_echo().unwrap(), None);
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        store.save_auto_echo(Some(addr)).unwrap();
        assert_eq!(store.auto_echo().unwrap(), Some(addr));

        // Non-loopback rejected and previous value retained
        let public_addr: std::net::SocketAddr = "1.1.1.1:4000".parse().unwrap();
        assert!(store.save_auto_echo(Some(public_addr)).is_err());
        assert_eq!(store.auto_echo().unwrap(), Some(addr));

        // None clears the echo
        store.save_auto_echo(None).unwrap();
        assert_eq!(store.auto_echo().unwrap(), None);

        // Corrupted rows surface as errors instead of silent None
        store.db.execute("INSERT INTO udp_redundancy_auto (id, echo) VALUES (1, 'invalid-addr')", []).unwrap();
        assert!(store.auto_echo().is_err());
        store.db.execute("UPDATE udp_redundancy_auto SET echo = '8.8.8.8:53' WHERE id = 1", []).unwrap();
        assert!(store.auto_echo().is_err());
    }
}
