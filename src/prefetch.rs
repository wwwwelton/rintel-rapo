use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const CHUNK: usize = 1024 * 1024;
const MIN_PREFETCH_BUDGET: u64 = 512 * 1024 * 1024;
const MAX_PREFETCH_BUDGET: u64 = 4 * 1024 * 1024 * 1024;

#[cfg(windows)]
#[repr(C)]
struct MemoryStatusEx {
    dw_length: u32,
    dw_memory_load: u32,
    ull_total_phys: u64,
    ull_avail_phys: u64,
    ull_total_page_file: u64,
    ull_avail_page_file: u64,
    ull_total_virtual: u64,
    ull_avail_virtual: u64,
    ull_avail_extended_virtual: u64,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
}

fn budget_for_available_ram(available: u64) -> u64 {
    (available / 4).clamp(MIN_PREFETCH_BUDGET, MAX_PREFETCH_BUDGET)
}

#[cfg(windows)]
fn dynamic_budget() -> u64 {
    #[cfg(windows)]
    {
        let mut status = MemoryStatusEx {
            dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
            dw_memory_load: 0,
            ull_total_phys: 0,
            ull_avail_phys: 0,
            ull_total_page_file: 0,
            ull_avail_page_file: 0,
            ull_total_virtual: 0,
            ull_avail_virtual: 0,
            ull_avail_extended_virtual: 0,
        };
        unsafe {
            if GlobalMemoryStatusEx(&mut status) == 0 {
                return 0;
            }
        }
        budget_for_available_ram(status.ull_avail_phys)
    }
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefetchControl {
    Continue,
    #[allow(dead_code)]
    Throttle,
    Stop,
}

#[derive(Debug, Default)]
pub struct RamCache {
    bytes: u64,
    files: usize,
}

impl RamCache {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    #[allow(dead_code)]
    pub fn files(&self) -> usize {
        self.files
    }
}

// Reads through the Windows file cache; the OS may evict those pages under pressure.
fn warm_file(
    path: &Path,
    total_read: u64,
    budget: &impl Fn() -> u64,
    buffer: &mut [u8],
    control: &impl Fn() -> PrefetchControl,
) -> Result<(u64, bool), String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = 0;
    loop {
        match control() {
            PrefetchControl::Stop => return Ok((bytes, true)),
            PrefetchControl::Throttle => std::thread::sleep(Duration::from_millis(5)),
            PrefetchControl::Continue => {}
        }
        let remaining = budget().saturating_sub(total_read.saturating_add(bytes));
        if remaining == 0 {
            break;
        }
        let chunk = remaining.min(buffer.len() as u64) as usize;
        let n = file
            .read(&mut buffer[..chunk])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
    }
    Ok((bytes, false))
}

