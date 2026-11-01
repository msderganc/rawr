#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use native_windows_gui as nwg;
use rars::ReadCancellation;
use rawr::engine::{self, AppError, ExtractReport, ExtractRequest, OpenedArchive, Progress};
use rawr::listing::{self, Row};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const PAD: i32 = 10;
const ROW: i32 = 28;
const MESSAGES_HEIGHT: i32 = 72;

#[derive(Clone)]
enum Job {
    Open,
    Extract {
        out_dir: String,
        selected: Option<HashSet<String>>,
        overwrite: bool,
    },
    Test,
}

enum Outcome {
    Opened(OpenedArchive),
    Extracted(ExtractReport, String),
    Tested(usize),
}

/// What the worker thread hands back to the window thread.
#[derive(Default)]
struct Shared {
    progress: Option<Progress>,
    done: Option<Result<Outcome, AppError>>,
}

#[derive(Default)]
struct State {
    /// The archive a running or finished job is about.
    path: String,
    password: Option<String>,
    archive: Option<OpenedArchive>,
    folder: String,
    rows: Vec<Row>,
    messages: Vec<String>,
    job: Option<Job>,
    cancel: Option<ReadCancellation>,
}

#[derive(Default)]
struct App {
    window: nwg::Window,
    embed: nwg::EmbedResource,
    icon: nwg::Icon,
    open: nwg::Button,
    up: nwg::Button,
    summary: nwg::Label,
    test: nwg::Button,
    list: nwg::ListView,
    messages: nwg::TextBox,
    dest_label: nwg::Label,
    dest: nwg::TextInput,
    browse: nwg::Button,
    overwrite: nwg::ComboBox<&'static str>,
    extract: nwg::Button,
    bar: nwg::ProgressBar,
    cancel: nwg::Button,
    current: nwg::Label,
    open_dialog: nwg::FileDialog,
    folder_dialog: nwg::FileDialog,
    notice: nwg::Notice,
    state: RefCell<State>,
    shared: Arc<Mutex<Shared>>,
    busy: Cell<bool>,
    closing: Cell<bool>,
}

impl App {
    fn build() -> Result<Rc<App>, nwg::NwgError> {
        let mut app = App::default();
        nwg::EmbedResource::builder().build(&mut app.embed)?;
        app.icon = app.embed.icon(1, None).unwrap_or_default();
        nwg::Window::builder()
            .title("rawr")
            .size((900, 620))
            .center(true)
            .accept_files(true)
            .icon(Some(&app.icon))
            .flags(nwg::WindowFlags::MAIN_WINDOW | nwg::WindowFlags::VISIBLE)
            .build(&mut app.window)?;
        let parent = &app.window;
        nwg::Button::builder().text("Open...").parent(parent).build(&mut app.open)?;
        nwg::Button::builder().text("Up").parent(parent).enabled(false).build(&mut app.up)?;
        nwg::Label::builder()
            .text("Open a RAR archive, or drop one on this window.")
            .parent(parent)
            .build(&mut app.summary)?;
        nwg::Button::builder().text("Test").parent(parent).enabled(false).build(&mut app.test)?;
        nwg::ListView::builder()
            .parent(parent)
            .list_style(nwg::ListViewStyle::Detailed)
            .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
            .flags(
                nwg::ListViewFlags::VISIBLE
                    | nwg::ListViewFlags::TAB_STOP
                    | nwg::ListViewFlags::ALWAYS_SHOW_SELECTION,
            )
            .build(&mut app.list)?;
        for (name, width, right) in [
            ("Name", 400, false),
            ("Size", 80, true),
            ("Packed", 80, true),
            ("Modified", 130, false),
            ("", 80, false),
        ] {
            app.list.insert_column(nwg::InsertListViewColumn {
                index: None,
                fmt: right.then_some(nwg::ListViewColumnFlags::RIGHT),
                width: Some(width),
                text: Some(name.to_owned()),
            });
        }
        app.list.set_headers_enabled(true);
        nwg::TextBox::builder()
            .parent(parent)
            .readonly(true)
            .flags(nwg::TextBoxFlags::TAB_STOP | nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL)
            .build(&mut app.messages)?;
        nwg::Label::builder().text("Extract to:").parent(parent).build(&mut app.dest_label)?;
        nwg::TextInput::builder().parent(parent).build(&mut app.dest)?;
        nwg::Button::builder().text("Browse...").parent(parent).build(&mut app.browse)?;
        nwg::ComboBox::builder()
            .parent(parent)
            .collection(vec!["Skip existing files", "Replace existing files"])
            .selected_index(Some(0))
            .build(&mut app.overwrite)?;
        nwg::Button::builder().text("Extract").parent(parent).build(&mut app.extract)?;
        nwg::ProgressBar::builder().parent(parent).range(0..1000).build(&mut app.bar)?;
        nwg::Button::builder().text("Cancel").parent(parent).build(&mut app.cancel)?;
        nwg::Label::builder().text("").parent(parent).build(&mut app.current)?;
        nwg::FileDialog::builder()
            .title("Open RAR archive")
            .action(nwg::FileDialogAction::Open)
            .filters("RAR archives(*.rar;*.r00;*.exe)|All files(*.*)")
            .build(&mut app.open_dialog)?;
        nwg::FileDialog::builder()
            .title("Extract to")
            .action(nwg::FileDialogAction::OpenDirectory)
            .build(&mut app.folder_dialog)?;
        nwg::Notice::builder().parent(parent).build(&mut app.notice)?;
        Ok(Rc::new(app))
    }

