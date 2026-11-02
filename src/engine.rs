//! Open, test and extract, independent of the window so it can be tested directly.
use crate::others::{self, Kind};
use crate::volumes::{discover_sibling_volumes, missing_parts};
use rars::filename::{decoded_name, relative_path, LegacyNameEncoding};
use rars::{
    extract_volumes_with_links, test_volumes_with_options_and_callback, Archive, ArchiveFamily,
    ArchiveReadOptions, ArchiveReader, ErrorKind, ExtractedEntryMeta, ExtractedLink,
    ExtractionDecision, ReadCancellation,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
            ErrorKind::UnsupportedFormat => "unsupported",
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
    /// The format can't say how big its contents are without decompressing them.
    pub size_unknown: bool,
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

/// Old archives don't record their names' code page, so it's a setting. These are
/// listed in the order Settings offers them.
const LEGACY_ENCODINGS: [LegacyNameEncoding; 6] = [
    LegacyNameEncoding::Cp437,
    LegacyNameEncoding::Cp850,
    LegacyNameEncoding::Cp852,
    LegacyNameEncoding::Cp866,
    LegacyNameEncoding::Windows1252,
    LegacyNameEncoding::Windows1251,
];

static LEGACY_ENCODING: AtomicUsize = AtomicUsize::new(0);

pub fn set_legacy_encoding(choice: usize) {
    LEGACY_ENCODING.store(choice.min(LEGACY_ENCODINGS.len() - 1), Ordering::Relaxed);
}

/// How a name without Unicode reads in a given encoding. Valid UTF-8 stays as it is.
pub fn legacy_text(raw: &[u8], unicode: bool, choice: usize) -> String {
    let decoded = match std::str::from_utf8(raw) {
        Ok(_) => None,
        Err(_) => decoded_name(raw, unicode, LEGACY_ENCODINGS.get(choice).copied()).ok(),
    };
    String::from_utf8_lossy(decoded.as_deref().unwrap_or(raw)).replace('\\', "/")
}

pub(crate) fn display_name(family: ArchiveFamily, raw: &[u8], unicode: bool) -> String {
    if family == ArchiveFamily::Rar50Plus {
        // RAR 5 names use '/'; a backslash is a literal character, unusable on Windows.
        return String::from_utf8_lossy(raw).replace('\\', "_");
    }
    legacy_text(raw, unicode, LEGACY_ENCODING.load(Ordering::Relaxed))
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

/// The largest RAR 5 member decoded whole in memory. Larger ones stream, holding
/// one dictionary rather than the entire file, and go through the threaded decoder.
const BUFFERED_MEMBER_LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) fn options<'a>(
    password: Option<&'a [u8]>,
    cancel: &'a ReadCancellation,
) -> ArchiveReadOptions<'a> {
    ArchiveReadOptions::with_optional_password(password)
        .with_cancellation(cancel)
        .with_rar50_buffered_decode_limit(BUFFERED_MEMBER_LIMIT)
}

pub(crate) fn load(
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
    let kind = others::kind_of(path);
    if kind != Kind::Rar {
        return others::open(path, kind, password);
    }
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
        size_unknown: false,
    })
}

pub(crate) struct Tracker {
    done: AtomicU64,
    total: AtomicU64,
    current: Mutex<String>,
    last_emit: Mutex<Instant>,
    emit: ProgressFn,
}

impl Tracker {
    pub(crate) fn new(total: u64, emit: ProgressFn) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            current: Mutex::new(String::new()),
            last_emit: Mutex::new(Instant::now() - Duration::from_secs(1)),
            emit,
        })
    }

    pub(crate) fn set_current(&self, name: &str) {
        *self.current.lock().unwrap() = name.to_owned();
        self.report(true);
    }

    pub(crate) fn set_absolute(&self, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.report(false);
    }

    pub(crate) fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        self.report(false);
    }

    fn report(&self, force: bool) {
        let mut last = self.last_emit.lock().unwrap();
        if force || last.elapsed() >= Duration::from_millis(100) {
            *last = Instant::now();
            (self.emit)(Progress {
                done_bytes: self.done.load(Ordering::Relaxed),
                total_bytes: self.total.load(Ordering::Relaxed),
                current: self.current.lock().unwrap().clone(),
            });
        }
    }
}

