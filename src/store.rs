//! SQLite store for every measured performance profile: CPU/core trials and best profile per game,
//! plus relay connect statistics per destination and authorized exit. Startup reads the best ones back.

use crate::learning::{Profile, Score, Settings};
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
CREATE TABLE IF NOT EXISTS relay_paths (
    destination TEXT NOT NULL, authorization TEXT NOT NULL, updated_at INTEGER NOT NULL,
    mean_ms REAL NOT NULL, samples INTEGER NOT NULL, failures INTEGER NOT NULL,
    PRIMARY KEY (destination, authorization));";

const SETTINGS: &str = "priority, pin, qos, threads, min_percent, cores, smt, p_only, park_cores";

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64) }
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

    /// One-time import of the registry profiles (formats 2-4); never overwrites a profile already in SQLite.
    pub fn import(&self, profiles: &[Profile]) -> Result<usize, String> {
        let known = self.best_profiles()?;
        let mut imported = 0;
        for profile in profiles.iter().filter(|p| !known.iter().any(|k| k.exe == p.exe)) {
            self.save_best(profile)?;
            imported += 1;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Store { Store::open(Path::new(":memory:")).unwrap() }
    fn profile(exe: &str, fps: f64) -> Profile {
        Profile { exe: exe.into(), settings: Settings { pin: true, cores: 6, ..Settings::default() }, score: Score { fps, low_fps: fps * 0.8 } }
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
}
