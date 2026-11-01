//! Explorer integration and settings, all per-user under HKCU. Nothing here needs admin.
use std::io;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

// Tests work in a private key, so they can't disturb the real .rar settings.
#[cfg(not(test))]
macro_rules! key {
    ($path:literal) => {
        concat!(r"Software\", $path)
    };
}
#[cfg(test)]
macro_rules! key {
    ($path:literal) => {
        concat!(r"Software\rawr-test\", $path)
    };
}

const SETTINGS: &str = key!("rawr");
const CLASS: &str = key!(r"Classes\rawr.archive");
const RAR_EXTENSION: &str = key!(r"Classes\.rar");
const EXTRACT_VERB: &str = key!(r"Classes\SystemFileAssociations\.rar\shell\rawr.extract");
const ADD_FILE_VERB: &str = key!(r"Classes\*\shell\rawr.add");
const ADD_FOLDER_VERB: &str = key!(r"Classes\Directory\shell\rawr.add");

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

fn exe() -> io::Result<String> {
    Ok(std::env::current_exe()?.display().to_string())
}

fn exists(path: &str) -> bool {
    hkcu().open_subkey_with_flags(path, KEY_READ).is_ok()
}

fn remove(path: &str) -> io::Result<()> {
    match hkcu().delete_subkey_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn add_verb(path: &str, label: &str, arguments: &str) -> io::Result<()> {
    let exe = exe()?;
    let (verb, _) = hkcu().create_subkey(path)?;
    verb.set_value("", &label)?;
    verb.set_value("Icon", &exe)?;
    let (command, _) = verb.create_subkey("command")?;
    command.set_value("", &format!("\"{exe}\" {arguments}"))
}

pub fn context_menu_enabled() -> bool {
    exists(EXTRACT_VERB)
}

pub fn set_context_menu(enabled: bool) -> io::Result<()> {
    if enabled {
        add_verb(EXTRACT_VERB, "Extract here", "--extract-here \"%1\"")?;
        add_verb(ADD_FILE_VERB, "Add to archive", "--add \"%1\"")?;
        add_verb(ADD_FOLDER_VERB, "Add to archive", "--add \"%1\"")
    } else {
        remove(EXTRACT_VERB)?;
        remove(ADD_FILE_VERB)?;
        remove(ADD_FOLDER_VERB)
    }
}

pub fn association_enabled() -> bool {
    exists(CLASS)
}

/// Offers rawr as a handler for .rar. Windows keeps the user's chosen default, so
/// taking over from another app still needs "Open with" once.
pub fn set_association(enabled: bool) -> io::Result<()> {
    if enabled {
        let exe = exe()?;
        let (class, _) = hkcu().create_subkey(CLASS)?;
        class.set_value("", &"RAR archive")?;
        class
            .create_subkey("DefaultIcon")?
            .0
            .set_value("", &format!("\"{exe}\",0"))?;
        class
            .create_subkey(r"shell\open\command")?
            .0
            .set_value("", &format!("\"{exe}\" \"%1\""))?;
        let (extension, _) = hkcu().create_subkey(RAR_EXTENSION)?;
        // Keep whatever was there, so turning this off can put it back.
        let previous: String = extension.get_value("").unwrap_or_default();
        if previous != "rawr.archive" {
            class.set_value("Previous", &previous)?;
        }
        extension.set_value("", &"rawr.archive")?;
        extension
            .create_subkey("OpenWithProgids")?
            .0
            .set_value("rawr.archive", &"")
    } else {
        let previous: String = hkcu()
            .open_subkey(CLASS)
            .and_then(|class| class.get_value("Previous"))
            .unwrap_or_default();
        if let Ok((extension, _)) = hkcu().create_subkey(RAR_EXTENSION) {
            let current: String = extension.get_value("").unwrap_or_default();
            if current == "rawr.archive" {
                extension.set_value("", &previous)?;
            }
            if let Ok(progids) = extension.create_subkey("OpenWithProgids") {
                let _ = progids.0.delete_value("rawr.archive");
            }
        }
        remove(CLASS)
    }
}

/// 0 follows Windows, 1 light, 2 dark.
pub fn theme() -> i32 {
    hkcu()
        .open_subkey(SETTINGS)
        .and_then(|key| key.get_value::<u32, _>("Theme"))
        .map_or(0, |value| value.min(2) as i32)
}

pub fn set_theme(choice: i32) {
    if let Ok((key, _)) = hkcu().create_subkey(SETTINGS) {
        let _ = key.set_value("Theme", &(choice.max(0) as u32));
    }
}

/// The index into the encodings Settings offers.
pub fn name_encoding() -> usize {
    hkcu()
        .open_subkey(SETTINGS)
        .and_then(|key| key.get_value::<u32, _>("NameEncoding"))
        .map_or(0, |value| value as usize)
}

pub fn set_name_encoding(choice: usize) {
    if let Ok((key, _)) = hkcu().create_subkey(SETTINGS) {
        let _ = key.set_value("NameEncoding", &(choice as u32));
    }
}

/// Explorer starts one rawr per selected file. Each leaves its path in `dir` and
/// waits briefly. The earliest then takes every path and the rest quietly exit.
pub fn gather_paths(
    dir: &std::path::Path,
    path: &std::path::Path,
    wait: std::time::Duration,
) -> io::Result<Option<Vec<std::path::PathBuf>>> {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    const STALE_MS: u128 = 60_000;

    std::fs::create_dir_all(dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let mine = format!(
        "{now:020}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::write(dir.join(&mine), path.to_string_lossy().as_bytes())?;
    std::thread::sleep(wait);

    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stamp: u128 = name.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        if now.saturating_sub(stamp) > STALE_MS {
            let _ = std::fs::remove_file(entry.path());
        } else {
            names.push(name);
        }
    }
    names.sort();
    if names.first() != Some(&mine) {
        return Ok(None);
    }
    let mut paths = Vec::new();
    for name in names {
        let file = dir.join(&name);
        paths.push(std::path::PathBuf::from(std::fs::read_to_string(&file)?));
        let _ = std::fs::remove_file(file);
    }
    paths.sort();
    paths.dedup();
    Ok(Some(paths))
}

/// For the uninstaller: everything rawr added to the registry, restoring .rar.
pub fn remove_everything() {
    let _ = set_context_menu(false);
    if association_enabled() {
        let _ = set_association(false);
    }
    let _ = remove(SETTINGS);
}

const RECENT_LIMIT: usize = 8;

/// Most recent first. Archives that no longer exist are left out.
pub fn recent() -> Vec<String> {
    let stored: String = hkcu()
        .open_subkey(SETTINGS)
        .and_then(|key| key.get_value("Recent"))
        .unwrap_or_default();
    stored
        .lines()
        .filter(|path| std::path::Path::new(path).exists())
        .map(str::to_owned)
        .collect()
}

pub fn remember(path: &str) {
    let mut paths = recent();
    paths.retain(|known| !known.eq_ignore_ascii_case(path));
    paths.insert(0, path.to_owned());
    paths.truncate(RECENT_LIMIT);
    store_recent(&paths);
}

pub fn clear_recent() {
    store_recent(&[]);
}

fn store_recent(paths: &[String]) {
    if let Ok((key, _)) = hkcu().create_subkey(SETTINGS) {
        let _ = key.set_value("Recent", &paths.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_launches_merge_into_one() {
        let dir = std::env::temp_dir().join(format!("rawr-gather-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let workers: Vec<_> = (0..6)
            .map(|index| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let path = std::path::PathBuf::from(format!("file{index}"));
                    gather_paths(&dir, &path, std::time::Duration::from_millis(300)).unwrap()
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let winners: Vec<_> = results.into_iter().flatten().collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].len(), 6);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn value(path: &str, name: &str) -> String {
        hkcu()
            .open_subkey(path)
            .and_then(|key| key.get_value(name))
            .unwrap_or_default()
    }

    #[test]
    fn toggles_register_and_restore() {
        let _ = hkcu().delete_subkey_all(r"Software\rawr-test");
        hkcu()
            .create_subkey(RAR_EXTENSION)
            .unwrap()
            .0
            .set_value("", &"WinRAR")
            .unwrap();

        set_association(true).unwrap();
        assert!(association_enabled());
        assert_eq!(value(RAR_EXTENSION, ""), "rawr.archive");
        let open = format!(r"{CLASS}\shell\open\command");
        assert!(value(&open, "").ends_with(r#"" "%1""#));
        set_association(true).unwrap();
        set_association(false).unwrap();
        assert!(!association_enabled());
        assert_eq!(value(RAR_EXTENSION, ""), "WinRAR");

        set_context_menu(true).unwrap();
        assert!(context_menu_enabled());
        let command = format!(r"{EXTRACT_VERB}\command");
        assert!(value(&command, "").contains(r#"--extract-here "%1""#));
        assert!(value(&format!(r"{ADD_FOLDER_VERB}\command"), "").contains(r#"--add "%1""#));
        set_context_menu(false).unwrap();
        assert!(!context_menu_enabled());
        assert!(!exists(ADD_FILE_VERB));

        set_theme(2);
        assert_eq!(theme(), 2);
        set_context_menu(true).unwrap();
        set_association(true).unwrap();
        remove_everything();
        assert!(!context_menu_enabled() && !association_enabled() && !exists(SETTINGS));
        assert_eq!(value(RAR_EXTENSION, ""), "WinRAR");

        let dir = std::env::temp_dir().join(format!("rawr-recent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<String> = (0..10)
            .map(|index| {
                let path = dir.join(format!("{index}.rar"));
                std::fs::write(&path, b"x").unwrap();
                path.display().to_string()
            })
            .collect();
        paths.iter().for_each(|path| remember(path));
        assert_eq!(recent().len(), RECENT_LIMIT);
        assert_eq!(recent()[0], paths[9]);
        remember(&paths[5]);
        assert_eq!(recent()[0], paths[5]);
        assert_eq!(recent().iter().filter(|p| **p == paths[5]).count(), 1);
        std::fs::remove_file(&paths[5]).unwrap();
        assert!(!recent().contains(&paths[5]));
        clear_recent();
        assert!(recent().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
        hkcu().delete_subkey_all(r"Software\rawr-test").unwrap();
    }
}