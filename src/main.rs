#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

use rars::ReadCancellation;
use rawr::authoring::{self, CreateReport, CreateRequest, Edit, Format};
use rawr::engine::{self, AppError, ExtractReport, ExtractRequest, OpenedArchive, Progress};
use rawr::listing::{self, Row, SortBy};
use rawr::shell;
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
enum Job {
    Open,
    Extract {
        out_dir: String,
        selected: Option<HashSet<String>>,
        overwrite: bool,
        /// Open this extracted file afterwards rather than reporting.
        launch: Option<PathBuf>,
    },
    Test,
    Create {
        output: PathBuf,
        inputs: Vec<PathBuf>,
        settings: CreateSettings,
    },
    Edit(Edit),
    Repair {
        output: PathBuf,
    },
}

#[derive(Clone)]
struct CreateSettings {
    format: Format,
    level: u8,
    solid: bool,
    password: Option<String>,
    encrypt_names: bool,
    comment: Option<String>,
    recovery_percent: Option<u64>,
    volume_bytes: Option<u64>,
}

enum Outcome {
    Opened(OpenedArchive),
    Extracted(ExtractReport, String, Option<PathBuf>),
    Tested(usize),
    Created(CreateReport),
    Edited(Vec<String>),
    Repaired(String, PathBuf),
}

#[derive(Clone)]
enum Confirm {
    Delete(HashSet<String>),
    Launch(String),
}

#[derive(Default)]
struct State {
    path: String,
    password: Option<String>,
    archive: Option<OpenedArchive>,
    folder: String,
    rows: Vec<Row>,
    /// Rows selected, by position in `rows`.
    selected: HashSet<usize>,
    anchor: Option<usize>,
    sort: SortBy,
    descending: bool,
    job: Option<Job>,
    cancel: Option<ReadCancellation>,
    started: Option<Instant>,
    closing: bool,
    create_inputs: Vec<PathBuf>,
    /// What the notice's button does.
    notice_folder: Option<PathBuf>,
    notice_details: Vec<String>,
    notice_repair: bool,
    rename_from: Option<String>,
    confirm: Option<Confirm>,
    /// Extract straight away once this opens (Explorer's "Extract here").
    extract_on_open: bool,
    temp_dir: Option<PathBuf>,
}

/// Everything on the window thread. Worker threads reach it through `with_app`.
struct App {
    window: AppWindow,
    state: RefCell<State>,
    rows: Rc<VecModel<FileRow>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn with_app(work: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|slot| slot.borrow().clone());
    if let Some(app) = app {
        work(&app);
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn plural(count: usize, one: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {one}s")
    }
}

impl App {
    // ---- list ----

    fn searching(&self) -> Option<String> {
        let query = self.window.get_search_text();
        (!query.trim().is_empty()).then(|| query.trim().to_string())
    }

    fn refresh_rows(&self) {
        let mut state = self.state.borrow_mut();
        let Some(archive) = &state.archive else {
            state.rows.clear();
            self.rows.set_vec(Vec::new());
            return;
        };
        let mut rows = match self.searching() {
            Some(query) => listing::search(&archive.entries, &query),
            None => listing::rows_in(&archive.entries, &state.folder),
        };
        listing::sort_rows(&mut rows, state.sort, !state.descending);
        let ui_rows: Vec<FileRow> = rows.iter().map(ui_row).collect();
        state.rows = rows;
        state.selected.clear();
        state.anchor = None;
        drop(state);
        self.rows.set_vec(ui_rows);
        self.window.set_selected_count(0);
        self.refresh_crumbs();
    }

    fn refresh_crumbs(&self) {
        let state = self.state.borrow();
        let root = Crumb {
            label: "Archive".into(),
            path: "".into(),
        };
        let mut crumbs = vec![root];
        if self.searching().is_some() {
            crumbs[0].label = match state.rows.len() {
                1 => "1 match in this archive".to_owned(),
                count => format!("{count} matches in this archive"),
            }
            .into();
        } else {
            let mut path = String::new();
            for part in state.folder.split('/').filter(|part| !part.is_empty()) {
                path.push_str(part);
                path.push('/');
                crumbs.push(Crumb {
                    label: part.into(),
                    path: path.as_str().into(),
                });
            }
        }
        self.window
            .set_crumbs(ModelRc::new(VecModel::from(crumbs)));
    }

    fn sync_selection(&self) {
        let state = self.state.borrow();
        for index in 0..self.rows.row_count() {
            if let Some(mut row) = self.rows.row_data(index) {
                let selected = state.selected.contains(&index);
                if row.selected != selected {
                    row.selected = selected;
                    self.rows.set_row_data(index, row);
                }
            }
        }
        self.window.set_selected_count(state.selected.len() as i32);
    }

    fn enter_folder(&self, folder: String) {
        self.state.borrow_mut().folder = folder;
        self.window.set_search_text("".into());
        self.refresh_rows();
    }

