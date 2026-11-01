//! Finding the other parts of a multi-part set.
//! Ported from rars-cli 0.12.3 `volumes.rs` (Apache-2.0), which is not importable.
use std::fs;
use std::path::{Path, PathBuf};

pub fn discover_sibling_volumes(first: &Path) -> Vec<PathBuf> {
    let parent = first
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let Some(first_key) = volume_name_key(first) else {
        return vec![first.to_path_buf()];
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return vec![first.to_path_buf()];
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            volume_name_key(path).as_ref() == Some(&first_key) && volume_sort_key(path).is_some()
        })
        .collect();
    if paths.is_empty() {
        paths.push(first.to_path_buf());
    }
    paths.sort_by(|a, b| {
        volume_sort_key(a)
            .cmp(&volume_sort_key(b))
            .then_with(|| a.cmp(b))
    });
    paths
}

/// Zero-based positions absent from a sorted set, for telling the user what's missing.
pub fn missing_parts(paths: &[PathBuf]) -> Vec<usize> {
    let keys: Vec<usize> = paths.iter().filter_map(|p| volume_sort_key(p)).collect();
    let Some(&last) = keys.last() else {
        return Vec::new();
    };
    (0..last).filter(|index| !keys.contains(index)).collect()
}

fn volume_name_key(path: &Path) -> Option<Vec<u8>> {
    let name = rars::filename::native_bytes(path.file_name()?).ok()?;
    let lower = name.to_ascii_lowercase();
    if let Some(pos) = lower.windows(5).rposition(|s| s == b".part") {
        if let Some(digits) = lower[pos + 5..].strip_suffix(b".rar") {
            if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                return Some([b"part:".as_slice(), &lower[..pos]].concat());
            }
        }
    }
    if lower.ends_with(b".rar")
        || (lower.len() >= 4
            && lower[lower.len() - 4..lower.len() - 2] == *b".r"
            && lower[lower.len() - 2..].iter().all(u8::is_ascii_digit))
    {
        return Some([b"old:".as_slice(), &lower[..lower.len() - 4]].concat());
    }
    None
}

fn volume_sort_key(path: &Path) -> Option<usize> {
    let name = rars::filename::native_bytes(path.file_name()?).ok()?;
    let lower = name.to_ascii_lowercase();
    if let Some(pos) = lower.windows(5).rposition(|s| s == b".part") {
        if let Some(digits) = lower[pos + 5..].strip_suffix(b".rar") {
            return std::str::from_utf8(digits)
                .ok()?
                .parse::<usize>()
                .ok()?
                .checked_sub(1);
        }
    }
    if lower.ends_with(b".rar") {
        return Some(0);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext.len() == 3 && ext.starts_with('r') {
        return ext[1..].parse::<usize>().ok().map(|index| index + 1);
    }
    None
}