/// A file past this many bytes finishes writing on a thread of its own, so the disk
/// does not hold up decoding. A small file is done before a thread would pay.
const BACKGROUND_WRITE_AFTER: u64 = 8 * 1024 * 1024;
/// How many chunks may wait for the disk, which bounds what a slow one can hold.
const QUEUED_WRITES: usize = 32;

/// The thread finishing a large file, shared with its `Pending` so nothing is
/// published until every byte has landed.
#[derive(Default)]
struct BackgroundWrites {
    chunks: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
}

impl BackgroundWrites {
    fn start(&mut self, file: Arc<File>) {
        let (chunks, queue) = std::sync::mpsc::sync_channel::<Vec<u8>>(QUEUED_WRITES);
        self.chunks = Some(chunks);
        self.worker = Some(std::thread::spawn(move || {
            for chunk in queue {
                (&*file).write_all(&chunk)?;
            }
            Ok(())
        }));
    }

    /// Waits for what is queued, and reports the first write that failed.
    fn finish(&mut self) -> io::Result<()> {
        self.chunks = None;
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("file writing stopped unexpectedly"))),
            None => Ok(()),
        }
    }
}

impl Drop for BackgroundWrites {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// A file up to this size stays in memory until its checksum passes, then a publisher
/// thread writes it straight to its destination. Creating files is most of the cost
/// of unpacking small ones, and decoding no longer waits for it.
const IN_MEMORY_FILE: usize = 1024 * 1024;
const PUBLISHER_THREADS: usize = 4;
/// How many verified files may wait, which bounds the memory they hold.
const QUEUED_FILES: usize = 32;

/// What has been written of one entry so far.
#[derive(Default)]
struct EntryOutput {
    buffer: Vec<u8>,
    file: Option<Arc<File>>,
    background: BackgroundWrites,
}

struct CountingFile {
    output: Arc<std::sync::Mutex<EntryOutput>>,
    staging: PathBuf,
    temp: PathBuf,
    tracker: Arc<Tracker>,
    written: u64,
}

impl Write for CountingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut output = self.output.lock().expect("writes do not panic");
        if output.file.is_none() {
            if output.buffer.len() + buf.len() <= IN_MEMORY_FILE {
                output.buffer.extend_from_slice(buf);
                self.tracker.add(buf.len() as u64);
                return Ok(buf.len());
            }
            fs::create_dir_all(&self.staging)?;
            let file = Arc::new(File::create(&self.temp)?);
            (&*file).write_all(&output.buffer)?;
            self.written = output.buffer.len() as u64;
            output.buffer = Vec::new();
            output.file = Some(file);
        }
        let file = output.file.clone().expect("the file was just made");
        if self.written >= BACKGROUND_WRITE_AFTER {
            if output.background.worker.is_none() {
                output.background.start(file);
            }
            let queued = output
                .background
                .chunks
                .as_ref()
                .is_some_and(|chunks| chunks.send(buf.to_vec()).is_ok());
            if !queued {
                // The worker has stopped, which only a failed write does.
                output.background.finish()?;
                return Err(io::Error::other("file writing stopped unexpectedly"));
            }
            self.tracker.add(buf.len() as u64);
            return Ok(buf.len());
        }
        let written = (&*file).write(buf)?;
        self.written += written as u64;
        self.tracker.add(written as u64);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A verified file on its way to its destination.
struct FinishedFile {
    bytes: Vec<u8>,
    out_dir: PathBuf,
    relative: PathBuf,
    destination: PathBuf,
    staging: PathBuf,
    temp: PathBuf,
    modified: Option<std::time::SystemTime>,
}

impl FinishedFile {
    fn write(self) -> io::Result<()> {
        // Re-check: an earlier member can't have planted a link, but the disk can change.
        rars::filename::checked_output_path(&self.out_dir, &self.relative)
            .map_err(|error| io::Error::other(error.to_string()))?;
        // create_new refuses whatever is already there, links included. Replacing it
        // goes through a rename, which replaces the link rather than following it.
        let replacing = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.destination)
        {
            Ok(file) => Some((file, false)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
            Err(error) => return Err(error),
        };
        let (file, renamed) = match replacing {
            Some(opened) => opened,
            None => {
                fs::create_dir_all(&self.staging)?;
                (File::create(&self.temp)?, true)
            }
        };
        (&file).write_all(&self.bytes)?;
        if let Some(modified) = self.modified {
            file.set_modified(modified)?;
        }
        drop(file);
        if renamed {
            fs::rename(&self.temp, &self.destination)?;
        }
        Ok(())
    }
}

/// Threads writing out verified files.
struct Publisher {
    files: std::sync::mpsc::SyncSender<FinishedFile>,
    workers: Vec<std::thread::JoinHandle<()>>,
    failure: Arc<Mutex<Option<io::Error>>>,
}

impl Publisher {
    fn new() -> Self {
        let (files, queue) = std::sync::mpsc::sync_channel::<FinishedFile>(QUEUED_FILES);
        let queue = Arc::new(Mutex::new(queue));
        let failure = Arc::new(Mutex::new(None));
        let workers = (0..PUBLISHER_THREADS)
            .map(|_| {
                let (queue, failure) = (queue.clone(), failure.clone());
                std::thread::spawn(move || loop {
                    let Ok(file) = queue.lock().unwrap().recv() else {
                        return;
                    };
                    if let Err(error) = file.write() {
                        failure.lock().unwrap().get_or_insert(error);
                    }
                })
            })
            .collect();
        Self {
            files,
            workers,
            failure,
        }
    }