    fn click_row(&self, index: usize, control: bool, shift: bool) {
        {
            let mut state = self.state.borrow_mut();
            if shift {
                let anchor = state.anchor.unwrap_or(index);
                let (low, high) = (anchor.min(index), anchor.max(index));
                if !control {
                    state.selected.clear();
                }
                state.selected.extend(low..=high);
            } else if control {
                if !state.selected.remove(&index) {
                    state.selected.insert(index);
                }
                state.anchor = Some(index);
            } else {
                state.selected.clear();
                state.selected.insert(index);
                state.anchor = Some(index);
            }
        }
        self.sync_selection();
    }

    fn chosen_names(&self) -> Option<HashSet<String>> {
        let state = self.state.borrow();
        let archive = state.archive.as_ref()?;
        if state.selected.is_empty() {
            return None;
        }
        let chosen = state.selected.iter().filter_map(|&i| state.rows.get(i));
        Some(listing::expand_selection(&archive.entries, chosen))
    }

    fn activate_row(self: &Rc<Self>, index: usize) {
        let row = self.state.borrow().rows.get(index).cloned();
        let Some(row) = row else { return };
        if row.is_folder {
            self.enter_folder(row.path);
        } else if listing::runs_code(&row.name) {
            self.state.borrow_mut().confirm = Some(Confirm::Launch(row.path));
            self.window.set_confirm_title("Run this program?".into());
            self.window.set_confirm_body(
                format!("{} can run code on your PC. Only open it if you trust where this archive came from.", row.name).into(),
            );
            self.window.set_confirm_action("Open".into());
            self.window.set_sheet(Sheet::Confirm);
        } else {
            self.launch(row.path);
        }
    }

    /// Extracts one file to a private temp folder and opens it.
    fn launch(self: &Rc<Self>, entry: String) {
        let dir = {
            let mut state = self.state.borrow_mut();
            state
                .temp_dir
                .get_or_insert_with(|| {
                    std::env::temp_dir().join(format!("rawr-{}", std::process::id()))
                })
                .clone()
        };
        let launch = engine::safe_relative(&entry).ok().map(|rel| dir.join(rel));
        let Some(launch) = launch else {
            self.notify(3, format!("{entry} has a name Windows can't use."));
            return;
        };
        self.start(Job::Extract {
            out_dir: dir.display().to_string(),
            selected: Some(HashSet::from([entry])),
            overwrite: true,
            launch: Some(launch),
        });
    }

    // ---- jobs ----

    fn notify(&self, kind: i32, text: impl Into<SharedString>) {
        let mut state = self.state.borrow_mut();
        state.notice_folder = None;
        state.notice_details.clear();
        state.notice_repair = false;
        self.window.set_notice_kind(kind);
        self.window.set_notice(text.into());
        self.window.set_notice_action("".into());
    }

