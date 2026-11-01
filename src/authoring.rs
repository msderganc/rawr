//! Creating, changing and repairing archives.
use crate::engine::{
    display_name, extract, load, options, safe_relative, AppError, ExtractRequest, ProgressFn,
    Tracker,
};
use rars::{
    ArchiveFamily, ArchiveReader, ArchiveVersion, Builder, EntrySource, ErrorKind,
    ReadCancellation, WriteOperation, WriteProgress, WriteProgressEvent, WriterResources,
};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rar4,
    Rar5,
    Rar7,
}

impl Format {
    fn version(self) -> ArchiveVersion {
        match self {
            Self::Rar4 => ArchiveVersion::Rar40,
            Self::Rar5 => ArchiveVersion::Rar50,
            Self::Rar7 => ArchiveVersion::Rar70,
        }
    }
}

pub struct CreateRequest<'a> {
    pub output: &'a Path,
    pub inputs: &'a [PathBuf],
    pub format: Format,
    /// 0 stores, 5 packs hardest.
    pub level: u8,
    pub solid: bool,
    pub password: Option<&'a str>,
    pub encrypt_names: bool,
    pub comment: Option<&'a str>,
    pub recovery_percent: Option<u64>,
    pub volume_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct CreateReport {
    pub parts: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

fn message(text: impl Into<String>) -> AppError {
    AppError {
        kind: "error",
        message: text.into(),
    }
}

/// Reports writer progress and carries cancellation into the writer.
struct WriteReporter {
    tracker: Arc<Tracker>,
    cancel: ReadCancellation,
}

impl WriteProgress for WriteReporter {
    fn report(&self, event: WriteProgressEvent<'_>) {
        match event {
            WriteProgressEvent::EntryStarted {
                operation: WriteOperation::Compression,
                name,
                ..
            } => self.tracker.set_current(&String::from_utf8_lossy(name)),
            WriteProgressEvent::OperationStarted {
                operation: WriteOperation::Recovery,
                ..
            } => self.tracker.set_current("Adding recovery record"),
            WriteProgressEvent::OperationStarted {
                operation: WriteOperation::Emission,
                ..
            } => self.tracker.set_current("Writing archive"),
            WriteProgressEvent::Advanced {
                operation: WriteOperation::Compression,
                completed_bytes,
                total_bytes,
                ..
            } => self.tracker.set_absolute(completed_bytes, total_bytes),
            _ => {}
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// One thing to put in an archive, named the way the archive will name it.
enum Item {
    Directory {
        name: String,
        mtime: Option<u32>,
    },
    File {
        name: String,
        path: PathBuf,
        mtime: Option<u32>,
    },
}

impl Item {
    fn name(&self) -> &str {
        match self {
            Self::Directory { name, .. } | Self::File { name, .. } => name,
        }
    }
}

/// RAR 5 stores Unix seconds; older formats store DOS local-time fields.
fn stored_time(family: ArchiveFamily, modified: Option<std::time::SystemTime>) -> Option<u32> {
    use chrono::{DateTime, Datelike, Local, Timelike};
    let modified = modified?;
    if family == ArchiveFamily::Rar50Plus {
        return modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok());
    }
    let local: DateTime<Local> = modified.into();
    let year = u32::try_from(local.year()).ok()?.checked_sub(1980)?;
    (year < 128).then(|| {
        (year << 25)
            | (local.month() << 21)
            | (local.day() << 16)
            | (local.hour() << 11)
            | (local.minute() << 5)
            | (local.second() / 2)
    })
}

/// Links are left out and reported: following one would archive someone else's file.
fn collect(
    path: &Path,
    name: String,
    family: ArchiveFamily,
    skip: &Path,
    items: &mut Vec<Item>,
    warnings: &mut Vec<String>,
) -> Result<(), AppError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        warnings.push(format!("Left out: {name} (link)"));
        return Ok(());
    }
    let mtime = stored_time(family, meta.modified().ok());
    if meta.is_dir() {
        items.push(Item::Directory {
            name: name.clone(),
            mtime,
        });
        let mut children = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
            collect(&child.path(), child_name, family, skip, items, warnings)?;
        }
    } else if path != skip {
        items.push(Item::File {
            name,
            path: path.to_path_buf(),
            mtime,
        });
    }
    Ok(())
}

