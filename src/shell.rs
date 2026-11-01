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

#[cfg(test)]
mod tests {
    use super::*;

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
        hkcu().delete_subkey_all(r"Software\rawr-test").unwrap();
    }
}