    /// Waits for every queued file, and reports the first that failed.
    fn finish(self) -> io::Result<()> {
        drop(self.files);
        for worker in self.workers {
            let _ = worker.join();
        }
        match self.failure.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// A file awaiting its checksum. The library verifies each member before asking
/// for the next output, so the next callback means this one is good.
struct Pending {
    output: Arc<std::sync::Mutex<EntryOutput>>,
    temp: PathBuf,
    destination: PathBuf,
    relative: PathBuf,
    modified: Option<std::time::SystemTime>,
}

pub(crate) struct Extraction<'a> {
    out_dir: &'a Path,
    staging: PathBuf,
    family: ArchiveFamily,
    selected: Option<&'a HashSet<String>>,
    overwrite: bool,
    tracker: Arc<Tracker>,
    pending: Option<Pending>,
    publisher: Option<Publisher>,
    planned: HashSet<String>,
    written_paths: HashMap<PathBuf, PathBuf>,
    next_temp: u64,
    // Per-file filesystem calls are most of the cost of unpacking many small files.
    made_dirs: HashSet<PathBuf>,
    checked_dirs: HashSet<PathBuf>,
    report: ExtractReport,
}

impl<'a> Extraction<'a> {
    fn new(
        out_dir: &'a Path,
        staging: PathBuf,
        family: ArchiveFamily,
        request: &'a ExtractRequest<'_>,
        tracker: Arc<Tracker>,
    ) -> Self {
        Self {
            out_dir,
            staging,
            family,
            selected: request.selected.as_ref(),
            overwrite: request.overwrite,
            tracker,
            pending: None,
            publisher: None,
            planned: HashSet::new(),
            written_paths: HashMap::new(),
            next_temp: 0,
            made_dirs: HashSet::new(),
            checked_dirs: HashSet::new(),
            report: ExtractReport::default(),
        }
    }

    fn publish(&mut self) -> rars::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Some(parent) = pending.destination.parent() {
            if !self.made_dirs.contains(parent) {
                fs::create_dir_all(parent)?;
                self.made_dirs.insert(parent.to_path_buf());
            }
        }
        let mut output = pending.output.lock().expect("writes do not panic");
        output.background.finish()?;
        if let Some(file) = output.file.take() {
            // Re-check: an earlier member can't have planted a link, but the disk can change.
            rars::filename::checked_output_path(self.out_dir, &pending.relative)?;
            if let Some(modified) = pending.modified {
                file.set_modified(modified)?;
            }
            drop(file);
            fs::rename(&pending.temp, &pending.destination)?;
        } else {
            let finished = FinishedFile {
                bytes: std::mem::take(&mut output.buffer),
                out_dir: self.out_dir.to_path_buf(),
                relative: pending.relative.clone(),
                destination: pending.destination.clone(),
                staging: self.staging.clone(),
                temp: pending.temp.clone(),
                modified: pending.modified,
            };
            let publisher = self.publisher.get_or_insert_with(Publisher::new);
            if publisher.files.send(finished).is_err() {
                return Err(io::Error::other("file publishing stopped unexpectedly").into());
            }
        }
        drop(output);
        self.written_paths
            .insert(pending.relative, pending.destination);
        self.report.written += 1;
        Ok(())
    }