    fn start(self: &Rc<Self>, job: Job) {
        let cancel = ReadCancellation::new();
        let (path, password) = {
            let mut state = self.state.borrow_mut();
            state.job = Some(job.clone());
            state.cancel = Some(cancel.clone());
            state.started = Some(Instant::now());
            (state.path.clone(), state.password.clone())
        };
        self.window.set_notice("".into());
        self.window.set_cancelling(false);
        self.window.set_busy_fraction(0.0);
        self.window.set_busy_stats("".into());
        self.window.set_busy_detail("".into());
        self.window.set_busy_title(
            match &job {
                Job::Open => "Opening",
                Job::Extract { launch: Some(_), .. } => "Preparing file",
                Job::Extract { .. } => "Extracting",
                Job::Test => "Testing",
                Job::Create { .. } => "Creating archive",
                Job::Edit(_) => "Updating archive",
                Job::Repair { .. } => "Repairing",
            }
            .into(),
        );
        self.window.set_busy(true);

        std::thread::spawn(move || {
            let emit: engine::ProgressFn = Arc::new(|progress: Progress| {
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| app.on_progress(progress));
                });
            });
            let result = run_job(job, &path, password.as_deref(), &cancel, emit);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.finish(result));
            });
        });
    }

    fn on_progress(&self, progress: Progress) {
        self.window.set_busy_detail(progress.current.as_str().into());
        if progress.total_bytes == 0 {
            return;
        }
        let fraction = progress.done_bytes as f64 / progress.total_bytes as f64;
        self.window.set_busy_fraction(fraction as f32);
        let started = self.state.borrow().started;
        let elapsed = started.map_or(0.0, |at| at.elapsed().as_secs_f64());
        let mut stats = format!("{:.0}%", (fraction * 100.0).min(100.0));
        if elapsed > 1.0 && progress.done_bytes > 0 {
            let speed = progress.done_bytes as f64 / elapsed;
            stats += &format!("  ·  {}/s", listing::format_size(speed as u64));
            let left = progress.total_bytes.saturating_sub(progress.done_bytes) as f64 / speed;
            stats += &format!("  ·  {} left", listing::format_duration(left as u64));
        }
        self.window.set_busy_stats(stats.into());
    }

    fn finish(self: &Rc<Self>, result: Result<Outcome, AppError>) {
        self.window.set_busy(false);
        if self.state.borrow().closing {
            let _ = slint::quit_event_loop();
            return;
        }
        let job = self.state.borrow().job.clone();
        match result {
            Ok(outcome) => self.on_success(outcome),
            Err(error) if matches!(error.kind, "password_required" | "bad_password") => {
                self.window.set_password_prompt(
                    if error.kind == "bad_password" {
                        "That password didn't work, or the data is damaged. Try again."
                    } else {
                        "This archive is protected. Enter its password."
                    }
                    .into(),
                );
                self.window.set_password_text("".into());
                self.window.set_sheet(Sheet::Password);
            }
            Err(error) if error.kind == "cancelled" => {
                let text = match job {
                    Some(Job::Extract { .. }) => {
                        "Cancelled. Files finished before that were kept."
                    }
                    Some(Job::Edit(_)) => "Cancelled. The archive is unchanged.",
                    _ => "Cancelled.",
                };
                self.notify(0, text);
            }
            Err(error) => self.on_failure(job, error),
        }
    }

    fn on_failure(&self, job: Option<Job>, error: AppError) {
        let text = match job {
            Some(Job::Open) => format!("Couldn't open this archive: {}", error.message),
            Some(Job::Test) => format!("Problem found: {}", error.message),
            Some(Job::Create { .. }) => format!("Couldn't create the archive: {}", error.message),
            Some(Job::Edit(_)) => format!("Archive unchanged: {}", error.message),
            Some(Job::Repair { .. }) => format!("Couldn't repair it: {}", error.message),
            _ => format!("Extraction stopped: {}", error.message),
        };
        self.notify(3, text.as_str());
        // A damaged archive is exactly what repair is for.
        if matches!(job, Some(Job::Open | Job::Test)) {
            self.state.borrow_mut().notice_repair = true;
            self.window.set_notice_action("Repair".into());
        } else {
            self.state.borrow_mut().notice_details = vec![text];
            self.window.set_notice_action("Details".into());
        }
    }

    fn on_success(self: &Rc<Self>, outcome: Outcome) {
        match outcome {
            Outcome::Opened(archive) => self.on_opened(archive),
            Outcome::Extracted(_, _, Some(file)) => {
                if let Err(error) = open::that_detached(&file) {
                    self.notify(3, format!("Couldn't open it: {error}"));
                }
            }
            Outcome::Extracted(report, out_dir, None) => {
                let mut text = format!(
                    "Extracted {}",
                    plural(report.written, "file")
                );
                if report.skipped_existing > 0 {
                    text += &format!(", skipped {} already there", report.skipped_existing);
                }
                let warnings = report.warnings;
                self.notify(if warnings.is_empty() { 1 } else { 2 }, text);
                let mut state = self.state.borrow_mut();
                if warnings.is_empty() {
                    state.notice_folder = Some(PathBuf::from(out_dir));
                    self.window.set_notice_action("Show folder".into());
                } else {
                    state.notice_details = warnings;
                    self.window.set_notice_action("Details".into());
                }
            }
            Outcome::Tested(count) => {
                self.notify(1, format!("All good: {} checked.", plural(count, "item")));
            }
            Outcome::Created(report) => {
                let first = report.parts[0].display().to_string();
                self.state.borrow_mut().path = first;
                self.state.borrow_mut().create_inputs.clear();
                self.start(Job::Open);
                if !report.warnings.is_empty() {
                    self.state.borrow_mut().notice_details = report.warnings;
                }
            }
            Outcome::Edited(warnings) => {
                self.state.borrow_mut().notice_details = warnings;
                self.start(Job::Open);
            }
            Outcome::Repaired(summary, output) => {
                self.notify(1, format!("{summary} Saved as {}", file_name(&output.display().to_string())));
                self.state.borrow_mut().notice_folder =
                    output.parent().map(Path::to_path_buf);
                self.window.set_notice_action("Show folder".into());
            }
        }
    }

    fn on_opened(self: &Rc<Self>, archive: OpenedArchive) {
        let name = file_name(&archive.path).to_owned();
        self.window.set_window_title(format!("{name} - rawr").into());
        self.window.set_archive_name(name.as_str().into());
        self.window.set_archive_detail(detail_line(&archive).into());
        self.window.set_info_text(info_text(&archive).into());
        self.window
            .set_info_comment(archive.comment.clone().unwrap_or_default().into());
        self.window.set_can_edit(archive.parts == 1);
        self.window.set_has_archive(true);
        let warnings = archive.warnings.clone();
        let extract_now = {
            let mut state = self.state.borrow_mut();
            // An edit keeps your place; a different archive starts at the top.
            let keep = state
                .archive
                .as_ref()
                .is_some_and(|old| old.path == archive.path);
            if !keep {
                state.folder.clear();
                self.window.set_extract_dest("".into());
            }
            state.archive = Some(archive);
            std::mem::take(&mut state.extract_on_open)
        };
        self.window.set_search_text("".into());
        self.refresh_rows();
        self.window.invoke_focus_list();
        let pending: Vec<String> = std::mem::take(&mut self.state.borrow_mut().notice_details);
        let all: Vec<String> = warnings.into_iter().chain(pending).collect();
        if !all.is_empty() {
            self.notify(2, plural(all.len(), "thing").to_string() + " to check");
            self.state.borrow_mut().notice_details = all;
            self.window.set_notice_action("Details".into());
        } else {
            self.window.set_notice("".into());
        }
        if extract_now {
            let path = self.state.borrow().path.clone();
            self.start(Job::Extract {
                out_dir: listing::default_destination(&path),
                selected: None,
                overwrite: false,
                launch: None,
            });
        }
    }

    // ---- actions ----

    fn open_path(self: &Rc<Self>, path: String) {
        if self.window.get_busy() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if state.path != path {
                state.password = None;
            }
            state.path = path;
        }
        self.start(Job::Open);
    }

    /// Whatever was dropped: an archive opens, anything else goes in a new one or
    /// the open archive.
    fn dropped(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if self.window.get_busy() || paths.is_empty() {
            return;
        }
        let looks_like_archive = |path: &Path| {
            path.extension().is_some_and(|extension| {
                let extension = extension.to_string_lossy().to_ascii_lowercase();
                extension == "rar"
                    || (extension.len() == 3
                        && extension.starts_with('r')
                        && extension[1..].bytes().all(|b| b.is_ascii_digit()))
            })
        };
        if paths.len() == 1 && looks_like_archive(&paths[0]) {
            self.open_path(paths[0].display().to_string());
        } else if self.window.get_has_archive() && self.window.get_can_edit() {
            let into = self.state.borrow().folder.clone();
            self.start(Job::Edit(Edit::Add { inputs: paths, into }));
        } else {
            self.begin_create(paths);
        }
    }

    fn begin_create(&self, inputs: Vec<PathBuf>) {
        let first = inputs[0].clone();
        let stem = first
            .file_stem()
            .map_or("archive".to_owned(), |s| s.to_string_lossy().into_owned());
        let output = first
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(if inputs.len() == 1 {
                format!("{stem}.rar")
            } else {
                "archive.rar".to_owned()
            });
        self.window.set_create_summary(
            if inputs.len() == 1 {
                format!("Adding {}", file_name(&first.display().to_string()))
            } else {
                format!("Adding {}", plural(inputs.len(), "item"))
            }
            .into(),
        );
        self.window
            .set_create_output(output.display().to_string().into());
        self.window.set_create_password("".into());
        self.window.set_create_comment("".into());
        self.window.set_create_hide_names(false);
        self.state.borrow_mut().create_inputs = inputs;
        self.window.set_sheet(Sheet::Create);
    }

    fn confirm_create(self: &Rc<Self>) {
        let window = &self.window;
        let password = window.get_create_password().to_string();
        let comment = window.get_create_comment().to_string();
        let settings = CreateSettings {
            format: match window.get_create_format() {
                0 => Format::Rar4,
                2 => Format::Rar7,
                _ => Format::Rar5,
            },
            level: [0, 1, 3, 5][window.get_create_level().clamp(0, 3) as usize],
            solid: window.get_create_solid(),
            encrypt_names: window.get_create_hide_names() && !password.is_empty(),
            password: (!password.is_empty()).then_some(password),
            comment: (!comment.is_empty()).then_some(comment),
            recovery_percent: [None, Some(3), Some(5), Some(10)]
                [window.get_create_recovery().clamp(0, 3) as usize],
            volume_bytes: [
                None,
                Some(100 * 1024 * 1024),
                Some(700 * 1024 * 1024),
                Some(1024 * 1024 * 1024),
                Some(4 * 1024 * 1024 * 1024),
            ][window.get_create_split().clamp(0, 4) as usize],
        };
        let mut output = PathBuf::from(window.get_create_output().trim());
        if output.extension().is_none_or(|extension| !extension.eq_ignore_ascii_case("rar")) {
            output.as_mut_os_string().push(".rar");
        }
        let inputs = self.state.borrow().create_inputs.clone();
        {
            let mut state = self.state.borrow_mut();
            state.password = settings.password.clone();
        }
        window.set_sheet(Sheet::None);
        self.start(Job::Create {
            output,
            inputs,
            settings,
        });
    }

    fn begin_extract(&self) {
        let (path, count) = {
            let state = self.state.borrow();
            (state.path.clone(), state.selected.len())
        };
        self.window.set_extract_title(
            if count == 0 {
                "Extract everything".to_owned()
            } else {
                format!("Extract {}", plural(count, "item"))
            }
            .into(),
        );
        if self.window.get_extract_dest().is_empty() {
            self.window
                .set_extract_dest(listing::default_destination(&path).into());
        }
        self.window.set_sheet(Sheet::Extract);
    }

    fn begin_rename(&self) {
        let state = self.state.borrow();
        let single = (state.selected.len() == 1)
            .then(|| state.selected.iter().next().and_then(|&i| state.rows.get(i)))
            .flatten()
            .cloned();
        drop(state);
        let Some(row) = single else { return };
        let from = row.path.trim_end_matches('/').to_owned();
        let leaf = from.rsplit('/').next().unwrap_or(&from).to_owned();
        self.state.borrow_mut().rename_from = Some(from);
        self.window.set_rename_text(leaf.as_str().into());
        self.window.set_sheet(Sheet::Rename);
    }

    fn confirm_rename(self: &Rc<Self>) {
        let from = self.state.borrow_mut().rename_from.take();
        let Some(from) = from else { return };
        self.window.set_sheet(Sheet::None);
        let new_leaf = self.window.get_rename_text().trim().to_owned();
        if new_leaf.is_empty() || new_leaf.contains(['/', '\\']) {
            self.notify(3, "A name can't be empty or contain slashes.");
            return;
        }
        let to = match from.rfind('/') {
            Some(slash) => format!("{}{new_leaf}", &from[..=slash]),
            None => new_leaf,
        };
        if to != from {
            self.start(Job::Edit(Edit::Rename { from, to }));
        }
    }

    fn begin_delete(&self) {
        let Some(names) = self.chosen_names() else {
            return;
        };
        let count = self.state.borrow().selected.len();
        self.window
            .set_confirm_title(format!("Delete {} from the archive?", plural(count, "item")).into());
        self.window
            .set_confirm_body("This changes the archive itself, and can't be undone.".into());
        self.window.set_confirm_action("Delete".into());
        self.state.borrow_mut().confirm = Some(Confirm::Delete(names));
        self.window.set_sheet(Sheet::Confirm);
    }

    fn accept_confirm(self: &Rc<Self>) {
        self.window.set_sheet(Sheet::None);
        let confirm = self.state.borrow_mut().confirm.take();
        match confirm {
            Some(Confirm::Delete(names)) => self.start(Job::Edit(Edit::Delete(names))),
            Some(Confirm::Launch(entry)) => self.launch(entry),
            None => {}
        }
    }

    fn navigate_key(self: &Rc<Self>, key: &str, shift: bool) -> bool {
        let count = self.state.borrow().rows.len();
        let cursor = self.state.borrow().anchor;
        let target = match key {
            "up" => Some(cursor.map_or(0, |at| at.saturating_sub(1))),
            "down" => Some(cursor.map_or(0, |at| (at + 1).min(count.saturating_sub(1)))),
            "home" => Some(0),
            "end" => Some(count.saturating_sub(1)),
            "enter" => {
                if let Some(index) = cursor {
                    self.activate_row(index);
                }
                return true;
            }
            "back" => {
                let folder = self.state.borrow().folder.clone();
                if !folder.is_empty() {
                    self.enter_folder(listing::parent(&folder).to_owned());
                }
                return true;
            }
            "delete" => {
                if self.window.get_can_edit() {
                    self.begin_delete();
                }
                return true;
            }
            "rename" => {
                if self.window.get_can_edit() {
                    self.begin_rename();
                }
                return true;
            }
            "all" => {
                let mut state = self.state.borrow_mut();
                state.selected = (0..count).collect();
                drop(state);
                self.sync_selection();
                return true;
            }
            _ => return false,
        };
        if let (Some(index), true) = (target, count > 0) {
            if shift {
                let mut state = self.state.borrow_mut();
                state.selected.insert(index);
                state.anchor = Some(index);
                drop(state);
                self.sync_selection();
            } else {
                self.click_row(index, false, false);
            }
            self.window.invoke_show_row(index as i32);
        }
        true
    }

    fn apply_theme(&self, choice: i32) {
        let scheme = match choice {
            1 => slint::language::ColorScheme::Light,
            2 => slint::language::ColorScheme::Dark,
            _ => slint::language::ColorScheme::Unknown,
        };
        self.window.global::<Palette>().set_color_scheme(scheme);
        let theme = match choice {
            1 => Some(winit::window::Theme::Light),
            2 => Some(winit::window::Theme::Dark),
            _ => None,
        };
        let _ = self
            .window
            .window()
            .with_winit_window(|window| window.set_theme(theme));
    }

    fn notice_action(self: &Rc<Self>) {
        let (folder, details, repair) = {
            let state = self.state.borrow();
            (
                state.notice_folder.clone(),
                state.notice_details.clone(),
                state.notice_repair,
            )
        };
        if let Some(folder) = folder {
            let _ = open::that_detached(folder);
        } else if repair {
            self.begin_repair();
        } else if !details.is_empty() {
            self.window.set_details_title("Details".into());
            self.window.set_details_text(details.join("\n").into());
            self.window.set_sheet(Sheet::Details);
        }
    }

    fn begin_repair(self: &Rc<Self>) {
        let path = self.state.borrow().path.clone();
        if path.is_empty() {
            return;
        }
        let dialog = rfd::FileDialog::new()
            .set_title("Save repaired copy as")
            .add_filter("RAR archive", &["rar"])
            .set_file_name(file_name(&listing::repaired_name(&path)));
        let dialog = in_folder_of(dialog, &path);
        if let Some(output) = dialog.save_file() {
            self.start(Job::Repair { output });
        }
    }
}

