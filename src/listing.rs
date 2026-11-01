//! Turning the archive's flat entry list into what the window shows.
use crate::engine::Entry;
use chrono::{Local, TimeZone};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub is_folder: bool,
    /// Entry name for a file; `a/b/` prefix for a folder.
    pub path: String,
    pub size: u64,
    pub packed: u64,
    pub modified_ms: Option<u64>,
    pub encrypted: bool,
    pub link: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    Name,
    Size,
    Packed,
    Modified,
}

/// Folders first, then by the chosen column. Names break ties, ignoring case.
pub fn sort_rows(rows: &mut [Row], by: SortBy, ascending: bool) {
    rows.sort_by(|a, b| {
        let primary = match by {
            SortBy::Name => Ordering::Equal,
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Packed => a.packed.cmp(&b.packed),
            SortBy::Modified => a.modified_ms.cmp(&b.modified_ms),
        };
        let order = primary.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        b.is_folder
            .cmp(&a.is_folder)
            .then(if ascending { order } else { order.reverse() })
    });
}

fn file_row(entry: &Entry, name: String) -> Row {
    Row {
        name,
        is_folder: false,
        path: entry.name.clone(),
        size: entry.unpacked_size,
        packed: entry.packed_size,
        modified_ms: entry.modified_ms,
        encrypted: entry.is_encrypted,
        link: entry.is_link,
    }
}

/// The rows directly inside `folder`, which is "" or ends in '/'.
pub fn rows_in(entries: &[Entry], folder: &str) -> Vec<Row> {
    let mut folders: HashMap<&str, Row> = HashMap::new();
    let mut rows = Vec::new();
    for entry in entries {
        let Some(rest) = entry.name.strip_prefix(folder) else {
            continue;
        };
        let rest = rest.trim_start_matches('/');
        match rest.split_once('/') {
            None if rest.is_empty() => {}
            None if !entry.is_directory => rows.push(file_row(entry, rest.to_owned())),
            split => {
                let name = split.map_or(rest, |(name, _)| name);
                let row = folders.entry(name).or_insert_with(|| Row {
                    name: name.to_owned(),
                    is_folder: true,
                    path: format!("{folder}{name}/"),
                    size: 0,
                    packed: 0,
                    modified_ms: None,
                    encrypted: false,
                    link: false,
                });
                if entry.is_directory {
                    row.modified_ms = row.modified_ms.or(entry.modified_ms);
                } else {
                    row.size += entry.unpacked_size;
                    row.packed += entry.packed_size;
                }
            }
        }
    }
    rows.extend(folders.into_values());
    rows
}

/// Every file in the archive whose path contains `query`, ignoring case.
pub fn search(entries: &[Entry], query: &str) -> Vec<Row> {
    let query = query.to_lowercase();
    entries
        .iter()
        .filter(|entry| !entry.is_directory && entry.name.to_lowercase().contains(&query))
        .map(|entry| file_row(entry, entry.name.clone()))
        .collect()
}

pub fn parent(folder: &str) -> &str {
    let trimmed = folder.strip_suffix('/').unwrap_or(folder);
    trimmed.rfind('/').map_or("", |slash| &trimmed[..=slash])
}

/// Every entry name a set of chosen rows covers, including inside folders.
pub fn expand_selection<'a>(
    entries: &[Entry],
    rows: impl Iterator<Item = &'a Row>,
) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut prefixes = Vec::new();
    for row in rows {
        if row.is_folder {
            prefixes.push(row.path.as_str());
        } else {
            names.insert(row.path.clone());
        }
    }
    for entry in entries {
        let as_folder = format!("{}/", entry.name.trim_end_matches('/'));
        if prefixes
            .iter()
            .any(|prefix| entry.name.starts_with(prefix) || as_folder == *prefix)
        {
            names.insert(entry.name.clone());
        }
    }
    names
}

/// The icon tint for a file: 0 generic, 1 image, 2 audio, 3 video, 4 archive, 5 code, 6 program.
pub fn kind_of(name: &str) -> i32 {
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tif" | "tiff" | "heic"
        | "ico" => 1,
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => 2,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "m4v" => 3,
        "rar" | "zip" | "7z" | "gz" | "tar" | "xz" | "bz2" | "iso" | "cab" => 4,
        "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "cs" | "java" | "go" | "html" | "css"
        | "json" | "xml" | "toml" | "yml" | "yaml" | "sh" | "ps1" => 5,
        "exe" | "msi" | "bat" | "cmd" | "com" | "scr" | "dll" | "vbs" | "lnk" => 6,
        _ => 0,
    }
}

