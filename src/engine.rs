//! Open, test and extract, independent of the window so it can be tested directly.
use crate::volumes::{discover_sibling_volumes, missing_parts};
use rars::filename::{decoded_name, relative_path, LegacyNameEncoding};
use rars::{
    extract_volumes_with_links, test_volumes_with_options_and_callback, Archive, ArchiveFamily,
    ArchiveReadOptions, ArchiveReader, ErrorKind, ExtractedEntryMeta, ExtractedLink,
    ReadCancellation,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

#[derive(Debug)]
pub struct AppError {
    pub kind: &'static str,
    pub message: String,
}

impl From<rars::Error> for AppError {
    fn from(error: rars::Error) -> Self {
        let kind = match error.kind() {
            ErrorKind::PasswordRequired => "password_required",
            ErrorKind::BadPassword => "bad_password",
            ErrorKind::Cancelled => "cancelled",
            _ => "error",
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: "error",
            message: error.to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub is_directory: bool,
    pub is_link: bool,
    pub is_encrypted: bool,
    pub packed_size: u64,
    pub unpacked_size: u64,
    pub modified_ms: Option<u64>,
}

#[derive(Debug)]
pub struct OpenedArchive {
    pub path: String,
    pub format: &'static str,
    pub parts: usize,
    pub is_solid: bool,
    pub any_encrypted: bool,
    pub comment: Option<String>,
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current: String,
}

#[derive(Debug, Default)]
pub struct ExtractReport {
    pub written: usize,
    pub skipped_existing: usize,
    pub warnings: Vec<String>,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Legacy archives without Unicode names most often come from DOS/OEM tools.
const LEGACY_FALLBACK: LegacyNameEncoding = LegacyNameEncoding::Cp437;

fn display_name(family: ArchiveFamily, raw: &[u8], unicode: bool) -> String {
    if family == ArchiveFamily::Rar50Plus {
        // RAR 5 names use '/'; a backslash is a literal character, unusable on Windows.
        return String::from_utf8_lossy(raw).replace('\\', "_");
    }
    let decoded = match std::str::from_utf8(raw) {
        Ok(_) => None,
        Err(_) => decoded_name(raw, unicode, Some(LEGACY_FALLBACK)).ok(),
    };
    String::from_utf8_lossy(decoded.as_deref().unwrap_or(raw)).replace('\\', "/")
}

/// The path below the output folder, or why this name must not be written on Windows.
/// `relative_path` refuses traversal, roots and drives. The rest (streams like
/// `a.txt:evil`, device names like `CON`, trailing dots) is Windows-specific.
pub fn safe_relative(name: &str) -> Result<PathBuf, String> {
    let path = relative_path(name.as_bytes(), false).map_err(|error| error.to_string())?;
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err("unsafe path".into());
        };
        let part = part.to_str().ok_or("name is not valid Unicode")?;
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err("name has characters Windows doesn't allow".into());
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err("name ends with a dot or space".into());
        }
        let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            return Err("name is reserved by Windows".into());
        }
    }
    Ok(path)
}

fn options<'a>(
    password: Option<&'a [u8]>,
    cancel: &'a ReadCancellation,
) -> ArchiveReadOptions<'a> {
    ArchiveReadOptions::with_optional_password(password).with_cancellation(cancel)
}

fn load(
    path: &Path,
    password: Option<&[u8]>,
    cancel: &ReadCancellation,
) -> Result<(Vec<PathBuf>, Vec<Archive>), AppError> {
    let paths = discover_sibling_volumes(path);
    let archives = paths
        .iter()
        .map(|part| ArchiveReader::read_path_with_options(part, options(password, cancel)))
        .collect::<rars::Result<Vec<_>>>()?;
    Ok((paths, archives))
}

pub fn open(
    path: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
) -> Result<OpenedArchive, AppError> {
    let password = password.map(str::as_bytes);
    let (paths, archives) = load(path, password, cancel)?;
    let family = archives[0].family();
    let mut warnings: Vec<String> = missing_parts(&paths)
        .into_iter()
        .map(|index| format!("Part {} of this set is missing.", index + 1))
        .collect();
    let mut entries: Vec<Entry> = Vec::new();
    let (mut is_solid, mut any_encrypted, mut continues) = (false, false, false);
    for archive in &archives {
        for member in archive.members() {
            let meta = &member.meta;
            is_solid |= member.is_solid();
            any_encrypted |= meta.is_encrypted;
            continues = meta.is_split_after;
            // A member split across parts appears once per part.
            if meta.is_split_before {
                if let Some(last) = entries.last_mut() {
                    last.packed_size += meta.packed_size;
                }
                continue;
            }
            entries.push(Entry {
                name: display_name(family, &meta.name, member.name_is_unicode()),
                is_directory: meta.is_directory,
                is_link: meta.is_redirection,
                is_encrypted: meta.is_encrypted,
                packed_size: meta.packed_size,
                unpacked_size: meta.unpacked_size,
                modified_ms: meta
                    .modification_time()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_millis() as u64),
            });
        }
    }
    if continues {
        warnings.push("The last file continues into a part that wasn't found.".into());
    }
    let comment = archives[0]
        .comment_with_options(options(password, cancel))
        .ok()
        .flatten()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    Ok(OpenedArchive {
        path: path.display().to_string(),
        format: match family {
            ArchiveFamily::Rar13 => "RAR 1.3/1.4",
            ArchiveFamily::Rar15To40 => "RAR 1.5-4.x",
            ArchiveFamily::Rar50Plus => "RAR 5/7",
            _ => "RAR",
        },
        parts: archives.len(),
        is_solid,
        any_encrypted,
        comment,
        entries,
        warnings,
    })
}