fn in_folder_of(dialog: rfd::FileDialog, path: &str) -> rfd::FileDialog {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => dialog.set_directory(parent),
        _ => dialog,
    }
}

fn run_job(
    job: Job,
    path: &str,
    password: Option<&str>,
    cancel: &ReadCancellation,
    emit: engine::ProgressFn,
) -> Result<Outcome, AppError> {
    let archive = PathBuf::from(path);
    match job {
        Job::Open => engine::open(&archive, password, cancel).map(Outcome::Opened),
        Job::Test => engine::test(&archive, password, cancel, emit).map(Outcome::Tested),
        Job::Extract {
            out_dir,
            selected,
            overwrite,
            launch,
        } => engine::extract(
            ExtractRequest {
                archive: &archive,
                out_dir: Path::new(&out_dir),
                password,
                selected,
                overwrite,
            },
            cancel,
            emit,
        )
        .map(|report| Outcome::Extracted(report, out_dir, launch)),
        Job::Create {
            output,
            inputs,
            settings,
        } => authoring::create(
            CreateRequest {
                output: &output,
                inputs: &inputs,
                format: settings.format,
                level: settings.level,
                solid: settings.solid,
                password: settings.password.as_deref(),
                encrypt_names: settings.encrypt_names,
                comment: settings.comment.as_deref(),
                recovery_percent: settings.recovery_percent,
                volume_bytes: settings.volume_bytes,
            },
            cancel,
            emit,
        )
        .map(Outcome::Created),
        Job::Edit(change) => {
            authoring::edit(&archive, password, change, cancel, emit).map(Outcome::Edited)
        }
        Job::Repair { output } => authoring::repair(&archive, &output, password, cancel)
            .map(|summary| Outcome::Repaired(summary, output)),
    }
}