fn collect(dir: &Path, root: &Path, utoc: &mut Vec<PathBuf>, ucas: &mut Vec<(u64, PathBuf)>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let path = entry.path();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect(&path, root, utoc, ucas)?;
        } else if kind.is_file() {
            let ext = path.extension().map(|e| e.to_ascii_lowercase());
            let canonical = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
            if !canonical.starts_with(root) {
                continue;
            }
            match ext.as_deref().and_then(|e| e.to_str()) {
                Some("utoc") => utoc.push(canonical),
                Some("ucas") => {
                    let size = entry.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len();
                    ucas.push((size, canonical));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn prefetch_with_budget(
    root: &Path,
    budget: impl Fn() -> u64,
    control: impl Fn() -> PrefetchControl,
) -> Result<RamCache, String> {
    let root = root.canonicalize().map_err(|e| format!("Pasta do AION 2: {e}"))?;
    let game = if root.join("Aion2/Binaries/Win64/AION2.exe").is_file() {
        root.join("Aion2")
    } else {
        root.clone()
    };
    let exe = game
        .join("Binaries/Win64/AION2.exe")
        .canonicalize()
        .map_err(|e| format!("Executável AION2.exe não encontrado: {e}"))?;
    let paks = game
        .join("Content/Paks")
        .canonicalize()
        .map_err(|e| format!("Pasta Content/Paks não encontrada: {e}"))?;
    if !exe.starts_with(&root) || !paks.starts_with(&root) || !paks.is_dir() {
        return Err("Arquivos do jogo fora da pasta selecionada".into());
    }
    let (mut utoc, mut ucas) = (Vec::new(), Vec::new());
    collect(&paks, &paks, &mut utoc, &mut ucas)?;
    ucas.sort();
    let mut buffer = vec![0u8; CHUNK];
    let mut cache = RamCache::default();
    let order = std::iter::once(exe).chain(utoc).chain(ucas.into_iter().map(|(_, p)| p));
    for path in order {
        if cache.bytes >= budget() {
            break;
        }
        let (read, stopped) = warm_file(&path, cache.bytes, &budget, &mut buffer, &control)?;
        if read > 0 {
            cache.bytes += read;
            cache.files += 1;
        }
        if stopped {
            break;
        }
    }
    Ok(cache)
}

#[allow(dead_code)]
pub fn prefetch_aion2_ram(
    root: &Path,
    budget: u64,
    control: impl Fn() -> PrefetchControl,
) -> Result<RamCache, String> {
    prefetch_with_budget(root, || budget, control)
}

/// Uses one quarter of currently available physical RAM, bounded to 512 MiB–4 GiB.
/// The budget is sampled before every read, so pressure can stop the worker early.
#[cfg(windows)]
pub fn prefetch_aion2_dynamic(
    root: &Path,
    control: impl Fn() -> PrefetchControl,
) -> Result<RamCache, String> {
    prefetch_with_budget(root, dynamic_budget, control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dynamic_budget_uses_quarter_of_available_ram_with_safe_bounds() {
        assert_eq!(budget_for_available_ram(256 * 1024 * 1024), MIN_PREFETCH_BUDGET);
        assert_eq!(budget_for_available_ram(8 * 1024 * 1024 * 1024), 2 * 1024 * 1024 * 1024);
        assert_eq!(budget_for_available_ram(32 * 1024 * 1024 * 1024), MAX_PREFETCH_BUDGET);
    }

    #[test]
    fn caches_full_disk_files_in_ram_and_scales_with_budget() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-{}", std::process::id()));
        let game = root.join("Aion2");
        let binary = game.join("Binaries/Win64/AION2.exe");
        let paks = game.join("Content/Paks");
        let l10n = paks.join("L10N");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&l10n).unwrap();
        fs::write(&binary, b"binary").unwrap();
        fs::write(paks.join("global.utoc"), b"index").unwrap();
        fs::write(l10n.join("voice.utoc"), b"subtoc").unwrap();
        fs::write(paks.join("chunk0.ucas"), b"0123456789").unwrap();

        let cache_small = prefetch_aion2_ram(&root, 17, || PrefetchControl::Continue).unwrap();
        assert_eq!(cache_small.bytes(), 17);
        assert_eq!(cache_small.files(), 3);

        let cache_large = prefetch_aion2_ram(&root, 64, || PrefetchControl::Throttle).unwrap();
        assert_eq!(cache_large.bytes(), 27);
        assert_eq!(cache_large.files(), 4);
        assert_eq!(fs::read(&binary).unwrap(), b"binary");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stops_dynamically_when_cancelled() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-stop-{}", std::process::id()));
        let binary = root.join("Binaries/Win64/AION2.exe");
        let paks = root.join("Content/Paks");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&paks).unwrap();
        fs::write(binary, b"binary").unwrap();
        let cache = prefetch_aion2_ram(&root, 64, || PrefetchControl::Stop).unwrap();
        assert_eq!(cache.bytes(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn dynamic_prefetch_reads_using_live_memory_budget() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-dynamic-{}", std::process::id()));
        let binary = root.join("Binaries/Win64/AION2.exe");
        let paks = root.join("Content/Paks");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&paks).unwrap();
        fs::write(binary, b"binary").unwrap();

        let cache = prefetch_aion2_dynamic(&root, || PrefetchControl::Continue).unwrap();
        assert_eq!(cache.bytes(), 6);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks_pointing_outside_game_directory() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-link-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("rapo-outside-{}.utoc", std::process::id()));
        let binary = root.join("Binaries/Win64/AION2.exe");
        let paks = root.join("Content/Paks");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&paks).unwrap();
        fs::write(binary, b"binary").unwrap();
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, paks.join("escape.utoc")).unwrap();

        assert_eq!(prefetch_aion2_ram(&root, 64, || PrefetchControl::Continue).unwrap().bytes(), 6);
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