    fn layout(&self) {
        let (width, height) = self.window.size();
        let (width, height) = (width as i32, height as i32);
        let place = |set_pos: &dyn Fn(i32, i32), set_size: &dyn Fn(u32, u32), x, y, w: i32, h: i32| {
            set_pos(x, y);
            set_size(w.max(1) as u32, h.max(1) as u32);
        };
        macro_rules! at {
            ($control:expr, $x:expr, $y:expr, $w:expr, $h:expr) => {
                place(&|x, y| $control.set_position(x, y), &|w, h| $control.set_size(w, h), $x, $y, $w, $h)
            };
        }
        at!(self.open, PAD, PAD, 84, ROW);
        at!(self.up, PAD + 90, PAD, 50, ROW);
        at!(self.summary, PAD + 150, PAD + 5, width - 2 * PAD - 150 - 80, ROW - 5);
        at!(self.test, width - PAD - 70, PAD, 70, ROW);

        let busy = self.busy.get();
        let bottom = if busy { 2 * ROW + 6 } else { ROW };
        let has_messages = !self.state.borrow().messages.is_empty();
        let footer = PAD + bottom + PAD + if has_messages { MESSAGES_HEIGHT + PAD } else { 0 };
        let list_top = PAD + ROW + PAD;
        let list_height = height - list_top - footer;
        at!(self.list, PAD, list_top, width - 2 * PAD, list_height);
        // Column widths are real pixels, unlike everything else here.
        let scale = nwg::scale_factor();
        let others = 80 + 80 + 130 + 80 + 30;
        let name_width = (width - 2 * PAD - others).max(120);
        self.list.set_column_width(0, (f64::from(name_width) * scale) as isize);
        for (column, logical) in [(1, 80.0), (2, 80.0), (3, 130.0), (4, 80.0)] {
            self.list.set_column_width(column, (logical * scale) as isize);
        }

        self.messages.set_visible(has_messages);
        at!(self.messages, PAD, list_top + list_height + PAD, width - 2 * PAD, MESSAGES_HEIGHT);

        let y = height - PAD - bottom;
        for control in [&self.bar as &dyn Visible, &self.cancel, &self.current] {
            control.show(busy);
        }
        for control in [&self.dest_label as &dyn Visible, &self.dest, &self.browse, &self.overwrite, &self.extract] {
            control.show(!busy);
        }
        at!(self.bar, PAD, y + 4, width - 2 * PAD - 90, ROW - 8);
        at!(self.cancel, width - PAD - 80, y, 80, ROW);
        at!(self.current, PAD, y + ROW + 8, width - 2 * PAD, ROW - 8);

        let right = width - PAD;
        at!(self.extract, right - 120, y, 120, ROW);
        at!(self.overwrite, right - 120 - 6 - 170, y, 170, ROW);
        at!(self.browse, right - 120 - 6 - 170 - 6 - 80, y, 80, ROW);
        at!(self.dest_label, PAD, y + 5, 70, ROW - 5);
        at!(self.dest, PAD + 74, y, right - 120 - 6 - 170 - 6 - 80 - 6 - PAD - 74, ROW);
    }

