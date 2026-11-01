//! Turning the archive's flat entry list into folder-by-folder rows.
use crate::engine::Entry;
use chrono::{Local, TimeZone};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub is_folder: bool,
    pub is_up: bool,
    /// Entry name for a file; `name/` prefix for a folder.
    pub path: String,
    pub size: u64,
    pub packed: u64,
    pub modified_ms: Option<u64>,
    pub note: &'static str,
}

/// The rows directly inside `folder`, which is "" or ends in '/'.
pub fn rows_in(entries: &[Entry], folder: &str) -> Vec<Row> {
    let mut folders: HashMap<&str, Row> = HashMap::new();
    let mut files = Vec::new();
    for entry in entries {
        let Some(rest) = entry.name.strip_prefix(folder) else {
            continue;
        };
        let rest = rest.trim_start_matches('/');
        match rest.split_once('/') {
            None if rest.is_empty() => {}
            None if !entry.is_directory => files.push(Row {
                name: rest.to_owned(),
                is_folder: false,
                is_up: false,
                path: entry.name.clone(),
                size: entry.unpacked_size,
                packed: entry.packed_size,
                modified_ms: entry.modified_ms,
                note: match (entry.is_link, entry.is_encrypted) {
                    (true, _) => "link",
                    (_, true) => "encrypted",
                    _ => "",
                },
            }),
            split => {
                let name = split.map_or(rest, |(name, _)| name);
                let row = folders.entry(name).or_insert_with(|| Row {
                    name: name.to_owned(),
                    is_folder: true,
                    is_up: false,
                    path: format!("{folder}{name}/"),
                    size: 0,
                    packed: 0,
                    modified_ms: None,
                    note: "",
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
    let key = |row: &Row| row.name.to_lowercase();
    let mut folders: Vec<Row> = folders.into_values().collect();
    folders.sort_by_key(key);
    files.sort_by_key(key);
    let mut rows = Vec::with_capacity(folders.len() + files.len() + 1);
    if !folder.is_empty() {
        rows.push(Row {
            name: "..".to_owned(),
            is_folder: true,
            is_up: true,
            path: parent(folder).to_owned(),
            size: 0,
            packed: 0,
            modified_ms: None,
            note: "",
        });
    }
    rows.extend(folders);
    rows.extend(files);
    rows
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
    for row in rows.filter(|row| !row.is_up) {
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
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// The folder next to the archive, named after it without any `.partN`.
pub fn default_destination(archive: &str) -> String {
    let split = archive.rfind(['\\', '/']).map_or(0, |slash| slash + 1);
    let (dir, file) = archive.split_at(split);
    let stem = file.rfind('.').map_or(file, |dot| &file[..dot]);
    let lower = stem.to_ascii_lowercase();
    let stem = match lower.rfind(".part") {
        Some(pos)
            if lower.len() > pos + 5 && lower[pos + 5..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            &stem[..pos]
        }
        _ => stem,
    };
    format!("{dir}{}", if stem.is_empty() { "extracted" } else { stem })
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

    #[test]
    fn folders_come_first_with_totals() {
        let entries = [
            entry("z.txt", false, 1),
            entry("docs/a.txt", false, 10),
            entry("docs/deep/b.txt", false, 20),
            entry("empty", true, 0),
        ];
        let rows = rows_in(&entries, "");
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["docs", "empty", "z.txt"]);
        assert_eq!(rows[0].size, 30);

        let inside = rows_in(&entries, "docs/");
        let names: Vec<_> = inside.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "deep", "a.txt"]);
        assert_eq!(inside[0].path, "");
        assert_eq!(rows_in(&entries, "docs/deep/")[0].path, "docs/");
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
    fn sizes_and_destinations() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(50 * 1024 * 1024), "50 MB");
        assert_eq!(default_destination(r"C:\x\film.part01.rar"), r"C:\x\film");
        assert_eq!(default_destination(r"C:\x\a.part.rar"), r"C:\x\a.part");
        assert_eq!(default_destination("set.rar"), "set");
    }
}
