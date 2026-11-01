//! ZIP and 7z: opened, tested and extracted through the same safe output path as RAR.
use crate::engine::{
    extract_into, AppError, Entry, ExtractReport, ExtractRequest, Extraction, OpenedArchive,
    ProgressFn, Tracker,
};
use chrono::{Local, TimeZone};
use rars::{AttrSource, ExtractedEntryMeta, ReadCancellation};
use sevenz_rust2::{ArchiveReader, Error as SevenError, Password};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use zip::result::ZipError;
use zip::ZipArchive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rar,
    Zip,
    SevenZ,
}

/// By content, so a misnamed file still opens with the right reader.
pub fn kind_of(path: &Path) -> Kind {
    let mut magic = [0u8; 6];
    let read = File::open(path).and_then(|mut file| file.read(&mut magic));
    match (read, &magic) {
        (Ok(6), [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) => Kind::SevenZ,
        (Ok(6), [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..]) => Kind::Zip,
        _ => Kind::Rar,
    }
}

fn error(kind: &'static str, message: impl Into<String>) -> AppError {
    AppError {
        kind,
        message: message.into(),
    }
}

fn cancelled() -> AppError {
    error("cancelled", "cancelled")
}

fn zip_error(failure: ZipError) -> AppError {
    match failure {
        ZipError::InvalidPassword => error("bad_password", "wrong password"),
        ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => {
            error("password_required", "a password is required")
        }
        other => error("error", other.to_string()),
    }
}

fn seven_error(failure: SevenError) -> AppError {
    match failure {
        SevenError::PasswordRequired => error("password_required", "a password is required"),
        SevenError::MaybeBadPassword(_) => {
            error("bad_password", "wrong password, or the data is damaged")
        }
        other => error("error", other.to_string()),
    }
}

/// Copies to the end, which is what makes both readers verify the checksum.
fn pump(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    cancel: &ReadCancellation,
) -> Result<(), AppError> {
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..count])?;
    }
}

/// Names use '/' from here on. ZIPs made on Windows sometimes store backslashes.
fn clean_name(name: &str) -> String {
    name.replace('\\', "/")
}

fn unix_seconds(time: SystemTime) -> Option<u32> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok())
}

fn meta(name: &str, seconds: Option<u32>, is_directory: bool) -> ExtractedEntryMeta {
    let mut meta = ExtractedEntryMeta::new(name.as_bytes().to_vec(), seconds, 0x20, is_directory)
        .with_attr_source(AttrSource::Dos);
    meta.name_is_unicode = true;
    meta
}

// ---- ZIP ----

fn open_zip(path: &Path) -> Result<ZipArchive<File>, AppError> {
    ZipArchive::new(File::open(path)?).map_err(zip_error)
}

/// ZIP stores local wall-clock time, with no zone.
fn zip_seconds(time: Option<zip::DateTime>) -> Option<u32> {
    let time = time?;
    let local = Local
        .with_ymd_and_hms(
            i32::from(time.year()),
            u32::from(time.month()),
            u32::from(time.day()),
            u32::from(time.hour()),
            u32::from(time.minute()),
            u32::from(time.second()),
        )
        .earliest()?;
    u32::try_from(local.timestamp()).ok()
}

fn list_zip(path: &Path) -> Result<OpenedArchive, AppError> {
    let mut archive = open_zip(path)?;
    let mut entries = Vec::with_capacity(archive.len());
    let mut any_encrypted = false;
    for index in 0..archive.len() {
        let file = archive.by_index_raw(index).map_err(zip_error)?;
        any_encrypted |= file.encrypted();
        entries.push(Entry {
            name: clean_name(&file.name().map_err(zip_error)?),
            is_directory: file.is_dir(),
            is_link: file.is_symlink(),
            is_encrypted: file.encrypted(),
            packed_size: file.compressed_size(),
            unpacked_size: file.size(),
            modified_ms: zip_seconds(file.last_modified()).map(|seconds| u64::from(seconds) * 1000),
        });
    }
    let comment = String::from_utf8_lossy(archive.comment()).trim().to_owned();
    Ok(OpenedArchive {
        path: path.display().to_string(),
        format: "ZIP",
        parts: 1,
        is_solid: false,
        any_encrypted,
        comment: (!comment.is_empty()).then_some(comment),
        entries,
        warnings: Vec::new(),
    })
}

fn zip_entry<'a>(
    archive: &'a mut ZipArchive<File>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a, File>, AppError> {
    match password {
        Some(password) => archive.by_index_decrypt(index, password.as_bytes()),
        None => archive.by_index(index),
    }
    .map_err(zip_error)
}

fn extract_zip(
    path: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
    state: &RefCell<Extraction<'_>>,
) -> Result<(), AppError> {
    let mut archive = open_zip(path)?;
    for index in 0..archive.len() {
        let (encrypted, wanted) = {
            let file = archive.by_index_raw(index).map_err(zip_error)?;
            let name = clean_name(&file.name().map_err(zip_error)?);
            if file.is_symlink() {
                state.borrow_mut().skip_link(&name);
                continue;
            }
            let selected = state.borrow().wants(&name);
            (file.encrypted(), selected)
        };
        // Reading an entry nobody asked for would demand its password for nothing.
        if !wanted {
            continue;
        }
        let password = password.filter(|_| encrypted);
        let mut file = zip_entry(&mut archive, index, password)?;
        let name = clean_name(&file.name().map_err(zip_error)?);
        let info = meta(&name, zip_seconds(file.last_modified()), file.is_dir());
        let mut writer = state.borrow_mut().open_entry(&info)?;
        pump(&mut file, &mut writer, cancel)?;
    }
    Ok(())
}