struct Tracker {
    done: AtomicU64,
    total: u64,
    current: Mutex<String>,
    last_emit: Mutex<Instant>,
    emit: ProgressFn,
}

impl Tracker {
    fn new(total: u64, emit: ProgressFn) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total,
            current: Mutex::new(String::new()),
            last_emit: Mutex::new(Instant::now() - Duration::from_secs(1)),
            emit,
        })
    }

    fn set_current(&self, name: &str) {
        *self.current.lock().unwrap() = name.to_owned();
        self.report(true);
    }

    fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        self.report(false);
    }

    fn report(&self, force: bool) {
        let mut last = self.last_emit.lock().unwrap();
        if force || last.elapsed() >= Duration::from_millis(100) {
            *last = Instant::now();
            (self.emit)(Progress {
                done_bytes: self.done.load(Ordering::Relaxed),
                total_bytes: self.total,
                current: self.current.lock().unwrap().clone(),
            });
        }
    }
}

struct CountingFile {
    file: File,
    tracker: Arc<Tracker>,
}

impl Write for CountingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        self.tracker.add(written as u64);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A file awaiting its checksum. The library verifies each member before asking
/// for the next output, so the next callback means this one is good.
struct Pending {
    temp: PathBuf,
    destination: PathBuf,
    relative: PathBuf,
    modified: Option<std::time::SystemTime>,
}

struct Extraction<'a> {
    out_dir: &'a Path,
    staging: PathBuf,
    family: ArchiveFamily,
    selected: Option<&'a HashSet<String>>,
    overwrite: bool,
    tracker: Arc<Tracker>,
    pending: Option<Pending>,
    planned: HashSet<String>,
    written_paths: HashMap<PathBuf, PathBuf>,
    next_temp: u64,
    report: ExtractReport,
}

impl Extraction<'_> {
    fn publish(&mut self) -> rars::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Some(parent) = pending.destination.parent() {
            fs::create_dir_all(parent)?;
        }
        // Re-check: an earlier member can't have planted a link, but the disk can change.
        rars::filename::checked_output_path(self.out_dir, &pending.relative)?;
        fs::rename(&pending.temp, &pending.destination)?;
        if let Some(modified) = pending.modified {
            File::options()
                .write(true)
                .open(&pending.destination)?
                .set_modified(modified)?;
        }
        self.written_paths
            .insert(pending.relative, pending.destination);
        self.report.written += 1;
        Ok(())
    }

    /// Where this entry goes, or `None` if it's left out (recording why, if notable).
    fn destination(
        &mut self,
        meta: &ExtractedEntryMeta,
    ) -> rars::Result<Option<(String, PathBuf, PathBuf)>> {
        let name = display_name(self.family, &meta.name, meta.name_is_unicode);
        if self.selected.is_some_and(|set| !set.contains(&name)) {
            return Ok(None);
        }
        let relative = match safe_relative(&name) {
            Ok(relative) => relative,
            Err(reason) => {
                self.report
                    .warnings
                    .push(format!("Not extracted: {name} ({reason})"));
                return Ok(None);
            }
        };
        // Windows names ignore case, so "A" and "a" would overwrite each other.
        if !self.planned.insert(name.to_lowercase()) && !meta.is_directory {
            self.report
                .warnings
                .push(format!("Not extracted: {name} (same name as an earlier file)"));
            return Ok(None);
        }
        let destination = match rars::filename::checked_output_path(self.out_dir, &relative) {
            Ok(destination) => destination,
            Err(error) => {
                self.report
                    .warnings
                    .push(format!("Not extracted: {name} ({error})"));
                return Ok(None);
            }
        };
        Ok(Some((name, relative, destination)))
    }

    fn open_entry(&mut self, meta: &ExtractedEntryMeta) -> rars::Result<Box<dyn Write>> {
        self.publish()?;
        let Some((name, relative, destination)) = self.destination(meta)? else {
            return Ok(Box::new(io::sink()));
        };
        self.tracker.set_current(&name);
        if meta.is_directory {
            fs::create_dir_all(&destination)?;
            return Ok(Box::new(io::sink()));
        }
        if !self.overwrite && fs::symlink_metadata(&destination).is_ok() {
            self.report.skipped_existing += 1;
            return Ok(Box::new(CountingSink(self.tracker.clone())));
        }
        fs::create_dir_all(&self.staging)?;
        let temp = self.staging.join(self.next_temp.to_string());
        self.next_temp += 1;
        let file = File::create(&temp)?;
        self.pending = Some(Pending {
            temp,
            destination,
            relative,
            modified: rars::timestamp::extracted_system_time(
                self.family,
                meta.file_time,
                meta.mtime_refinement,
            ),
        });
        Ok(Box::new(CountingFile {
            file,
            tracker: self.tracker.clone(),
        }))
    }

    /// Copies are recreated from files this run wrote. Symlinks and junctions are
    /// reported, never created: they could redirect later writes out of the folder.
    fn link_entry(
        &mut self,
        meta: &ExtractedEntryMeta,
        link: ExtractedLink<'_>,
    ) -> rars::Result<()> {
        self.publish()?;
        let Some((name, relative, destination)) = self.destination(meta)? else {
            return Ok(());
        };
        let source = match link {
            ExtractedLink::HardLink { target } | ExtractedLink::FileCopy { target } => {
                let target = display_name(self.family, target, true);
                safe_relative(&target)
                    .ok()
                    .and_then(|target| self.written_paths.get(&target).cloned())
            }
            _ => None,
        };
        let Some(source) = source else {
            self.report
                .warnings
                .push(format!("Not extracted: {name} (link)"));
            return Ok(());
        };
        if !self.overwrite && fs::symlink_metadata(&destination).is_ok() {
            self.report.skipped_existing += 1;
            return Ok(());
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &destination)?;
        self.written_paths.insert(relative, destination);
        self.report.written += 1;
        Ok(())
    }
}