fn collect_inputs(
    inputs: &[PathBuf],
    prefix: &str,
    family: ArchiveFamily,
    skip: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<Item>, AppError> {
    let mut items = Vec::new();
    for input in inputs {
        let file_name = input
            .file_name()
            .ok_or_else(|| message(format!("Can't add {}", input.display())))?
            .to_string_lossy();
        let name = format!("{prefix}{file_name}");
        collect(input, name, family, skip, &mut items, warnings)?;
    }
    Ok(items)
}

fn queue(builder: &mut Builder, items: Vec<Item>) -> rars::Result<()> {
    for item in items {
        match item {
            Item::Directory { name, mtime } => {
                builder.add_directory(name.into_bytes(), mtime, None)?;
            }
            Item::File { name, path, mtime } => {
                builder.add_source(name.into_bytes(), EntrySource::from_path(path), mtime, None)?;
            }
        }
    }
    Ok(())
}

fn part_path(first: &Path, index: u64, width: usize) -> PathBuf {
    let stem = first.file_stem().unwrap_or_default().to_string_lossy();
    let stem = stem
        .rfind(".part")
        .filter(|pos| stem[pos + 5..].bytes().all(|b| b.is_ascii_digit()))
        .map_or(&*stem, |pos| &stem[..pos]);
    first.with_file_name(format!("{stem}.part{:0width$}.rar", index + 1))
}

/// Writes parts under temporary names, so a failure leaves no partial set behind.
struct PartSink {
    first: PathBuf,
    temps: Vec<PathBuf>,
}

impl rars::rar50::VolumeSink for PartSink {
    fn start_volume(&mut self, index: u64) -> rars::Result<Box<dyn Write + Send>> {
        let temp = self.first.with_extension(format!("rawr-part-{index}.tmp"));
        let file = File::create(&temp)?;
        self.temps.push(temp);
        Ok(Box::new(file))
    }
}

impl PartSink {
    fn publish(mut self) -> io::Result<Vec<PathBuf>> {
        let width = self.temps.len().to_string().len().max(2);
        let mut parts = Vec::new();
        for (index, temp) in std::mem::take(&mut self.temps).into_iter().enumerate() {
            let part = part_path(&self.first, index as u64, width);
            fs::rename(temp, &part)?;
            parts.push(part);
        }
        Ok(parts)
    }
}

impl Drop for PartSink {
    fn drop(&mut self) {
        for temp in &self.temps {
            let _ = fs::remove_file(temp);
        }
    }
}

fn resources_beside(path: &Path) -> WriterResources {
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => WriterResources::default().with_temp_dir(parent),
        None => WriterResources::default(),
    }
}