fn test_zip(
    path: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
    tracker: &Tracker,
) -> Result<usize, AppError> {
    let mut archive = open_zip(path)?;
    let mut tested = 0;
    for index in 0..archive.len() {
        let encrypted = archive.by_index_raw(index).map_err(zip_error)?.encrypted();
        let mut file = zip_entry(&mut archive, index, password.filter(|_| encrypted))?;
        tracker.set_current(&clean_name(&file.name().map_err(zip_error)?));
        pump(&mut file, &mut CountingSink(tracker), cancel)?;
        tested += 1;
    }
    Ok(tested)
}

// ---- 7z ----

fn seven_password(password: Option<&str>) -> Password {
    password.map_or_else(Password::empty, Password::from)
}

fn seven_seconds(entry: &sevenz_rust2::ArchiveEntry) -> Option<u32> {
    entry
        .has_last_modified_date
        .then(|| unix_seconds(entry.last_modified_date.into()))
        .flatten()
}

fn list_seven(path: &Path, password: Option<&str>) -> Result<OpenedArchive, AppError> {
    let reader = ArchiveReader::open(path, seven_password(password)).map_err(seven_error)?;
    let archive = reader.archive();
    let is_solid = archive.is_solid;
    let entries = archive
        .files
        .iter()
        .filter(|entry| !entry.is_anti_item)
        .map(|entry| Entry {
            name: clean_name(&entry.name),
            is_directory: entry.is_directory,
            is_link: false,
            is_encrypted: false,
            packed_size: entry.compressed_size,
            unpacked_size: entry.size,
            modified_ms: seven_seconds(entry).map(|seconds| u64::from(seconds) * 1000),
        })
        .collect();
    Ok(OpenedArchive {
        path: path.display().to_string(),
        format: "7z",
        parts: 1,
        is_solid,
        any_encrypted: false,
        comment: None,
        entries,
        warnings: Vec::new(),
    })
}

/// 7z decodes in stream order, so every entry is read even if only one is wanted.
fn walk_seven(
    path: &Path,
    password: Option<&str>,
    mut visit: impl FnMut(&sevenz_rust2::ArchiveEntry, &mut dyn Read) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let mut reader = ArchiveReader::open(path, seven_password(password)).map_err(seven_error)?;
    let mut stopped = None;
    let outcome = reader.for_each_entries(|entry, data| {
        if entry.is_anti_item {
            return Ok(true);
        }
        match visit(entry, data) {
            Ok(()) => Ok(true),
            Err(failure) => {
                stopped = Some(failure);
                Err(SevenError::Other("stopped".into()))
            }
        }
    });
    if let Some(failure) = stopped {
        // A wrong key usually surfaces mid-stream as a read failure.
        return Err(failure);
    }
    outcome.map_err(seven_error)
}

fn extract_seven(
    path: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
    state: &RefCell<Extraction<'_>>,
) -> Result<(), AppError> {
    // Empty files and folders have no data stream, so the walk may never visit them.
    let reader = ArchiveReader::open(path, seven_password(password)).map_err(seven_error)?;
    let mut done = HashSet::new();
    for entry in reader.archive().files.iter().filter(|entry| !entry.has_stream && !entry.is_anti_item) {
        let name = clean_name(&entry.name);
        let info = meta(&name, seven_seconds(entry), entry.is_directory);
        drop(state.borrow_mut().open_entry(&info)?);
        done.insert(name);
    }
    drop(reader);
    walk_seven(path, password, |entry, data| {
        let name = clean_name(&entry.name);
        if done.contains(&name) {
            return Ok(());
        }
        let info = meta(&name, seven_seconds(entry), entry.is_directory);
        let mut writer = state.borrow_mut().open_entry(&info)?;
        pump(data, &mut writer, cancel).map_err(seven_read_error)
    })
}

/// The 7z reader reports decryption failures as plain read errors.
fn seven_read_error(failure: AppError) -> AppError {
    if failure.kind == "error" && failure.message.to_lowercase().contains("password") {
        return error("bad_password", failure.message);
    }
    failure
}

struct CountingSink<'a>(&'a Tracker);

impl Write for CountingSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.add(buf.len() as u64);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ---- entry points ----

pub fn open(path: &Path, kind: Kind, password: Option<&str>) -> Result<OpenedArchive, AppError> {
    match kind {
        Kind::Zip => list_zip(path),
        _ => list_seven(path, password),
    }
}

pub fn extract(
    kind: Kind,
    request: ExtractRequest<'_>,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<ExtractReport, AppError> {
    let listing = open(request.archive, kind, request.password)?;
    let total = listing
        .entries
        .iter()
        .filter(|entry| {
            request
                .selected
                .as_ref()
                .is_none_or(|set| set.contains(&entry.name))
        })
        .map(|entry| entry.unpacked_size)
        .sum();
    let archive = request.archive;
    let password = request.password;
    extract_into(&request, total, emit, |state| match kind {
        Kind::Zip => extract_zip(archive, password, cancel, state),
        _ => extract_seven(archive, password, cancel, state),
    })
}

pub fn test(
    kind: Kind,
    path: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<usize, AppError> {
    let listing = open(path, kind, password)?;
    let total = listing.entries.iter().map(|entry| entry.unpacked_size).sum();
    let tracker: Arc<Tracker> = Tracker::new(total, emit);
    if kind == Kind::Zip {
        return test_zip(path, password, cancel, &tracker);
    }
    let mut tested = 0;
    walk_seven(path, password, |entry, data| {
        tracker.set_current(&clean_name(&entry.name));
        pump(data, &mut CountingSink(&tracker), cancel).map_err(seven_read_error)?;
        tested += 1;
        Ok(())
    })?;
    Ok(tested)
}