    fn update_controls(&self) {
        let state = self.state.borrow();
        let opened = state.archive.is_some();
        let busy = self.busy.get();
        self.open.set_enabled(!busy);
        self.test.set_enabled(opened && !busy);
        self.up.set_enabled(opened && !busy && !state.folder.is_empty());
        self.dest.set_enabled(opened && !busy);
        self.browse.set_enabled(opened && !busy);
        self.overwrite.set_enabled(opened && !busy);
        self.extract.set_enabled(opened && !busy && !self.dest.text().trim().is_empty());
        let selecting = self.chosen_rows(&state).next().is_some();
        self.extract.set_text(if selecting { "Extract selected" } else { "Extract" });
    }

    fn chosen_rows<'a>(&self, state: &'a State) -> impl Iterator<Item = &'a Row> {
        self.list
            .selected_items()
            .into_iter()
            .filter_map(|index| state.rows.get(index))
            .filter(|row| !row.is_up)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn show_folder(&self, folder: String) {
        let rows = {
            let state = self.state.borrow();
            let Some(archive) = &state.archive else { return };
            listing::rows_in(&archive.entries, &folder)
        };
        self.list.set_redraw(false);
        self.list.clear();
        for row in &rows {
            let (size, packed) = if row.is_up {
                (String::new(), String::new())
            } else {
                (listing::format_size(row.size), listing::format_size(row.packed))
            };
            let display = if row.is_folder && !row.is_up {
                format!("{}\\", row.name)
            } else {
                row.name.clone()
            };
            self.list.insert_items_row(
                None,
                &[display, size, packed, listing::format_date(row.modified_ms), row.note.to_owned()],
            );
        }
        self.list.set_redraw(true);
        self.list.invalidate();
        let mut state = self.state.borrow_mut();
        state.folder = folder;
        state.rows = rows;
        drop(state);
        self.update_controls();
    }

    fn enter_selected(&self) {
        let target = {
            let state = self.state.borrow();
            self.list
                .selected_item()
                .and_then(|index| state.rows.get(index))
                .filter(|row| row.is_folder)
                .map(|row| row.path.clone())
        };
        if let Some(folder) = target {
            self.show_folder(folder);
        }
    }

    fn go_up(&self) {
        let parent = listing::parent(&self.state.borrow().folder).to_owned();
        self.show_folder(parent);
    }

    fn set_messages(&self, messages: Vec<String>) {
        self.messages.set_text(&messages.join("\r\n"));
        self.state.borrow_mut().messages = messages;
        self.layout();
    }

    fn open_path(self: &Rc<Self>, path: String) {
        if self.busy.get() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.path = path;
            state.password = None;
        }
        self.set_messages(Vec::new());
        self.start(Job::Open);
    }

    fn start(self: &Rc<Self>, job: Job) {
        let cancel = ReadCancellation::new();
        let (path, password) = {
            let mut state = self.state.borrow_mut();
            state.job = Some(job.clone());
            state.cancel = Some(cancel.clone());
            (state.path.clone(), state.password.clone())
        };
        self.busy.set(true);
        self.bar.set_pos(0);
        self.bar.set_marquee(false, 0);
        self.cancel.set_enabled(true);
        self.current.set_text("");
        self.update_controls();
        self.layout();

        let shared = self.shared.clone();
        let sender = self.notice.sender();
        std::thread::spawn(move || {
            let progress_shared = shared.clone();
            let emit: engine::ProgressFn = Arc::new(move |progress| {
                progress_shared.lock().unwrap().progress = Some(progress);
                sender.notice();
            });
            let archive = PathBuf::from(&path);
            let password = password.as_deref();
            let result = match job {
                Job::Open => engine::open(&archive, password, &cancel).map(Outcome::Opened),
                Job::Test => engine::test(&archive, password, &cancel, emit).map(Outcome::Tested),
                Job::Extract { out_dir, selected, overwrite } => engine::extract(
                    ExtractRequest {
                        archive: &archive,
                        out_dir: &PathBuf::from(&out_dir),
                        password,
                        selected,
                        overwrite,
                    },
                    &cancel,
                    emit,
                )
                .map(|report| Outcome::Extracted(report, out_dir)),
            };
            shared.lock().unwrap().done = Some(result);
            sender.notice();
        });
    }

    fn on_notice(self: &Rc<Self>) {
        let (progress, done) = {
            let mut shared = self.shared.lock().unwrap();
            (shared.progress.take(), shared.done.take())
        };
        if let Some(progress) = progress {
            if progress.total_bytes > 0 {
                let fraction = progress.done_bytes as f64 / progress.total_bytes as f64;
                self.bar.set_pos((fraction.min(1.0) * 1000.0) as u32);
            }
            if self.cancel.enabled() {
                self.current.set_text(&progress.current);
            }
        }
        if let Some(result) = done {
            self.finish(result);
        }
    }

    fn finish(self: &Rc<Self>, result: Result<Outcome, AppError>) {
        self.busy.set(false);
        if self.closing.get() {
            nwg::stop_thread_dispatch();
            return;
        }
        let job = self.state.borrow().job.clone();
        let mut messages = Vec::new();
        match result {
            Ok(Outcome::Opened(archive)) => {
                self.window.set_text(&format!("{} - rawr", file_name(&archive.path)));
                self.summary.set_text(&summary_of(&archive));
                self.dest.set_text(&listing::default_destination(&archive.path));
                messages.extend(archive.warnings.iter().cloned());
                messages.extend(archive.comment.clone());
                self.state.borrow_mut().archive = Some(archive);
                self.show_folder(String::new());
            }
            Ok(Outcome::Extracted(report, out_dir)) => {
                let mut line = format!("Extracted {} file(s) to {out_dir}", report.written);
                if report.skipped_existing > 0 {
                    line += &format!(", skipped {} that already existed", report.skipped_existing);
                }
                messages.push(line + ".");
                messages.extend(report.warnings);
            }
            Ok(Outcome::Tested(count)) => {
                messages.push(format!("Integrity check passed: {count} item(s) OK."));
            }
            Err(error) if matches!(error.kind, "password_required" | "bad_password") => {
                let prompt = if error.kind == "bad_password" {
                    "That password didn't work, or the data is damaged. Try again:"
                } else {
                    "This archive is password protected. Enter the password:"
                };
                self.update_controls();
                self.layout();
                if let (Some(answer), Some(job)) = (ask_password(&self.window, prompt), job) {
                    self.state.borrow_mut().password = Some(answer);
                    self.start(job);
                    return;
                }
            }
            Err(error) if error.kind == "cancelled" => {
                messages.push("Cancelled. Files that finished before cancelling were kept.".into());
            }
            Err(error) => {
                messages.push(match job {
                    Some(Job::Open) => format!("Couldn't open this archive: {}", error.message),
                    Some(Job::Test) => format!("Test failed: {}", error.message),
                    _ => format!(
                        "Extraction stopped: {}. Files that passed their checks were kept.",
                        error.message
                    ),
                });
            }
        }
        self.update_controls();
        self.set_messages(messages);
    }

    /// Asks up front when file contents are encrypted but names aren't.
    fn password_ready(&self) -> bool {
        let needed = {
            let state = self.state.borrow();
            state.password.is_none() && state.archive.as_ref().is_some_and(|a| a.any_encrypted)
        };
        if !needed {
            return true;
        }
        match ask_password(&self.window, "This archive is password protected. Enter the password:") {
            Some(answer) => {
                self.state.borrow_mut().password = Some(answer);
                true
            }
            None => false,
        }
    }

    fn run_extract(self: &Rc<Self>) {
        if !self.password_ready() {
            return;
        }
        let selected = {
            let state = self.state.borrow();
            let Some(archive) = &state.archive else { return };
            let chosen: Vec<&Row> = self.chosen_rows(&state).collect();
            (!chosen.is_empty())
                .then(|| listing::expand_selection(&archive.entries, chosen.into_iter()))
        };
        self.set_messages(Vec::new());
        self.start(Job::Extract {
            out_dir: self.dest.text().trim().to_owned(),
            selected,
            overwrite: self.overwrite.selection() == Some(1),
        });
    }

    fn run_test(self: &Rc<Self>) {
        if self.password_ready() {
            self.set_messages(Vec::new());
            self.start(Job::Test);
        }
    }

    fn request_cancel(&self) {
        self.cancel.set_enabled(false);
        self.current.set_text("Cancelling...");
        if let Some(token) = &self.state.borrow().cancel {
            token.cancel();
        }
    }

    fn browse_open(self: &Rc<Self>) {
        if self.open_dialog.run(Some(&self.window)) {
            if let Ok(path) = self.open_dialog.get_selected_item() {
                self.open_path(path.to_string_lossy().into_owned());
            }
        }
    }

    fn browse_folder(&self) {
        if self.folder_dialog.run(Some(&self.window)) {
            if let Ok(path) = self.folder_dialog.get_selected_item() {
                self.dest.set_text(&path.to_string_lossy());
                self.update_controls();
            }
        }
    }
}