pub fn create(
    request: CreateRequest<'_>,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<CreateReport, AppError> {
    if request.encrypt_names && request.password.is_none() {
        return Err(message("Encrypting file names needs a password."));
    }
    if request.volume_bytes.is_some() && request.format == Format::Rar4 {
        return Err(message("Splitting into parts needs RAR 5 or RAR 7."));
    }
    if request.volume_bytes.is_some() && request.comment.is_some() {
        return Err(message("Split archives can't have a comment."));
    }
    if request.recovery_percent.is_some() && request.format == Format::Rar4 {
        return Err(message("Recovery records need RAR 5 or RAR 7."));
    }
    let version = request.format.version();
    let mut warnings = Vec::new();
    let items = collect_inputs(
        request.inputs,
        "",
        version.family(),
        request.output,
        &mut warnings,
    )?;
    if !items.iter().any(|item| matches!(item, Item::File { .. })) {
        return Err(message("There are no files to add."));
    }
    let mut builder = Builder::new(version)
        .compression_level(Some(request.level))
        .store(request.level == 0)
        .solid(request.solid)
        .password(request.password.map(|p| p.as_bytes().to_vec()))
        .header_encryption(request.encrypt_names)
        .comment(request.comment.map(|c| c.as_bytes().to_vec()))
        .recovery_percent(request.recovery_percent)
        .volume_size(request.volume_bytes.map(|bytes| bytes as usize));
    queue(&mut builder, items)?;
    let reporter = WriteReporter {
        tracker: Tracker::new(0, emit),
        cancel: cancel.clone(),
    };
    let resources = resources_beside(request.output);
    let parts = if request.volume_bytes.is_some() {
        let mut sink = PartSink {
            first: request.output.to_path_buf(),
            temps: Vec::new(),
        };
        builder.write_volumes_to(&mut sink, &resources, Some(&reporter))?;
        sink.publish()?
    } else {
        builder.write_to_path_with_resources(request.output, &resources, Some(&reporter))?;
        vec![request.output.to_path_buf()]
    };
    Ok(CreateReport { parts, warnings })
}

#[derive(Debug, Clone)]
pub enum Edit {
    Delete(HashSet<String>),
    /// `from` is a file or folder name as `open` lists it.
    Rename { from: String, to: String },
    /// Added under `into` ("" or ending in '/'), replacing anything of the same name.
    Add { inputs: Vec<PathBuf>, into: String },
    /// An empty comment removes it.
    SetComment(String),
}

/// Removes the working folder even if the edit fails or is cancelled.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn is_inside(name: &str, folder: &str) -> bool {
    name == folder
        || name
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

struct Kept {
    name: String,
    is_directory: bool,
    mtime: Option<u32>,
    attr: u64,
    attr_source: rars::AttrSource,
}