/// Files Windows would run rather than display, so opening one deserves a warning.
pub fn runs_code(name: &str) -> bool {
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "exe" | "msi" | "bat" | "cmd" | "com" | "scr" | "vbs" | "vbe" | "js" | "jse" | "wsf"
            | "wsh" | "ps1" | "lnk" | "pif" | "hta" | "cpl" | "reg" | "jar"
    )
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "";
    for next in ["KB", "MB", "GB", "TB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

pub fn format_date(ms: Option<u64>) -> String {
    ms.and_then(|ms| Local.timestamp_millis_opt(ms as i64).single())
        .map(|time| time.format("%Y-%m-%d  %H:%M").to_string())
        .unwrap_or_default()
}

pub fn format_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn strip_part(stem: &str) -> &str {
    let lower = stem.to_ascii_lowercase();
    match lower.rfind(".part") {
        Some(pos)
            if lower.len() > pos + 5 && lower[pos + 5..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            &stem[..pos]
        }
        _ => stem,
    }
}

/// The folder next to the archive, named after it without any `.partN`.
pub fn default_destination(archive: &str) -> String {
    let split = archive.rfind(['\\', '/']).map_or(0, |slash| slash + 1);
    let (dir, file) = archive.split_at(split);
    let stem = strip_part(file.rfind('.').map_or(file, |dot| &file[..dot]));
    format!("{dir}{}", if stem.is_empty() { "extracted" } else { stem })
}

/// `name.repaired.rar` beside the original.
pub fn repaired_name(archive: &str) -> String {
    match archive.rfind('.') {
        Some(dot) if !archive[dot..].contains(['\\', '/']) => {
            format!("{}.repaired{}", &archive[..dot], &archive[dot..])
        }
        _ => format!("{archive}.repaired.rar"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_directory: bool, size: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            is_directory,
            is_link: false,
            is_encrypted: false,
            packed_size: size / 2,
            unpacked_size: size,
            modified_ms: None,
        }
    }

    fn sorted_names(mut rows: Vec<Row>, by: SortBy, ascending: bool) -> Vec<String> {
        sort_rows(&mut rows, by, ascending);
        rows.into_iter().map(|row| row.name).collect()
    }

    #[test]
    fn folders_come_first_with_totals() {
        let entries = [
            entry("z.txt", false, 1),
            entry("docs/a.txt", false, 10),
            entry("docs/deep/b.txt", false, 20),
            entry("empty", true, 0),
        ];
        let names = sorted_names(rows_in(&entries, ""), SortBy::Name, true);
        assert_eq!(names, ["docs", "empty", "z.txt"]);
        let docs = rows_in(&entries, "")
            .into_iter()
            .find(|row| row.name == "docs")
            .unwrap();
        assert_eq!(docs.size, 30);
        let names = sorted_names(rows_in(&entries, "docs/"), SortBy::Name, true);
        assert_eq!(names, ["deep", "a.txt"]);
        assert_eq!(parent("docs/deep/"), "docs/");
        assert_eq!(parent("docs/"), "");
    }

    #[test]
    fn sorting_keeps_folders_first_in_both_directions() {
        let entries = [
            entry("big.bin", false, 900),
            entry("small.bin", false, 2),
            entry("folder/x", false, 1),
        ];
        assert_eq!(
            sorted_names(rows_in(&entries, ""), SortBy::Size, false),
            ["folder", "big.bin", "small.bin"]
        );
        assert_eq!(
            sorted_names(rows_in(&entries, ""), SortBy::Size, true),
            ["folder", "small.bin", "big.bin"]
        );
    }

    #[test]
    fn search_finds_files_anywhere() {
        let entries = [
            entry("docs/Report.txt", false, 1),
            entry("report", true, 0),
            entry("other.txt", false, 1),
        ];
        let rows = search(&entries, "repo");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "docs/Report.txt");
    }

    #[test]
    fn selecting_a_folder_covers_its_contents_only() {
        let entries = [
            entry("docs", true, 0),
            entry("docs/a.txt", false, 1),
            entry("docs2/b.txt", false, 1),
            entry("c.txt", false, 1),
        ];
        let rows = rows_in(&entries, "");
        let folder = rows.iter().find(|r| r.name == "docs").unwrap();
        let chosen = expand_selection(&entries, std::iter::once(folder));
        assert_eq!(
            chosen,
            HashSet::from(["docs".to_owned(), "docs/a.txt".to_owned()])
        );
    }

    #[test]
    fn names_and_formats() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(50 * 1024 * 1024), "50 MB");
        assert_eq!(format_duration(75), "1m 15s");
        assert_eq!(default_destination(r"C:\x\film.part01.rar"), r"C:\x\film");
        assert_eq!(default_destination(r"C:\x\a.part.rar"), r"C:\x\a.part");
        assert_eq!(repaired_name(r"C:\x\a.rar"), r"C:\x\a.repaired.rar");
        assert!(runs_code("SETUP.EXE"));
        assert!(!runs_code("notes.txt"));
        assert_eq!(kind_of("a.PNG"), 1);
    }
}
