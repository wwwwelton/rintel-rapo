use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Game {
    pub name: String,
    pub directory: PathBuf,
    pub is_aion2: bool,
}

#[derive(Debug)]
enum Value {
    Text(String),
    Object(Vec<(String, Value)>),
}

fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('\\') => value.push('\\'),
                            Some('"') => value.push('"'),
                            Some(next) => {
                                value.push('\\');
                                value.push(next);
                            }
                            None => break,
                        },
                        _ => value.push(c),
                    }
                }
                out.push(value);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '{' | '}' => out.push(ch.to_string()),
            c if c.is_whitespace() => (),
            _ => {
                let mut word = ch.to_string();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' {
                        break;
                    }
                    word.push(chars.next().unwrap());
                }
                out.push(word);
            }
        }
    }
    out
}

fn parse(input: &str) -> Vec<(String, Value)> {
    fn object(items: &[String], index: &mut usize) -> Vec<(String, Value)> {
        let mut result = Vec::new();
        while *index < items.len() {
            let key = &items[*index];
            *index += 1;
            if key == "}" {
                break;
            }
            if *index == items.len() {
                break;
            }
            let value = if items[*index] == "{" {
                *index += 1;
                Value::Object(object(items, index))
            } else {
                let text = items[*index].clone();
                *index += 1;
                Value::Text(text)
            };
            result.push((key.clone(), value));
        }
        result
    }
    object(&tokens(input), &mut 0)
}

fn field<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn text<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    match field(entries, key)? {
        Value::Text(v) => Some(v),
        _ => None,
    }
}

fn object<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    match field(entries, key)? {
        Value::Object(v) => Some(v),
        _ => None,
    }
}

pub fn discover(steam_root: &Path) -> Vec<Game> {
    let mut libraries = vec![steam_root.to_path_buf()];
    let config = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(contents) = fs::read_to_string(config) {
        let root = parse(&contents);
        if let Some(folders) = object(&root, "libraryfolders") {
            for (key, entry) in folders {
                if !key.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let path = match entry {
                    Value::Text(v) => Some(v.as_str()),
                    Value::Object(v) => text(v, "path"),
                };
                if let Some(path) = path {
                    libraries.push(PathBuf::from(path));
                }
            }
        }
    }
    let mut games = Vec::new();
    for library in libraries {
        let apps = library.join("steamapps");
        let Ok(entries) = fs::read_dir(&apps) else {
            continue;
        };
        for entry in entries.flatten() {
            let filename = entry.file_name();
            let filename = filename.to_string_lossy();
            if !filename.starts_with("appmanifest_") || !filename.ends_with(".acf") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let root = parse(&contents);
            let Some(state) = object(&root, "AppState") else {
                continue;
            };
            let (Some(name), Some(folder)) = (text(state, "name"), text(state, "installdir"))
            else {
                continue;
            };
            // Manifest data is untrusted; only consider an actual directory directly below common.
            if folder.is_empty() || folder == "." || folder == ".." || folder.contains(['/', '\\'])
            {
                continue;
            }
            let directory = apps.join("common").join(folder);
            if directory.is_dir() && !games.iter().any(|g: &Game| g.directory == directory) {
                games.push(Game {
                    name: name.to_owned(),
                    is_aion2: filename == "appmanifest_3393110.acf",
                    directory,
                });
            }
        }
    }
    games.sort_by_cached_key(|game| game.name.to_lowercase());
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_and_legacy_libraries() {
        let root = parse(
            r#""libraryfolders" { "0" { "path" "C:\\Steam" "apps" { "42" "1" } } "1" "D:\\Games" }"#,
        );
        let entries = object(&root, "libraryfolders").unwrap();
        assert_eq!(
            text(object(entries, "0").unwrap(), "path"),
            Some("C:\\Steam")
        );
        assert!(matches!(field(entries, "1"), Some(Value::Text(v)) if v == "D:\\Games"));
    }

    #[test]
    fn discovers_only_existing_installed_games() {
        let root =
            std::env::temp_dir().join(format!("steam-optimizer-test-{}", std::process::id()));
        let apps = root.join("steamapps");
        fs::create_dir_all(apps.join("common/Real Game")).unwrap();
        fs::write(
            apps.join("appmanifest_42.acf"),
            r#""AppState" { "name" "Real Game" "installdir" "Real Game" }"#,
        )
        .unwrap();
        fs::create_dir_all(apps.join("common/AION 2")).unwrap();
        fs::write(
            apps.join("appmanifest_3393110.acf"),
            r#""AppState" { "name" "AION 2" "installdir" "AION 2" }"#,
        )
        .unwrap();
        fs::write(
            apps.join("appmanifest_43.acf"),
            r#""AppState" { "name" "Missing" "installdir" "Missing" }"#,
        )
        .unwrap();
        fs::write(
            apps.join("appmanifest_44.acf"),
            r#""AppState" { "name" "Escape" "installdir" ".." }"#,
        )
        .unwrap();
        let games = discover(&root);
        assert_eq!(games.len(), 2);
        assert!(games.iter().any(|game| game.is_aion2 && game.name == "AION 2"));
        assert!(games.iter().any(|game| !game.is_aion2 && game.name == "Real Game"));
        fs::remove_dir_all(root).unwrap();
    }
}
