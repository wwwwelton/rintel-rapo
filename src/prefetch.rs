use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

fn warm_file(
    path: &Path,
    limit: u64,
    buffer: &mut [u8],
    game_running: &impl Fn() -> Result<bool, String>,
) -> Result<u64, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = 0;
    let mut since_check = 0;
    while bytes < limit {
        if since_check == 0 && game_running()? {
            return Err("AION 2 em execução; prefetch interrompido".into());
        }
        let chunk = (limit - bytes).min(buffer.len() as u64) as usize;
        let n = file
            .read(&mut buffer[..chunk])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        since_check += n;
        if since_check >= 1024 * 1024 {
            since_check = 0;
        }
    }
    Ok(bytes)
}

pub fn prefetch_aion2(
    root: &Path,
    game_running: impl Fn() -> Result<bool, String>,
) -> Result<u64, String> {
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
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = warm_file(&exe, 4 * 1024 * 1024, &mut buffer, &game_running)?;
    for entry in fs::read_dir(&paks).map_err(|e| format!("{}: {e}", paks.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", paks.display()))?;
        let path = entry.path();
        if !entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?.is_file()
            || !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("utoc"))
        {
            continue;
        }
        let path = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
        if !path.starts_with(&paks) {
            continue;
        }
        bytes += warm_file(&path, (32 * 1024 * 1024 - bytes).min(8 * 1024 * 1024), &mut buffer, &game_running)?;
        if bytes >= 32 * 1024 * 1024 {
            break;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn warms_only_game_binary_and_iostore_indices_without_changing_files() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-{}", std::process::id()));
        let game = root.join("Aion2");
        let binary = game.join("Binaries/Win64/AION2.exe");
        let paks = game.join("Content/Paks");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&paks).unwrap();
        fs::write(&binary, b"binary").unwrap();
        fs::write(paks.join("global.utoc"), b"index").unwrap();
        fs::write(paks.join("global.ucas"), b"bulk data").unwrap();

        let bytes = prefetch_aion2(&root, || Ok(false)).unwrap();
        assert_eq!(bytes, 11);
        assert_eq!(fs::read(&binary).unwrap(), b"binary");
        assert_eq!(fs::read(paks.join("global.utoc")).unwrap(), b"index");
        assert_eq!(fs::read(paks.join("global.ucas")).unwrap(), b"bulk data");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stops_immediately_when_game_is_running() {
        let root = std::env::temp_dir().join(format!("rapo-prefetch-stop-{}", std::process::id()));
        let binary = root.join("Binaries/Win64/AION2.exe");
        let paks = root.join("Content/Paks");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir_all(&paks).unwrap();
        fs::write(binary, b"binary").unwrap();
        assert!(prefetch_aion2(&root, || Ok(true)).is_err());
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

        assert_eq!(prefetch_aion2(&root, || Ok(false)).unwrap(), 6);
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