fn ui_row(row: &Row) -> FileRow {
    FileRow {
        name: row.name.as_str().into(),
        size: if row.is_folder && row.size == 0 {
            "".into()
        } else {
            listing::format_size(row.size).into()
        },
        packed: if row.is_folder && row.packed == 0 {
            "".into()
        } else {
            listing::format_size(row.packed).into()
        },
        modified: listing::format_date(row.modified_ms).into(),
        badge: if row.link {
            "link".into()
        } else if row.encrypted {
            "locked".into()
        } else {
            "".into()
        },
        kind: if row.is_folder {
            -1
        } else {
            listing::kind_of(&row.name)
        },
        selected: false,
    }
}

fn totals(archive: &OpenedArchive) -> (usize, u64, u64) {
    archive
        .entries
        .iter()
        .filter(|entry| !entry.is_directory)
        .fold((0, 0, 0), |(count, size, packed), entry| {
            (
                count + 1,
                size + entry.unpacked_size,
                packed + entry.packed_size,
            )
        })
}

fn detail_line(archive: &OpenedArchive) -> String {
    let (count, size, _) = totals(archive);
    let mut parts = vec![
        plural(count, "file"),
        listing::format_size(size),
        archive.format.to_owned(),
    ];
    if archive.parts > 1 {
        parts.push(format!("{} parts", archive.parts));
    }
    if archive.any_encrypted {
        parts.push("locked".to_owned());
    }
    parts.join("  ·  ")
}