    /// Waits for every file on its way to its destination.
    pub(crate) fn finish_publishing(&mut self) -> rars::Result<()> {
        if let Some(publisher) = self.publisher.take() {
            publisher.finish()?;
        }
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
            self.report.warnings.push(format!(
                "Not extracted: {name} (same name as an earlier file)"
            ));
            return Ok(None);
        }
        // This run creates no links, so a folder that passed stays good until publish
        // checks the whole path again.
        let parent = relative.parent().map(Path::to_path_buf).unwrap_or_default();
        let checked = if self.checked_dirs.contains(&parent) {
            Ok(self.out_dir.join(&relative))
        } else {
            rars::filename::checked_output_path(self.out_dir, &relative)
        };
        let destination = match checked {
            Ok(destination) => {
                self.checked_dirs.insert(parent);
                destination
            }
            Err(error) => {
                self.report
                    .warnings
                    .push(format!("Not extracted: {name} ({error})"));
                return Ok(None);
            }
        };
        Ok(Some((name, relative, destination)))
    }

    pub(crate) fn report_input(&self, done: u64, total: u64) {
        self.tracker.set_absolute(done, total);
    }

    pub(crate) fn wants(&self, name: &str) -> bool {
        self.selected.is_none_or(|set| set.contains(name))
    }

    pub(crate) fn skip_link(&mut self, name: &str) {
        if self.wants(name) {
            self.report
                .warnings
                .push(format!("Not extracted: {name} (link)"));
        }
    }

