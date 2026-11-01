//! Explorer integration and settings, all per-user under HKCU. Nothing here needs admin.
use std::io;
use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ};
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
const CLASSES: &str = key!("Classes");
const CAPABILITIES: &str = key!(r"rawr\Capabilities");
const APPLICATION: &str = key!(r"Classes\Applications\rawr.exe");
const REGISTERED_APPLICATIONS: &str = key!("RegisteredApplications");
const EXTENSIONS: [&str; 5] = [".rar", ".zip", ".7z", ".gz", ".tgz"];
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
    exists(CAPABILITIES)
}

fn extension_key(extension: &str) -> String {
    format!(r"{CLASSES}\{extension}")
}

/// Tells Explorer to pick up the change without a restart.
fn notify_shell() {
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            item1: *const core::ffi::c_void,
            item2: *const core::ffi::c_void,
        );
    }
    // SAFETY: this event takes no item pointers, and null is what it documents.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, 0, std::ptr::null(), std::ptr::null()) };
}

/// Lists rawr as a handler in "Open with" and Default apps. The user's current default
/// is left alone: Windows reserves that choice for the user, so only they can change it.
pub fn set_association(enabled: bool) -> io::Result<()> {
    // 0.3.0 overwrote the .rar default, which no longer belongs to rawr.
    if let Ok(extension) = hkcu().open_subkey_with_flags(RAR_EXTENSION, KEY_ALL_ACCESS) {
        if extension.get_value::<String, _>("").is_ok_and(|value| value == "rawr.archive") {
            extension.delete_value("")?;
        }
    }
    if enabled {
        let exe = exe()?;
        let open = format!("\"{exe}\" \"%1\"");
        let (class, _) = hkcu().create_subkey(CLASS)?;
        class.set_value("", &"Archive")?;
        class
            .create_subkey("DefaultIcon")?
            .0
            .set_value("", &format!("\"{exe}\",0"))?;
        class
            .create_subkey(r"shell\open\command")?
            .0
            .set_value("", &open)?;

        let (application, _) = hkcu().create_subkey(APPLICATION)?;
        application.set_value("FriendlyAppName", &"rawr")?;
        application
            .create_subkey(r"shell\open\command")?
            .0
            .set_value("", &open)?;
        let (supported, _) = application.create_subkey("SupportedTypes")?;

        let (capabilities, _) = hkcu().create_subkey(CAPABILITIES)?;
        capabilities.set_value("ApplicationName", &"rawr")?;
        capabilities.set_value(
            "ApplicationDescription",
            &"Open, test, extract and create archives",
        )?;
        capabilities.set_value("ApplicationIcon", &format!("\"{exe}\",0"))?;
        let (associations, _) = capabilities.create_subkey("FileAssociations")?;
        for extension in EXTENSIONS {
            associations.set_value(extension, &"rawr.archive")?;
            supported.set_value(extension, &"")?;
            hkcu()
                .create_subkey(format!(r"{}\OpenWithProgids", extension_key(extension)))?
                .0
                .set_value("rawr.archive", &"")?;
        }
        hkcu()
            .create_subkey(REGISTERED_APPLICATIONS)?
            .0
            .set_value("rawr", &CAPABILITIES)?;
    } else {
        if let Ok(registered) = hkcu().create_subkey(REGISTERED_APPLICATIONS) {
            let _ = registered.0.delete_value("rawr");
        }
        for extension in EXTENSIONS {
            let path = format!(r"{}\OpenWithProgids", extension_key(extension));
            if let Ok(progids) = hkcu().open_subkey_with_flags(path, KEY_ALL_ACCESS) {
                let _ = progids.delete_value("rawr.archive");
            }
        }
        remove(CAPABILITIES)?;
        remove(APPLICATION)?;
        remove(CLASS)?;
    }
    if !cfg!(test) {
        notify_shell();
    }
    Ok(())
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
    // Our own marker goes last: while it exists, no other copy can think it's first.
    for name in names.iter().rev() {
        let file = dir.join(name);
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
        // The user's default stays theirs.
        assert_eq!(value(RAR_EXTENSION, ""), "WinRAR");
        hkcu()
            .open_subkey_with_flags(RAR_EXTENSION, KEY_ALL_ACCESS)
            .unwrap()
            .set_value("", &"rawr.archive")
            .unwrap();
        set_association(true).unwrap();
        assert_eq!(value(RAR_EXTENSION, ""), "");
        hkcu()
            .open_subkey_with_flags(RAR_EXTENSION, KEY_ALL_ACCESS)
            .unwrap()
            .set_value("", &"WinRAR")
            .unwrap();
        let open = format!(r"{CLASS}\shell\open\command");
        assert!(value(&open, "").ends_with(r#"" "%1""#));
        assert_eq!(value(REGISTERED_APPLICATIONS, "rawr"), CAPABILITIES);
        let associations = format!(r"{CAPABILITIES}\FileAssociations");
        for extension in EXTENSIONS {
            assert_eq!(value(&associations, extension), "rawr.archive");
            let progids = format!(r"{}\OpenWithProgids", extension_key(extension));
            assert!(hkcu()
                .open_subkey(&progids)
                .unwrap()
                .get_raw_value("rawr.archive")
                .is_ok());
        }
        set_association(true).unwrap();
        set_association(false).unwrap();
        assert!(!association_enabled());
        assert_eq!(value(RAR_EXTENSION, ""), "WinRAR");
        assert_eq!(value(REGISTERED_APPLICATIONS, "rawr"), "");
        assert!(!exists(CLASS) && !exists(APPLICATION));

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