fn info_text(archive: &OpenedArchive) -> String {
    let (count, size, packed) = totals(archive);
    let ratio = if size > 0 {
        format!("{:.0}%", packed as f64 / size as f64 * 100.0)
    } else {
        "-".to_owned()
    };
    let yes_no = |value: bool| if value { "Yes" } else { "No" };
    format!(
        "Format:  {}\nLocation:  {}\nFiles:  {count}\nSize:  {}\nPacked:  {} ({ratio})\nParts:  {}\nSolid:  {}\nPassword protected:  {}",
        archive.format,
        archive.path,
        listing::format_size(size),
        listing::format_size(packed),
        archive.parts,
        yes_no(archive.is_solid),
        yes_no(archive.any_encrypted),
    )
}

fn pick_files() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Add files")
        .pick_files()
        .unwrap_or_default()
}

/// Asks to close. A running job gets to clean up its temporary files first.
fn request_close() -> slint::CloseRequestResponse {
    let mut response = slint::CloseRequestResponse::HideWindow;
    with_app(|app| {
        if app.window.get_busy() {
            app.state.borrow_mut().closing = true;
            app.window.set_cancelling(true);
            if let Some(token) = &app.state.borrow().cancel {
                token.cancel();
            }
            response = slint::CloseRequestResponse::KeepWindowShown;
        } else {
            let _ = slint::quit_event_loop();
        }
    });
    response
}

fn resize_direction(edge: &str) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection as D;
    Some(match edge {
        "n" => D::North,
        "s" => D::South,
        "e" => D::East,
        "w" => D::West,
        "ne" => D::NorthEast,
        "nw" => D::NorthWest,
        "se" => D::SouthEast,
        "sw" => D::SouthWest,
        _ => return None,
    })
}