    pub(crate) fn open_entry(&mut self, meta: &ExtractedEntryMeta) -> rars::Result<Box<dyn Write>> {
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
        let temp = self.staging.join(self.next_temp.to_string());
        self.next_temp += 1;
        let output = Arc::new(std::sync::Mutex::new(EntryOutput::default()));
        self.pending = Some(Pending {
            output: output.clone(),
            temp: temp.clone(),
            destination,
            relative,
            modified: rars::timestamp::extracted_system_time(
                self.family,
                meta.file_time,
                meta.mtime_refinement,
            ),
        });
        Ok(Box::new(CountingFile {
            output,
            staging: self.staging.clone(),
            temp,
            tracker: self.tracker.clone(),
            written: 0,
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
        // A copy reads a file this run wrote, so that file has to have landed.
        self.finish_publishing()?;
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
    let kind = others::kind_of(request.archive);
    if kind != Kind::Rar {
        return others::extract(kind, request, cancel, emit);
    }
    let password = request.password.map(str::as_bytes);
    let (_, archives) = load(request.archive, password, cancel)?;
    let family = archives[0].family();
    let total = selected_bytes(&archives, family, request.selected.as_ref());
    if independent_members(&archives, family, request.selected.as_ref()) {
        return extract_in_parallel(
            &request,
            &archives[0],
            family,
            password,
            cancel,
            total,
            emit,
        );
    }
    extract_into(&request, total, emit, |state| {
        state.borrow_mut().family = family;
        if can_skip_members(&archives, family, request.selected.as_ref()) {
            extract_skipping(&archives[0], family, options(password, cancel), state)
        } else {
            extract_volumes_with_links(
                &archives,
                options(password, cancel),
                |meta| state.borrow_mut().open_entry(meta),
                |meta, link| state.borrow_mut().link_entry(meta, link),
            )
        }
        .map_err(AppError::from)
    })
}

/// Sets up verified, staged output, runs `body`, and publishes what passed.
pub(crate) fn extract_into(
    request: &ExtractRequest<'_>,
    total: u64,
    emit: ProgressFn,
    body: impl FnOnce(&RefCell<Extraction<'_>>) -> Result<(), AppError>,
) -> Result<ExtractReport, AppError> {
    fs::create_dir_all(request.out_dir)?;
    let out_dir = fs::canonicalize(request.out_dir)?;
    let staging = out_dir.join(format!(".rawr-tmp-{}", std::process::id()));
    let state = RefCell::new(Extraction::new(
        &out_dir,
        staging.clone(),
        // Names and times from other formats arrive in the RAR 5 convention.
        ArchiveFamily::Rar50Plus,
        request,
        Tracker::new(total, emit),
    ));
    let result = body(&state);
    let mut state = state.into_inner();
    let result = result.and_then(|()| {
        state.publish()?;
        state.finish_publishing().map_err(AppError::from)
    });
    // Whatever had been verified still lands, even if a later file failed.
    let _ = state.finish_publishing();
    // An unpublished file failed or wasn't finished: it goes with the staging folder.
    let _ = fs::remove_dir_all(&staging);
    result?;
    Ok(state.report)
}

/// Unselected members can only be skipped in a single non-solid file. Solid members
/// depend on what came before them, and the skipping API can't recreate links.
fn can_skip_members(
    archives: &[Archive],
    family: ArchiveFamily,
    selected: Option<&HashSet<String>>,
) -> bool {
    let Some(selected) = selected else {
        return false;
    };
    let [archive] = archives else {
        return false;
    };
    if archive.is_solid() {
        return false;
    }
    archive.members().all(|member| {
        let name = display_name(family, &member.meta.name, member.name_is_unicode());
        !member.is_solid()
            && !member.meta.is_split_before
            && !member.meta.is_split_after
            && !(member.meta.is_redirection && selected.contains(&name))
    })
}

/// Members of one non-solid file don't depend on each other, so they can unpack side
/// by side. Links need the files they copy, and a repeated name needs an order.
fn independent_members(
    archives: &[Archive],
    family: ArchiveFamily,
    selected: Option<&HashSet<String>>,
) -> bool {
    let [archive] = archives else {
        return false;
    };
    if archive.is_solid() {
        return false;
    }
    let mut names = HashSet::new();
    archive.members().all(|member| {
        let name = display_name(family, &member.meta.name, member.name_is_unicode());
        !member.is_solid()
            && !member.meta.is_split_before
            && !member.meta.is_split_after
            && !(member.meta.is_redirection && selected.is_none_or(|set| set.contains(&name)))
            && names.insert(name.to_lowercase())
    }) && names.len() > 1
}

/// Runs one state per worker over `archive`. Workers walk the same list and each member
/// goes to the first to reach it, so a large file only holds up the worker unpacking it.
/// Returns every state, so each can be cleaned up, and the first error.
fn with_workers<S: Send>(
    archive: &Archive,
    family: ArchiveFamily,
    password: Option<&[u8]>,
    cancel: &ReadCancellation,
    selected: Option<&HashSet<String>>,
    states: Vec<S>,
    open: impl Fn(&mut S, &ExtractedEntryMeta) -> rars::Result<Box<dyn Write>> + Sync,
    finish: impl Fn(&mut S) -> rars::Result<()> + Sync,
) -> (Vec<S>, Option<rars::Error>) {
    let claimed = Mutex::new(HashSet::new());
    let failed = AtomicBool::new(false);
    let first_error = Mutex::new(None);
    let states = std::thread::scope(|scope| {
        let workers: Vec<_> = states
            .into_iter()
            .map(|mut state| {
                let (claimed, failed, first_error) = (&claimed, &failed, &first_error);
                let (open, finish) = (&open, &finish);
                scope.spawn(move || {
                    let result = archive
                        .extract_with_control(options(password, cancel), |member| {
                            let name =
                                display_name(family, &member.meta.name, member.name_is_unicode());
                            if failed.load(Ordering::Relaxed)
                                || selected.is_some_and(|set| !set.contains(&name))
                                || !claimed.lock().unwrap().insert(name)
                            {
                                return Ok(ExtractionDecision::Skip);
                            }
                            let mut meta = ExtractedEntryMeta::new(
                                member.meta.name.clone(),
                                member.meta.file_time,
                                member.meta.file_attr,
                                member.meta.is_directory,
                            )
                            .with_attr_source(member.meta.attr_source())
                            .with_mtime_refinement(member.meta.mtime_refinement);
                            meta.name_is_unicode = member.name_is_unicode();
                            Ok(ExtractionDecision::Extract(open(&mut state, &meta)?))
                        })
                        .map(|_| ())
                        .and_then(|()| finish(&mut state));
                    if let Err(error) = result {
                        failed.store(true, Ordering::Relaxed);
                        first_error.lock().unwrap().get_or_insert(error);
                    }
                    state
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    (states, first_error.into_inner().unwrap())
}

/// Workers beyond this mostly wait on the disk, and each holds a dictionary.
fn worker_count() -> usize {
    std::thread::available_parallelism().map_or(1, |count| count.get().min(8))
}

fn extract_in_parallel(
    request: &ExtractRequest<'_>,
    archive: &Archive,
    family: ArchiveFamily,
    password: Option<&[u8]>,
    cancel: &ReadCancellation,
    total: u64,
    emit: ProgressFn,
) -> Result<ExtractReport, AppError> {
    fs::create_dir_all(request.out_dir)?;
    let out_dir = fs::canonicalize(request.out_dir)?;
    let tracker = Tracker::new(total, emit);
    let states = (0..worker_count())
        .map(|worker| {
            let staging = out_dir.join(format!(".rawr-tmp-{}-{worker}", std::process::id()));
            Extraction::new(&out_dir, staging, family, request, tracker.clone())
        })
        .collect();
    let (states, error) = with_workers(
        archive,
        family,
        password,
        cancel,
        request.selected.as_ref(),
        states,
        |state, meta| state.open_entry(meta),
        |state| {
            state.publish()?;
            state.finish_publishing()
        },
    );
    let mut report = ExtractReport::default();
    for mut state in states {
        let _ = state.finish_publishing();
        // An unpublished file failed or wasn't finished: it goes with the staging folder.
        let _ = fs::remove_dir_all(&state.staging);
        report.written += state.report.written;
        report.skipped_existing += state.report.skipped_existing;
        report.warnings.extend(state.report.warnings);
    }
    match error {
        Some(error) => Err(error.into()),
        None => Ok(report),
    }
}

fn extract_skipping(
    archive: &Archive,
    family: ArchiveFamily,
    options: ArchiveReadOptions<'_>,
    state: &RefCell<Extraction<'_>>,
) -> rars::Result<()> {
    archive
        .extract_with_control(options, |member| {
            let name = display_name(family, &member.meta.name, member.name_is_unicode());
            if state
                .borrow()
                .selected
                .is_some_and(|set| !set.contains(&name))
            {
                return Ok(ExtractionDecision::Skip);
            }
            let mut meta = ExtractedEntryMeta::new(
                member.meta.name.clone(),
                member.meta.file_time,
                member.meta.file_attr,
                member.meta.is_directory,
            )
            .with_attr_source(member.meta.attr_source())
            .with_mtime_refinement(member.meta.mtime_refinement);
            meta.name_is_unicode = member.name_is_unicode();
            Ok(ExtractionDecision::Extract(
                state.borrow_mut().open_entry(&meta)?,
            ))
        })
        .map(|_| ())
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
    let kind = others::kind_of(archive);
    if kind != Kind::Rar {
        return others::test(kind, archive, password, cancel, emit);
    }
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
    if independent_members(&archives, family, None) {
        let tested = AtomicUsize::new(0);
        let (_, error) = with_workers(
            &archives[0],
            family,
            password,
            cancel,
            None,
            vec![(); worker_count()],
            |(), meta| {
                let name = display_name(family, &meta.name, meta.name_is_unicode);
                tracker.set_current(&name);
                tested.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(CountingSink(tracker.clone())))
            },
            |()| Ok(()),
        );
        return match error {
            Some(error) => Err(error.into()),
            None => Ok(tested.into_inner()),
        };
    }
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