/// Rebuilds the archive with the change applied. rars can't copy members across
/// directly, so what's kept is extracted first, which also verifies it. The
/// original is only replaced once the new archive has been fully written.
pub fn edit(
    archive_path: &Path,
    password: Option<&str>,
    change: Edit,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<Vec<String>, AppError> {
    let password_bytes = password.map(str::as_bytes);
    let (paths, archives) = load(archive_path, password_bytes, cancel)?;
    if paths.len() > 1 {
        return Err(message("Split archives can't be edited."));
    }
    let archive = &archives[0];
    if let Some(issue) = archive.rewrite_preservation_issues().first() {
        return Err(message(format!(
            "This archive can't be edited safely: {issue}"
        )));
    }
    if let Edit::Rename { to, .. } = &change {
        safe_relative(to).map_err(|reason| message(format!("Can't use that name: {reason}")))?;
    }
    let family = archive.family();
    let mut warnings = Vec::new();
    let added = match &change {
        Edit::Add { inputs, into } => {
            collect_inputs(inputs, into, family, archive_path, &mut warnings)?
        }
        _ => Vec::new(),
    };
    let added_names: HashSet<String> = added
        .iter()
        .map(|item| item.name().to_lowercase())
        .collect();
    let dropped = |name: &str| match &change {
        Edit::Delete(names) => names.contains(name),
        Edit::Add { .. } => added_names.contains(&name.to_lowercase()),
        Edit::Rename { .. } | Edit::SetComment(_) => false,
    };

    let mut kept = Vec::new();
    for member in archive.members() {
        let meta = &member.meta;
        let name = display_name(family, &meta.name, member.name_is_unicode());
        if meta.is_redirection {
            return Err(message("Archives containing links can't be edited."));
        }
        if !dropped(&name) {
            kept.push(Kept {
                name,
                is_directory: meta.is_directory,
                mtime: meta.file_time,
                attr: meta.file_attr,
                attr_source: meta.attr_source(),
            });
        }
    }
    if let Edit::Rename { from, to } = &change {
        if !kept.iter().any(|item| is_inside(&item.name, from)) {
            return Err(message(format!("{from} isn't in this archive.")));
        }
        if kept.iter().any(|item| is_inside(&item.name, to)) {
            return Err(message(format!("{to} already exists in this archive.")));
        }
    }

    let parent = archive_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = Staging(parent.join(format!(".rawr-edit-{}", std::process::id())));
    if kept.iter().any(|item| !item.is_directory) {
        let selected = kept.iter().map(|item| item.name.clone()).collect();
        let report = extract(
            ExtractRequest {
                archive: archive_path,
                out_dir: &staging.0,
                password,
                selected: Some(selected),
                overwrite: true,
            },
            cancel,
            emit.clone(),
        )?;
        if let Some(problem) = report.warnings.first() {
            return Err(message(format!("Can't edit this archive. {problem}")));
        }
    }
    let staged_root = if staging.0.exists() {
        fs::canonicalize(&staging.0)?
    } else {
        staging.0.clone()
    };

    let mut builder = archive.preserving_builder(password_bytes)?;
    let comment = match &change {
        Edit::SetComment(text) => (!text.is_empty()).then(|| text.as_bytes().to_vec()),
        _ => archive.comment_with_options(options(password_bytes, cancel))?,
    };
    builder = builder.comment(comment);
    for item in kept {
        let new_name = match &change {
            Edit::Rename { from, to } if is_inside(&item.name, from) => {
                format!("{to}{}", &item.name[from.len()..])
            }
            _ => item.name.clone(),
        };
        let bytes = new_name.into_bytes();
        let unix_mode = (item.attr_source == rars::AttrSource::Unix).then_some(item.attr as u32);
        if item.is_directory {
            builder.add_directory(bytes, item.mtime, unix_mode.map(|mode| mode & 0o7777))?;
            continue;
        }
        let staged = staged_root.join(safe_relative(&item.name).map_err(message)?);
        builder.add_source(
            bytes.clone(),
            EntrySource::from_path(staged),
            item.mtime,
            unix_mode,
        )?;
        if item.attr_source == rars::AttrSource::Dos {
            builder.set_dos_attributes(&bytes, item.attr)?;
        }
    }
    queue(&mut builder, added)?;
    if builder.is_empty() {
        return Err(message(
            "That would leave the archive empty. Delete the file itself instead.",
        ));
    }
    let reporter = WriteReporter {
        tracker: Tracker::new(0, emit),
        cancel: cancel.clone(),
    };
    builder.write_to_path_with_resources(
        archive_path,
        &resources_beside(archive_path),
        Some(&reporter),
    )?;
    Ok(warnings)
}

/// Writes a repaired copy, leaving the damaged original alone. Returns what changed.
pub fn repair(
    archive_path: &Path,
    output: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
) -> Result<String, AppError> {
    let password_bytes = password.map(str::as_bytes);
    let parsed =
        ArchiveReader::read_path_with_options(archive_path, options(password_bytes, cancel));
    let result = match parsed {
        Ok(archive) => archive.repair_recovery_with_options(options(password_bytes, cancel))?,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::PasswordRequired | ErrorKind::BadPassword | ErrorKind::Cancelled
            ) =>
        {
            return Err(error.into())
        }
        // Headers too damaged to parse can still be rebuilt from an inline record.
        Err(_) => rars::rar50::repair_inline_recovery_bytes_with_options(
            &fs::read(archive_path)?,
            options(password_bytes, cancel),
        )?,
    };
    result.write_to_path(output, Some(cancel))?;
    let report = result.report;
    let mut actions = Vec::new();
    if report.data_repaired {
        actions.push("repaired damaged data");
    }
    if report.recovery_record_rebuilt {
        actions.push("rebuilt the recovery record");
    }
    if report.end_record_rebuilt {
        actions.push("rebuilt the archive ending");
    }
    Ok(if actions.is_empty() {
        "No damage found.".to_owned()
    } else {
        let joined = actions.join(", ");
        format!("{}{}.", joined[..1].to_uppercase(), &joined[1..])
    })
}