fn wire(app: &Rc<App>) {
    let window = &app.window;
    macro_rules! on {
        ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {
            window.$setter(move |$($arg),*| {
                let mut value = Default::default();
                with_app(|$app| value = $body);
                value
            });
        };
    }

    let last_press = std::cell::Cell::new(None::<Instant>);
    window.on_title_pressed(move || {
        let double = last_press
            .get()
            .is_some_and(|at: Instant| at.elapsed().as_millis() < 400);
        last_press.set((!double).then(Instant::now));
        with_app(|app| {
            let window = app.window.window();
            if double {
                window.set_maximized(!window.is_maximized());
            } else {
                let _ = window.with_winit_window(|window| window.drag_window());
            }
        });
    });
    on!(on_minimize_clicked, |app| app.window.window().set_minimized(true));
    on!(on_maximize_clicked, |app| {
        let window = app.window.window();
        window.set_maximized(!window.is_maximized());
    });
    window.on_close_clicked(|| {
        if request_close() == slint::CloseRequestResponse::HideWindow {
            with_app(|app| {
                let _ = app.window.hide();
            });
        }
    });
    on!(on_resize_from, |app, edge| {
        if let Some(direction) = resize_direction(&edge) {
            let _ = app
                .window
                .window()
                .with_winit_window(|window| window.drag_resize_window(direction));
        }
    });

    on!(on_open_clicked, |app| {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open archive")
            .add_filter("RAR archives", &["rar", "r00", "exe"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            app.open_path(path.display().to_string());
        }
    });
    on!(on_new_clicked, |app| {
        let inputs = pick_files();
        if !inputs.is_empty() {
            app.begin_create(inputs);
        }
    });
    on!(on_add_files_clicked, |app| {
        let inputs = pick_files();
        if !inputs.is_empty() {
            app.dropped(inputs);
        }
    });
    on!(on_add_folder_clicked, |app| {
        if let Some(folder) = rfd::FileDialog::new().set_title("Add folder").pick_folder() {
            app.dropped(vec![folder]);
        }
    });
    on!(on_extract_clicked, |app| app.begin_extract());
    on!(on_test_clicked, |app| app.start(Job::Test));
    on!(on_repair_clicked, |app| app.begin_repair());
    on!(on_info_clicked, |app| app.window.set_sheet(Sheet::Info));
    on!(on_about_clicked, |app| app.window.set_sheet(Sheet::About));
    on!(on_settings_clicked, |app| {
        app.window.set_setting_context_menu(shell::context_menu_enabled());
        app.window.set_setting_association(shell::association_enabled());
        app.window.set_settings_note("".into());
        app.window.set_sheet(Sheet::Settings);
    });
    on!(on_close_archive_clicked, |app| {
        {
            let mut state = app.state.borrow_mut();
            state.archive = None;
            state.path.clear();
            state.password = None;
            state.folder.clear();
        }
        app.window.set_has_archive(false);
        app.window.set_window_title("rawr".into());
        app.window.set_extract_dest("".into());
        app.window.set_notice("".into());
        app.refresh_rows();
    });
    on!(on_cancel_clicked, |app| {
        app.window.set_cancelling(true);
        if let Some(token) = &app.state.borrow().cancel {
            token.cancel();
        }
    });
    on!(on_notice_action_clicked, |app| app.notice_action());
    on!(on_notice_dismissed, |app| app.window.set_notice("".into()));
    on!(on_crumb_clicked, |app, path| app.enter_folder(path.to_string()));
    on!(on_search_edited, |app| app.refresh_rows());
    on!(on_sort_clicked, |app, column| {
        {
            let mut state = app.state.borrow_mut();
            let by = match column {
                1 => SortBy::Size,
                2 => SortBy::Packed,
                3 => SortBy::Modified,
                _ => SortBy::Name,
            };
            state.descending = state.sort == by && !state.descending;
            state.sort = by;
            app.window.set_sort_ascending(!state.descending);
        }
        app.window.set_sort_column(column);
        app.refresh_rows();
    });
    on!(on_row_clicked, |app, index, control, shift| {
        app.click_row(index as usize, control, shift)
    });
    on!(on_row_activated, |app, index| app.activate_row(index as usize));
    on!(on_row_menu, |app, index, action| {
        let index = index as usize;
        // Acting on a row outside the selection means that row alone.
        if !app.state.borrow().selected.contains(&index) {
            app.click_row(index, false, false);
        }
        match action.as_str() {
            "open" => app.activate_row(index),
            "extract" => app.begin_extract(),
            "rename" => {
                app.click_row(index, false, false);
                app.begin_rename();
            }
            "delete" => app.begin_delete(),
            _ => {}
        }
    });
    on!(on_background_clicked, |app| {
        app.state.borrow_mut().selected.clear();
        app.state.borrow_mut().anchor = None;
        app.sync_selection();
    });
    on!(on_navigate_key, |app, key, shift| app.navigate_key(&key, shift));
    on!(on_password_submitted, |app| {
        app.state.borrow_mut().password = Some(app.window.get_password_text().to_string());
        app.window.set_password_text("".into());
        app.window.set_sheet(Sheet::None);
        let job = app.state.borrow().job.clone();
        if let Some(job) = job {
            app.start(job);
        }
    });
    on!(on_password_cancelled, |app| {
        app.state.borrow_mut().extract_on_open = false;
        app.window.set_password_text("".into());
        app.window.set_sheet(Sheet::None);
    });
    on!(on_extract_confirmed, |app| {
        app.window.set_sheet(Sheet::None);
        let selected = app.chosen_names();
        app.start(Job::Extract {
            out_dir: app.window.get_extract_dest().trim().to_owned(),
            selected,
            overwrite: app.window.get_extract_overwrite() == 1,
            launch: None,
        });
    });
    on!(on_browse_extract, |app| {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Extract to")
            .pick_folder()
        {
            app.window.set_extract_dest(folder.display().to_string().into());
        }
    });
    on!(on_create_confirmed, |app| app.confirm_create());
    on!(on_browse_create, |app| {
        let current = app.window.get_create_output().to_string();
        let dialog = rfd::FileDialog::new()
            .set_title("Save archive as")
            .add_filter("RAR archive", &["rar"])
            .set_file_name(file_name(&current));
        if let Some(path) = in_folder_of(dialog, &current).save_file() {
            app.window.set_create_output(path.display().to_string().into());
        }
    });
    on!(on_rename_confirmed, |app| app.confirm_rename());
    on!(on_confirm_accepted, |app| app.accept_confirm());
    on!(on_confirm_declined, |app| {
        app.state.borrow_mut().confirm = None;
        app.window.set_sheet(Sheet::None);
    });
    on!(on_theme_changed, |app, choice| {
        app.apply_theme(choice);
        shell::set_theme(choice);
    });
    on!(on_context_menu_toggled, |app, enabled| {
        match shell::set_context_menu(enabled) {
            Ok(()) => app.window.set_settings_note(
                "On Windows 11 these appear under \"Show more options\". They use this copy of rawr, so toggle again if you move it.".into(),
            ),
            Err(error) => app.window.set_settings_note(format!("Couldn't change that: {error}").into()),
        }
        app.window.set_setting_context_menu(shell::context_menu_enabled());
    });
    on!(on_association_toggled, |app, enabled| {
        match shell::set_association(enabled) {
            Ok(()) => app.window.set_settings_note(
                "If another app is your default for .rar, choose rawr once with Open with > Choose another app.".into(),
            ),
            Err(error) => app.window.set_settings_note(format!("Couldn't change that: {error}").into()),
        }
        app.window.set_setting_association(shell::association_enabled());
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    let rows = Rc::new(VecModel::default());
    window.set_rows(ModelRc::from(rows.clone()));
    let app = Rc::new(App {
        window,
        state: RefCell::new(State::default()),
        rows,
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    wire(&app);
    app.refresh_crumbs();

    let theme = shell::theme();
    app.window.set_theme_choice(theme);

    // Files arrive as separate events, one per path, so gather them until the
    // event loop is idle again.
    let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    app.window.window().on_winit_window_event(move |_, event| {
        match event {
            winit::event::WindowEvent::Resized(_) => {
                with_app(|app| {
                    app.window
                        .set_maximized(app.window.window().is_maximized());
                });
            }
            winit::event::WindowEvent::HoveredFile(_) => {
                with_app(|app| app.window.set_dropping(true));
            }
            winit::event::WindowEvent::HoveredFileCancelled => {
                with_app(|app| app.window.set_dropping(false));
            }
            winit::event::WindowEvent::DroppedFile(path) => {
                let first = pending.borrow().is_empty();
                pending.borrow_mut().push(path.clone());
                if first {
                    let pending = pending.clone();
                    slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
                        let paths = std::mem::take(&mut *pending.borrow_mut());
                        with_app(|app| {
                            app.window.set_dropping(false);
                            app.dropped(paths);
                        });
                    });
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });

    app.window.window().on_close_requested(request_close);

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--extract-here") => {
            if let Some(path) = args.next() {
                app.state.borrow_mut().extract_on_open = true;
                app.open_path(path);
            }
        }
        Some("--add") => {
            let inputs: Vec<PathBuf> = args.map(PathBuf::from).collect();
            if !inputs.is_empty() {
                app.begin_create(inputs);
            }
        }
        Some(path) => app.open_path(path.to_owned()),
        None => {}
    }

    app.window.show()?;
    app.apply_theme(theme);
    // Keeps the Windows 11 shadow and rounded corners without the system title bar.
    let _ = app.window.window().with_winit_window(|window| {
        use winit::platform::windows::{CornerPreference, WindowExtWindows};
        window.set_undecorated_shadow(true);
        window.set_corner_preference(CornerPreference::Round);
    });
    slint::run_event_loop()?;
    app.window.hide()?;

    let temp_dir = app.state.borrow().temp_dir.clone();
    if let Some(dir) = temp_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(())
}