/// The controls that swap between the extract form and the progress display.
trait Visible {
    fn show(&self, visible: bool);
}

macro_rules! visible {
    ($($control:ty),*) => {
        $(impl Visible for $control {
            fn show(&self, visible: bool) {
                self.set_visible(visible);
            }
        })*
    };
}
visible!(nwg::ProgressBar, nwg::Button, nwg::Label, nwg::TextInput, nwg::ComboBox<&'static str>);

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn summary_of(archive: &OpenedArchive) -> String {
    let files = archive.entries.iter().filter(|e| !e.is_directory);
    let (count, total) = files.fold((0, 0), |(n, sum), e| (n + 1, sum + e.unpacked_size));
    let mut parts = vec![
        file_name(&archive.path).to_owned(),
        archive.format.to_owned(),
        format!("{count} file(s)"),
        listing::format_size(total),
    ];
    if archive.parts > 1 {
        parts.push(format!("{} parts", archive.parts));
    }
    if archive.is_solid {
        parts.push("solid".to_owned());
    }
    if archive.any_encrypted {
        parts.push("encrypted".to_owned());
    }
    // A label treats '&' as a keyboard shortcut marker.
    parts.join("  |  ").replace('&', "&&")
}

/// A modal password window, running its own message loop until answered.
fn ask_password(parent: &nwg::Window, prompt: &str) -> Option<String> {
    let mut window = nwg::Window::default();
    let mut label = nwg::Label::default();
    let mut input = nwg::TextInput::default();
    let mut ok = nwg::Button::default();
    let mut cancel = nwg::Button::default();
    nwg::Window::builder()
        .title("Password")
        .size((420, 130))
        .center(true)
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
        .parent(Some(parent))
        .build(&mut window)
        .ok()?;
    nwg::Label::builder().text(prompt).parent(&window).position((12, 12)).size((396, 24)).build(&mut label).ok()?;
    nwg::TextInput::builder().parent(&window).password(Some('*')).focus(true).position((12, 42)).size((396, 26)).build(&mut input).ok()?;
    nwg::Button::builder().text("OK").parent(&window).position((238, 86)).size((80, 28)).build(&mut ok).ok()?;
    nwg::Button::builder().text("Cancel").parent(&window).position((328, 86)).size((80, 28)).build(&mut cancel).ok()?;

    let answer = Rc::new(RefCell::new(None));
    let (input, ok, cancel) = (Rc::new(input), Rc::new(ok), Rc::new(cancel));
    let handler = {
        let (answer, input, ok, cancel) = (answer.clone(), input.clone(), ok.clone(), cancel.clone());
        let window_handle = window.handle;
        nwg::full_bind_event_handler(&window.handle, move |event, _, handle| {
            let accept = (event == nwg::Event::OnButtonClick && handle == ok.handle)
                || (event == nwg::Event::OnKeyEnter && handle == input.handle);
            let dismiss = (event == nwg::Event::OnButtonClick && handle == cancel.handle)
                || (event == nwg::Event::OnWindowClose && handle == window_handle);
            if accept {
                *answer.borrow_mut() = Some(input.text());
            }
            if accept || dismiss {
                nwg::stop_thread_dispatch();
            }
        })
    };
    parent.set_enabled(false);
    nwg::dispatch_thread_events();
    parent.set_enabled(true);
    nwg::unbind_event_handler(&handler);
    window.set_visible(false);
    parent.set_focus();
    let result = answer.borrow_mut().take();
    result
}

fn main() {
    nwg::init().expect("failed to start Windows controls");
    let _ = nwg::Font::set_global_family("Segoe UI");
    let app = App::build().expect("failed to build the window");
    app.layout();
    app.update_controls();

    let handler_app = app.clone();
    let handler = nwg::full_bind_event_handler(&app.window.handle, move |event, data, handle| {
        let app = &handler_app;
        use nwg::Event as E;
        match event {
            E::OnWindowClose if handle == app.window.handle => {
                if app.busy.get() {
                    // Let the job clean up its temporary files before exiting.
                    app.closing.set(true);
                    app.request_cancel();
                    if let nwg::EventData::OnWindowClose(close) = &data {
                        close.close(false);
                    }
                } else {
                    nwg::stop_thread_dispatch();
                }
            }
            E::OnResize | E::OnWindowMaximize if handle == app.window.handle => app.layout(),
            E::OnNotice if handle == app.notice.handle => app.on_notice(),
            E::OnFileDrop if handle == app.window.handle => {
                if let Some(path) = data.on_file_drop().files().into_iter().next() {
                    app.open_path(path);
                }
            }
            E::OnButtonClick if handle == app.open.handle => app.browse_open(),
            E::OnButtonClick if handle == app.up.handle => app.go_up(),
            E::OnButtonClick if handle == app.test.handle => app.run_test(),
            E::OnButtonClick if handle == app.browse.handle => app.browse_folder(),
            E::OnButtonClick if handle == app.extract.handle => app.run_extract(),
            E::OnButtonClick if handle == app.cancel.handle => app.request_cancel(),
            E::OnListViewDoubleClick if handle == app.list.handle => app.enter_selected(),
            E::OnListViewItemChanged if handle == app.list.handle => app.update_controls(),
            E::OnTextInput if handle == app.dest.handle => app.update_controls(),
            _ => {}
        }
    });

    if let Some(path) = std::env::args().nth(1) {
        app.open_path(path);
    }
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
}