/// A skipped existing file still counts toward the bar.
struct CountingSink(Arc<Tracker>);

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.add(buf.len() as u64);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct ExtractRequest<'a> {
    pub archive: &'a Path,
    pub out_dir: &'a Path,
    pub password: Option<&'a str>,
    /// Entry names as `open` listed them; `None` means everything.
    pub selected: Option<HashSet<String>>,
    pub overwrite: bool,
}

pub fn extract(
    request: ExtractRequest<'_>,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<ExtractReport, AppError> {
    let password = request.password.map(str::as_bytes);
    let (_, archives) = load(request.archive, password, cancel)?;
    let family = archives[0].family();
    fs::create_dir_all(request.out_dir)?;
    let out_dir = fs::canonicalize(request.out_dir)?;
    let total = selected_bytes(&archives, family, request.selected.as_ref());
    let staging = out_dir.join(format!(".rawr-tmp-{}", std::process::id()));
    let state = RefCell::new(Extraction {
        out_dir: &out_dir,
        staging: staging.clone(),
        family,
        selected: request.selected.as_ref(),
        overwrite: request.overwrite,
        tracker: Tracker::new(total, emit),
        pending: None,
        planned: HashSet::new(),
        written_paths: HashMap::new(),
        next_temp: 0,
        report: ExtractReport::default(),
    });
    let result = extract_volumes_with_links(
        &archives,
        options(password, cancel),
        |meta| state.borrow_mut().open_entry(meta),
        |meta, link| state.borrow_mut().link_entry(meta, link),
    );
    let mut state = state.into_inner();
    let result = result.and_then(|()| state.publish());
    // An unpublished file failed or wasn't finished: it goes with the staging folder.
    let _ = fs::remove_dir_all(&staging);
    result?;
    Ok(state.report)
}

fn selected_bytes(
    archives: &[Archive],
    family: ArchiveFamily,
    selected: Option<&HashSet<String>>,
) -> u64 {
    archives
        .iter()
        .flat_map(|archive| archive.members())
        .filter(|member| !member.meta.is_split_before)
        .filter(|member| {
            selected.is_none_or(|set| {
                set.contains(&display_name(
                    family,
                    &member.meta.name,
                    member.name_is_unicode(),
                ))
            })
        })
        .map(|member| member.meta.unpacked_size)
        .sum()
}

pub fn test(
    archive: &Path,
    password: Option<&str>,
    cancel: &ReadCancellation,
    emit: ProgressFn,
) -> Result<usize, AppError> {
    let password = password.map(str::as_bytes);
    let (_, archives) = load(archive, password, cancel)?;
    let family = archives[0].family();
    let sizes: HashMap<String, u64> = archives
        .iter()
        .flat_map(|archive| archive.members())
        .filter(|member| !member.meta.is_split_before)
        .map(|member| {
            (
                display_name(family, &member.meta.name, member.name_is_unicode()),
                member.meta.unpacked_size,
            )
        })
        .collect();
    let tracker = Tracker::new(sizes.values().sum(), emit);
    let mut tested = 0;
    test_volumes_with_options_and_callback(&archives, options(password, cancel), |meta| {
        let name = display_name(family, &meta.name, meta.name_is_unicode);
        tracker.set_current(&name);
        tracker.add(sizes.get(&name).copied().unwrap_or(0));
        tested += 1;
        Ok(())
    })?;
    Ok(tested)
}
