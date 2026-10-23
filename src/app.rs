use crate::{
    browser::{Browser, Panel, Sort},
    clipboard,
    delete::DeleteBatch,
    display_name,
    files::{
        self, Directory, FileEvent, FileItem, FileRequest, FileTask, TransferMode, TransferQueue,
    },
    git::{GitTask, RepoState},
    git_actions::{GitAction, GitActionTask, GitEvent, GitJob, GitOptions, GitPreview, Repository},
    scan::{Identity, Kind, Options, Progress, ScanEvent, ScanTask, Snapshot},
    shell::ShellRequest,
    treemap::Direction,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::{
    collections::HashSet,
    io,
    path::PathBuf,
    process::ExitStatus,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const NOTICE_DURATION: Duration = Duration::from_secs(3);

pub enum Modal {
    Help,
    Delete(DeleteBatch),
    Errors,
    FileInput(FileInput),
    Result { title: String, text: String },
    GitPanel(GitPanel),
    GitPreview(Box<GitPreview>),
}

pub struct GitPanel {
    pub repository: Repository,
    pub options: GitOptions,
    pub error: Option<String>,
}

pub enum FileInputKind {
    Paste { queue: TransferQueue, base: PathBuf },
    Create { parent: Directory },
    Rename { item: FileItem, parent: Directory },
}

pub struct FileInput {
    pub kind: FileInputKind,
    pub text: String,
    pub cursor: usize,
    pub error: Option<String>,
}

impl FileInput {
    pub fn new(kind: FileInputKind, text: String) -> Self {
        let cursor = text.len();
        Self {
            kind,
            text,
            cursor,
            error: None,
        }
    }
    fn request(&self) -> io::Result<FileRequest> {
        Ok(match &self.kind {
            FileInputKind::Paste { queue, base } => {
                let path = PathBuf::from(&self.text);
                let path = if path.is_absolute() {
                    path
                } else {
                    base.join(path)
                };
                FileRequest::Paste {
                    queue: queue.clone(),
                    destination: Directory::capture(&path)?,
                }
            }
            FileInputKind::Create { parent } => {
                files::validate_name(&self.text)?;
                FileRequest::Create {
                    parent: parent.clone(),
                    name: self.text.clone(),
                }
            }
            FileInputKind::Rename { item, parent } => {
                files::validate_name(&self.text)?;
                FileRequest::Rename {
                    item: item.clone(),
                    parent: parent.clone(),
                    name: self.text.clone(),
                }
            }
        })
    }
}

#[derive(Default)]
pub struct HitAreas {
    pub tree: Rect,
    pub map: Rect,
    pub rows: Vec<(u16, usize, u16)>,
    pub page_size: usize,
}

struct Restore {
    selected: PathBuf,
    scope: PathBuf,
    expanded: HashSet<PathBuf>,
    hidden: HashSet<PathBuf>,
    marked: Vec<(PathBuf, Identity, Kind)>,
    sort: Sort,
    query: String,
    focus: Panel,
}

impl Restore {
    fn capture(b: &Browser) -> Self {
        Self {
            selected: b.data.path(b.selected),
            scope: b.data.path(b.scope),
            expanded: b.expanded.iter().map(|&id| b.data.path(id)).collect(),
            hidden: b.hidden_items().map(|id| b.data.path(id)).collect(),
            marked: b
                .marked
                .iter()
                .map(|&id| {
                    (
                        b.data.path(id),
                        b.data.nodes[id].identity.clone(),
                        b.data.nodes[id].kind,
                    )
                })
                .collect(),
            sort: b.sort,
            query: b.query.clone(),
            focus: b.focus,
        }
    }

    fn apply(self, mut b: Browser) -> Browser {
        // Reuse the normal sort path so the restore order matches the label.
        while std::mem::discriminant(&b.sort) != std::mem::discriminant(&self.sort) {
            b.cycle_sort();
        }
        b.expanded = self
            .expanded
            .iter()
            .filter_map(|path| b.data.find_path(path))
            .collect();
        b.expanded.insert(0);
        let hidden: Vec<_> = self
            .hidden
            .iter()
            .filter_map(|path| b.data.find_path(path))
            .collect();
        b.set_hidden(hidden);
        b.marked = self
            .marked
            .into_iter()
            .filter_map(|(path, identity, kind)| {
                let id = b.data.find_path(&path)?;
                let node = &b.data.nodes[id];
                (node.identity == identity && node.kind == kind).then_some(id)
            })
            .collect();
        let mut scope = self.scope;
        b.scope = loop {
            if let Some(id) = b.data.find_path(&scope) {
                break id;
            }
            if !scope.pop() {
                break 0;
            }
        };
        b.expanded.insert(b.scope);
        b.focus = self.focus;
        let mut selected = self.selected;
        let selected_id = loop {
            if let Some(id) = b.data.find_path(&selected) {
                break id;
            }
            if !selected.pop() {
                break b.scope;
            }
        };
        b.filter(self.query);
        if b.rows.iter().any(|r| r.id == selected_id) {
            b.selected = selected_id;
        } else if b.query.is_empty() {
            b.select(selected_id);
        }
        b.rebuild_rows();
        b
    }
}

pub struct App {
    pub root: PathBuf,
    pub options: Options,
    pub browser: Option<Browser>,
    pub scanning: Option<ScanTask>,
    pub progress: Progress,
    pub started: Instant,
    pub status: String,
    pub modal: Option<Modal>,
    pub modal_scroll: u16,
    pub searching: bool,
    pub quitting: bool,
    pub hits: HitAreas,
    pub deleting: Option<Receiver<Result<(), String>>>,
    pub copying: Option<Receiver<Result<PathBuf, String>>>,
    pub git: Option<GitTask>,
    pub shell: Option<ShellRequest>,
    pub transfer: Option<TransferQueue>,
    pub file_task: Option<FileTask>,
    pub git_action: Option<GitActionTask>,
    pub operation_status: String,
    select_after_scan: Option<PathBuf>,
    status_expires: Option<Instant>,
    exit_after_delete: bool,
    last_click: Option<(usize, Instant)>,
}

impl App {
    pub fn new(root: PathBuf, options: Options) -> Self {
        let task = ScanTask::start(root.clone(), options);
        Self {
            root,
            options,
            browser: None,
            scanning: Some(task),
            progress: Progress::default(),
            started: Instant::now(),
            status: "Scanning directory…".into(),
            modal: None,
            modal_scroll: 0,
            searching: false,
            quitting: false,
            hits: HitAreas::default(),
            deleting: None,
            copying: None,
            git: None,
            shell: None,
            transfer: None,
            file_task: None,
            git_action: None,
            operation_status: String::new(),
            select_after_scan: None,
            status_expires: None,
            exit_after_delete: false,
            last_click: None,
        }
    }

    pub fn rescan(&mut self) {
        if self.deleting.is_some() || self.file_task.is_some() || self.git_action.is_some() {
            return;
        }
        if !self.status.starts_with("Deletion") {
            self.set_status("Scanning directory…");
        }
        self.git = None;
        self.scanning = Some(ScanTask::start(self.root.clone(), self.options));
        self.progress = Progress::default();
        self.started = Instant::now();
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
        self.status_expires = None;
    }

    fn notify(&mut self, status: impl Into<String>) {
        self.set_status(status);
        self.status_expires = Some(Instant::now() + NOTICE_DURATION);
    }

    fn expire_status(&mut self, now: Instant) -> bool {
        if self.status_expires.is_none_or(|deadline| now < deadline) {
            return false;
        }
        self.set_status(if self.deleting.is_some() {
            "Deleting…"
        } else if self.scanning.is_some() {
            "Scanning directory…"
        } else {
            "Ready. Select an item to explore its space usage."
        });
        true
    }

    pub fn shell_finished(&mut self, result: io::Result<ExitStatus>) {
        self.last_click = None;
        self.notify(match result {
            Ok(status) if status.success() => {
                "Terminal: returned to Spacewalk. Press r to refresh.".into()
            }
            Ok(status) => format!("Terminal: shell exited with {status}. Press r to refresh."),
            Err(error) => format!("Terminal unavailable: {error}"),
        });
    }

    /// Process worker updates and report whether the screen needs redrawing.
    pub fn poll_workers(&mut self) -> bool {
        let mut changed = false;
        loop {
            let event = match self
                .git_action
                .as_ref()
                .map(|task| task.receiver.try_recv())
            {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => GitEvent::Finished(Err(
                    "Git worker stopped unexpectedly; review the repository in the shell".into(),
                )),
                _ => break,
            };
            changed = true;
            match event {
                GitEvent::Progress(text) => self.operation_status = text,
                event => {
                    self.git_action = None;
                    self.operation_status.clear();
                    self.modal_scroll = 0;
                    match event {
                        GitEvent::Inspected(Ok(repository)) => {
                            self.modal = Some(Modal::GitPanel(GitPanel {
                                repository,
                                options: GitOptions::default(),
                                error: None,
                            }))
                        }
                        GitEvent::Previewed(Ok(preview)) => {
                            self.modal = Some(Modal::GitPreview(preview))
                        }
                        GitEvent::Finished(result) => {
                            self.modal = Some(Modal::Result {
                                title: " GIT RESULT ".into(),
                                text: result.unwrap_or_else(|error| error),
                            });
                            self.rescan();
                        }
                        GitEvent::Inspected(Err(error)) | GitEvent::Previewed(Err(error)) => {
                            self.modal = Some(Modal::Result {
                                title: " GIT ACTION UNAVAILABLE ".into(),
                                text: error,
                            });
                            self.rescan();
                        }
                        GitEvent::Progress(_) => unreachable!(),
                    }
                    if self.exit_after_delete {
                        self.quitting = true;
                    }
                    break;
                }
            }
        }
        loop {
            let event = match self.file_task.as_ref().map(|task| task.receiver.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => FileEvent::Finished(files::FileOutcome {
                    error: Some("File worker stopped unexpectedly".into()),
                    ..Default::default()
                }),
                _ => break,
            };
            changed = true;
            match event {
                FileEvent::Progress(text) => self.operation_status = text,
                FileEvent::Finished(result) => {
                    self.file_task = None;
                    if let Some(queue) = self.transfer.as_mut()
                        && queue.mode == TransferMode::Move
                    {
                        queue
                            .items
                            .retain(|item| !result.completed.contains(&item.path));
                        if queue.items.is_empty() {
                            self.transfer = None;
                        }
                    }
                    self.select_after_scan = result.selected;
                    let text = if let Some(error) = result.error {
                        format!(
                            "{} items completed.\n\n{error}\n\nCompleted items are kept; unfinished cut items remain queued.",
                            result.completed.len()
                        )
                    } else {
                        "File operation complete. Disk usage refreshed.".into()
                    };
                    self.operation_status.clear();
                    self.modal = Some(Modal::Result {
                        title: " FILE OPERATION ".into(),
                        text,
                    });
                    self.modal_scroll = 0;
                    if self.exit_after_delete {
                        self.quitting = true;
                    } else {
                        self.rescan();
                    }
                    break;
                }
            }
        }
        let copied = match self.copying.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("Clipboard worker stopped unexpectedly".into()))
            }
            _ => None,
        };
        if let Some(result) = copied {
            self.copying = None;
            self.notify(match result {
                Ok(path) => format!("Clipboard: copied {}", display_name(path.as_os_str())),
                Err(error) => format!("Clipboard unavailable: {error}"),
            });
            changed = true;
        }
        loop {
            let event = match self.scanning.as_ref().map(|task| task.receiver.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => {
                    changed = true;
                    self.scanning = None;
                    self.set_status("Scanner stopped unexpectedly. Press r to retry.");
                    break;
                }
                _ => break,
            };
            changed = true;
            match event {
                ScanEvent::Progress(progress) => self.progress = progress,
                ScanEvent::Finished(result) => {
                    self.scanning = None;
                    match result {
                        Ok(data) => self.accept_snapshot(data),
                        Err(error) => {
                            self.set_status(format!("Scan failed: {error}. Press r to retry."))
                        }
                    }
                    break;
                }
            }
        }
        let result = match self.deleting.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("Deletion worker stopped unexpectedly".into()))
            }
            _ => None,
        };
        if let Some(result) = result {
            changed = true;
            self.deleting = None;
            self.notify(match result {
                Ok(()) => "Deletion complete. Disk usage refreshed.".into(),
                Err(error) => format!("Deletion failed: {error}. Refreshing usage."),
            });
            if self.exit_after_delete {
                self.quitting = true;
            } else {
                self.rescan();
            }
        }
        loop {
            match self.git.as_ref().map(|task| task.receiver.try_recv()) {
                Some(Ok((id, state))) => {
                    if let Some(b) = self.browser.as_mut()
                        && let Some(entry) = b.repositories.get_mut(&id)
                    {
                        *entry = state;
                        changed = true;
                    }
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    self.git = None;
                    if let Some(b) = self.browser.as_mut() {
                        for state in b.repositories.values_mut() {
                            if matches!(state, RepoState::Checking) {
                                *state = RepoState::Unavailable("Git worker stopped".into());
                                changed = true;
                            }
                        }
                    }
                    break;
                }
                _ => break,
            }
        }
        changed | self.expire_status(Instant::now())
    }

    fn accept_snapshot(&mut self, data: Snapshot) {
        let mut b = Browser::new(data);
        if let Some(restore) = self.browser.as_ref().map(Restore::capture) {
            b = restore.apply(b);
        }
        if let Some(path) = self.select_after_scan.take()
            && let Some(id) = b.data.find_path(&path)
        {
            b.select(id);
        }
        if self.status == "Scanning directory…" {
            self.set_status("Ready. Select an item to explore its space usage.");
        }
        let mut repositories: Vec<_> = b
            .repositories
            .keys()
            .map(|&id| (id, b.data.path(id)))
            .collect();
        repositories.sort_by_key(|(id, _)| *id);
        self.git = if repositories.is_empty() {
            None
        } else {
            Some(GitTask::start(repositories))
        };
        self.browser = Some(b);
        self.last_click = None;
    }

    fn quit(&mut self) {
        if let Some(task) = &self.git_action {
            task.cancel();
            self.exit_after_delete = true;
            self.set_status("Waiting for Git to stop before exit…");
        } else if let Some(task) = &self.file_task {
            task.cancel();
            self.exit_after_delete = true;
            self.set_status("Finishing file operation before exit…");
        } else if self.deleting.is_some() {
            self.exit_after_delete = true;
            self.set_status("Finishing deletion before exit…");
        } else {
            self.quitting = true;
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit();
            return;
        }
        if matches!(self.modal, Some(Modal::FileInput(_))) {
            self.file_input_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::GitPanel(_))) {
            self.git_panel_key(key);
            return;
        }
        if matches!(self.modal, Some(Modal::GitPreview(_))) {
            if key.kind != KeyEventKind::Press {
                return;
            }
            match key.code {
                KeyCode::Char('y') => {
                    if let Some(Modal::GitPreview(preview)) = self.modal.take() {
                        self.start_git_job(GitJob::Execute(preview));
                    }
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'q') => {
                    self.modal = None;
                    self.rescan();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal_scroll = self.modal_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal_scroll = self.modal_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.modal_scroll = self.modal_scroll.saturating_add(10),
                KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(10),
                _ => {}
            }
            return;
        }
        if self.modal.is_some() {
            if matches!(self.modal, Some(Modal::Delete(_))) {
                // Ignore repeat events for confirmation; holding d/y must not delete twice.
                if key.kind != KeyEventKind::Press {
                    return;
                }
                if key.code == KeyCode::Char('y') {
                    if let Some(Modal::Delete(request)) = self.modal.take() {
                        let (sender, receiver) = mpsc::channel();
                        self.set_status(format!(
                            "Deleting {}…",
                            if request.items.len() == 1 {
                                display_name(request.items[0].path.as_os_str())
                            } else {
                                format!("{} items", request.items.len())
                            }
                        ));
                        thread::spawn(move || {
                            let _ = sender.send(request.execute().map_err(|e| e.to_string()));
                        });
                        self.deleting = Some(receiver);
                    }
                } else if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'q')
                ) {
                    self.modal = None;
                } else {
                    match key.code {
                        KeyCode::Down => self.modal_scroll = self.modal_scroll.saturating_add(1),
                        KeyCode::Up => self.modal_scroll = self.modal_scroll.saturating_sub(1),
                        KeyCode::PageDown => {
                            self.modal_scroll = self.modal_scroll.saturating_add(10)
                        }
                        KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(10),
                        _ => {}
                    }
                }
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | '?') => self.modal = None,
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.modal_scroll = self.modal_scroll.saturating_add(1)
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.modal_scroll = self.modal_scroll.saturating_sub(1)
                    }
                    KeyCode::PageDown => self.modal_scroll = self.modal_scroll.saturating_add(10),
                    KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(10),
                    _ => {}
                }
            }
            return;
        }
        if self.searching {
            if let Some(b) = self.browser.as_mut() {
                let mut query = b.query.clone();
                match key.code {
                    KeyCode::Esc => {
                        self.searching = false;
                        b.filter(String::new());
                        return;
                    }
                    KeyCode::Enter => {
                        self.searching = false;
                        return;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.clear()
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        query.push(c)
                    }
                    _ => return,
                }
                b.filter(query);
            }
            return;
        }
        if key.kind == KeyEventKind::Press
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('g')
        {
            self.open_git_panel();
            return;
        }
        if key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('c' | 'x' | 'p' | 'a' | 'R'))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.file_action(key.code);
            return;
        }
        if key.code == KeyCode::Esc && self.transfer.is_some() {
            self.transfer = None;
            self.notify("Files: transfer queue cleared");
            return;
        }
        if key.code == KeyCode::F(4)
            && let Some(task) = &self.file_task
        {
            task.cancel();
            self.operation_status = "Cancelling at a safe boundary…".into();
            return;
        }
        if key.code == KeyCode::F(4)
            && let Some(task) = &self.git_action
        {
            task.cancel();
            self.operation_status = "Stopping Git; completed steps are kept…".into();
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.quit();
                return;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.modal = Some(Modal::Help);
                self.modal_scroll = 0;
                return;
            }
            KeyCode::Char('r') => {
                self.rescan();
                return;
            }
            _ => {}
        }
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                b.focus = if b.focus == Panel::Tree {
                    Panel::Map
                } else {
                    Panel::Tree
                }
            }
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Esc => b.filter(String::new()),
            KeyCode::Char('s') => b.cycle_sort(),
            KeyCode::Char('t') if key.kind == KeyEventKind::Press && !b.rows.is_empty() => {
                if self.scanning.is_some()
                    || self.deleting.is_some()
                    || self.file_task.is_some()
                    || self.git_action.is_some()
                {
                    self.notify("Wait for the current operation before opening a terminal.");
                } else if self.shell.is_none() {
                    self.shell = Some(ShellRequest::new(&b.data, b.selected));
                }
            }
            KeyCode::Char('y')
                if key.kind == KeyEventKind::Press
                    && !b.rows.is_empty()
                    && self.copying.is_none() =>
            {
                let path = b.data.path(b.selected);
                let (sender, receiver) = mpsc::channel();
                self.copying = Some(receiver);
                self.set_status("Clipboard: copying selected path…");
                thread::spawn(move || {
                    let result = clipboard::copy_path(&path).map(|()| path);
                    let _ = sender.send(result);
                });
            }
            KeyCode::Char('h') if !b.rows.is_empty() => {
                b.toggle_visibility(b.selected);
                self.last_click = None;
            }
            KeyCode::Char('H') => {
                b.show_all();
                self.last_click = None;
            }
            KeyCode::Char('m') if key.kind == KeyEventKind::Press && !b.rows.is_empty() => {
                if self.scanning.is_some()
                    || self.deleting.is_some()
                    || self.file_task.is_some()
                    || self.git_action.is_some()
                {
                    self.notify("Wait for the current operation before marking items.");
                } else if b.selected == 0 {
                    self.notify("The scan root cannot be marked.");
                } else {
                    b.toggle_mark(b.selected);
                }
            }
            KeyCode::Char('M') if key.kind == KeyEventKind::Press => b.marked.clear(),
            KeyCode::Char('n') => b.next_match(false),
            KeyCode::Char('N') => b.next_match(true),
            KeyCode::Char('e') => {
                self.modal = Some(Modal::Errors);
                self.modal_scroll = 0;
            }
            KeyCode::Enter => {
                if b.focus == Panel::Map
                    && b.data.nodes[b.selected].kind != crate::scan::Kind::Directory
                    && let Some(parent) = b.data.nodes[b.selected].parent
                {
                    b.select(parent);
                }
                b.zoom_in();
            }
            KeyCode::Backspace => b.zoom_out(),
            KeyCode::Home | KeyCode::Char('g') => {
                if let Some(row) = b.rows.first() {
                    b.selected = row.id;
                }
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let Some(row) = b.rows.last() {
                    b.selected = row.id;
                }
            }
            KeyCode::PageDown => b.move_tree(self.hits.page_size.max(1) as isize),
            KeyCode::PageUp => b.move_tree(-(self.hits.page_size.max(1) as isize)),
            KeyCode::Char('d') | KeyCode::Delete if key.kind == KeyEventKind::Press => {
                if self.scanning.is_some()
                    || self.deleting.is_some()
                    || self.file_task.is_some()
                    || self.git_action.is_some()
                {
                    self.notify("Wait for the current operation before deleting.");
                } else if !b.marked.is_empty() || !b.rows.is_empty() {
                    let ids = if b.marked.is_empty() {
                        vec![b.selected]
                    } else {
                        b.marked.iter().copied().collect()
                    };
                    match DeleteBatch::new(&b.data, ids) {
                        Ok(request) => {
                            self.modal = Some(Modal::Delete(request));
                            self.modal_scroll = 0;
                        }
                        Err(error) => self.notify(error.to_string()),
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if b.focus == Panel::Tree {
                    b.move_tree(-1);
                } else {
                    b.move_map(Direction::Up);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if b.focus == Panel::Tree {
                    b.move_tree(1);
                } else {
                    b.move_map(Direction::Down);
                }
            }
            KeyCode::Left => {
                if b.focus == Panel::Tree {
                    b.collapse();
                } else {
                    b.move_map(Direction::Left);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if b.focus == Panel::Tree {
                    b.expand();
                } else {
                    b.move_map(Direction::Right);
                }
            }
            KeyCode::Char(' ') if b.focus == Panel::Tree => b.toggle(b.selected),
            _ => {}
        }
    }

    fn start_git_job(&mut self, job: GitJob) {
        self.git = None;
        self.modal = None;
        self.operation_status = "Preparing Git action…".into();
        self.git_action = Some(GitActionTask::start(job));
    }

    fn open_git_panel(&mut self) {
        if self.scanning.is_some()
            || self.deleting.is_some()
            || self.file_task.is_some()
            || self.git_action.is_some()
        {
            self.notify("Wait for the current operation before opening Git actions.");
            return;
        }
        let Some(b) = &self.browser else {
            return;
        };
        if b.rows.is_empty() {
            return;
        }
        let path = b.data.path(b.selected);
        let directory = if b.data.nodes[b.selected].kind == Kind::Directory {
            path
        } else {
            path.parent().unwrap_or(&b.data.root).to_path_buf()
        };
        self.start_git_job(GitJob::Inspect(directory));
    }

    fn git_panel_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            self.modal = None;
            self.rescan();
            return;
        }
        let Some(Modal::GitPanel(panel)) = self.modal.as_mut() else {
            return;
        };
        if key.code == KeyCode::PageDown {
            self.modal_scroll = self.modal_scroll.saturating_add(5);
            return;
        }
        if key.code == KeyCode::PageUp {
            self.modal_scroll = self.modal_scroll.saturating_sub(5);
            return;
        }
        let action = match key.code {
            KeyCode::Char('f') => {
                panel.options.force = !panel.options.force;
                None
            }
            KeyCode::Char('l') => {
                panel.options.discard_commits = !panel.options.discard_commits;
                None
            }
            KeyCode::Char('b') => {
                panel.options.delete_branches = !panel.options.delete_branches;
                None
            }
            KeyCode::Char('s') => {
                panel.options.clear_stashes = !panel.options.clear_stashes;
                None
            }
            KeyCode::Char('p') => Some(GitAction::Pull),
            KeyCode::Char('P') => Some(GitAction::Push),
            KeyCode::Char('c') => Some(GitAction::Clean),
            KeyCode::Char('r') => Some(GitAction::Reset),
            _ => None,
        };
        if let Some(action) = action {
            let needs_remote = action != GitAction::Clean || panel.options.discard_commits;
            if panel.repository.head.is_none() {
                panel.error = Some("No commits yet; use the shell first.".into());
                return;
            }
            if needs_remote
                && (panel.repository.branch.is_none() || panel.repository.upstream.is_none())
            {
                panel.error = Some("This action needs an attached branch with a configured remote upstream. Use the shell to configure it.".into());
                return;
            }
            let options = panel.options;
            let path = panel.repository.root.clone();
            self.start_git_job(GitJob::Preview {
                path,
                action,
                options,
            });
        }
    }

    fn file_action(&mut self, code: KeyCode) {
        if self.scanning.is_some()
            || self.deleting.is_some()
            || self.file_task.is_some()
            || self.git_action.is_some()
        {
            self.notify("Wait for the current operation before changing files.");
            return;
        }
        let Some(b) = &self.browser else {
            return;
        };
        if b.rows.is_empty() {
            return;
        }
        let result = (|| -> io::Result<Option<FileInput>> {
            let path = b.data.path(b.selected);
            let directory = if b.data.nodes[b.selected].kind == Kind::Directory {
                path.clone()
            } else {
                path.parent().unwrap_or(&b.data.root).to_path_buf()
            };
            match code {
                KeyCode::Char('c' | 'x') => {
                    let ids = if b.marked.is_empty() {
                        vec![b.selected]
                    } else {
                        b.marked.iter().copied().collect()
                    };
                    let mode = if code == KeyCode::Char('c') {
                        TransferMode::Copy
                    } else {
                        TransferMode::Move
                    };
                    self.transfer = Some(TransferQueue::new(&b.data, ids, mode)?);
                    Ok(None)
                }
                KeyCode::Char('p') => {
                    let queue = self
                        .transfer
                        .clone()
                        .ok_or_else(|| io::Error::other("Use c to copy or x to cut items first"))?;
                    let text = directory
                        .to_str()
                        .ok_or_else(|| {
                            io::Error::other("Destination path is not valid Unicode; use the shell")
                        })?
                        .to_owned();
                    Ok(Some(FileInput::new(
                        FileInputKind::Paste {
                            queue,
                            base: directory,
                        },
                        text,
                    )))
                }
                KeyCode::Char('a') => Ok(Some(FileInput::new(
                    FileInputKind::Create {
                        parent: Directory::capture(&directory)?,
                    },
                    String::new(),
                ))),
                KeyCode::Char('R') => {
                    if b.selected == 0 {
                        return Err(io::Error::other("The scan root cannot be renamed"));
                    }
                    let text = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| {
                            io::Error::other("Name is not valid Unicode; use the shell")
                        })?
                        .to_owned();
                    Ok(Some(FileInput::new(
                        FileInputKind::Rename {
                            item: FileItem::from_snapshot(&b.data, b.selected)?,
                            parent: Directory::capture(path.parent().unwrap())?,
                        },
                        text,
                    )))
                }
                _ => Ok(None),
            }
        })();
        match result {
            Ok(Some(input)) => {
                self.modal = Some(Modal::FileInput(input));
                self.modal_scroll = 0;
            }
            Ok(None) => self.notify("Files: items queued · navigate then p paste · Esc clear"),
            Err(error) => self.notify(format!("Files: {error}")),
        }
    }

    fn file_input_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        let Some(Modal::FileInput(input)) = self.modal.as_mut() else {
            return;
        };
        if key.code == KeyCode::Enter && key.kind == KeyEventKind::Press {
            match input.request() {
                Ok(request) => {
                    self.file_task = Some(FileTask::start(request));
                    self.git = None;
                    self.operation_status = "Checking file operation…".into();
                    self.modal = None;
                }
                Err(error) => input.error = Some(error.to_string()),
            }
            return;
        }
        match key.code {
            KeyCode::PageDown => self.modal_scroll = self.modal_scroll.saturating_add(5),
            KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(5),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.text.clear();
                input.cursor = 0;
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.text.insert(input.cursor, ch);
                input.cursor += ch.len_utf8();
            }
            KeyCode::Left if input.cursor > 0 => {
                input.cursor = input.text[..input.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i);
            }
            KeyCode::Right if input.cursor < input.text.len() => {
                input.cursor += input.text[input.cursor..]
                    .chars()
                    .next()
                    .unwrap()
                    .len_utf8();
            }
            KeyCode::Home => input.cursor = 0,
            KeyCode::End => input.cursor = input.text.len(),
            KeyCode::Backspace if input.cursor > 0 => {
                let previous = input.text[..input.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i);
                input.text.drain(previous..input.cursor);
                input.cursor = previous;
            }
            KeyCode::Delete if input.cursor < input.text.len() => {
                let end = input.cursor
                    + input.text[input.cursor..]
                        .chars()
                        .next()
                        .unwrap()
                        .len_utf8();
                input.text.drain(input.cursor..end);
            }
            KeyCode::Tab => {
                if let FileInputKind::Paste { base, .. } = &input.kind {
                    match files::complete_directory(&input.text, base) {
                        Ok(text) => {
                            input.text = text;
                            input.cursor = input.text.len();
                            input.error = None;
                        }
                        Err(e) => input.error = Some(e.to_string()),
                    }
                }
            }
            _ => {}
        }
    }

    pub fn mouse(&mut self, event: MouseEvent) {
        if self.modal.is_some() || self.searching {
            return;
        }
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        let pos = ratatui::layout::Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.hits.tree.contains(pos) => {
                b.focus = Panel::Tree;
                if let Some(&(_, id, twisty)) =
                    self.hits.rows.iter().find(|&&(y, _, _)| y == event.row)
                {
                    b.selected = id;
                    if event.column == twisty {
                        b.toggle(id);
                    }
                    self.last_click = None;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if self.hits.map.contains(pos) => {
                b.focus = Panel::Map;
                if let Some(id) = b.map.hit(event.column, event.row) {
                    b.select(id);
                    if self
                        .last_click
                        .as_ref()
                        .is_some_and(|(last, time)| *last == id && time.elapsed().as_millis() < 350)
                    {
                        if let Some(parent) = b.data.nodes[id].parent {
                            b.select(parent);
                            b.zoom_in();
                        }
                        self.last_click = None;
                    } else {
                        self.last_click = Some((id, Instant::now()));
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.hits.map.contains(pos) => b.zoom_out(),
            MouseEventKind::ScrollDown if self.hits.tree.contains(pos) => {
                b.focus = Panel::Tree;
                b.move_tree(3);
            }
            MouseEventKind::ScrollUp if self.hits.tree.contains(pos) => {
                b.focus = Panel::Tree;
                b.move_tree(-3);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git::{RemoteInfo, RepoStatus},
        scan::scan,
        ui,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::atomic::AtomicBool;

    fn fixture() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().into(), Options::default());
        app.scanning = None;
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut browser = Browser::new(data);
        browser.repositories.insert(
            0,
            RepoState::Ready(RepoStatus {
                branches: 1,
                has_remote: true,
                fetched: true,
                remote: Some(RemoteInfo {
                    name: "origin".into(),
                    url: "git@host:team/repo.git".into(),
                    upstream: true,
                }),
                ..RepoStatus::default()
            }),
        );
        app.browser = Some(browser);
        (dir, app)
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 42)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn completion_notice_expires_after_three_seconds_and_restores_git_details() {
        let (_dir, mut app) = fixture();
        app.notify("Deletion complete. Disk usage refreshed.");
        let deadline = app.status_expires.unwrap();
        let before = screen(&mut app);
        assert!(before.contains("Deletion complete."));
        assert!(!before.contains("Git: git@host:team/repo.git"));
        assert!(!app.expire_status(deadline - Duration::from_nanos(1)));
        assert!(app.expire_status(deadline));
        let after = screen(&mut app);
        assert!(!after.contains("Deletion complete."));
        assert!(after.contains("Git: git@host:team/repo.git"));
        assert!(!app.expire_status(deadline + NOTICE_DURATION));
    }

    #[test]
    fn idle_poll_requests_a_redraw_when_a_clipboard_notice_expires() {
        let (_dir, mut app) = fixture();
        app.notify("Clipboard: copied /selected/path");
        app.status_expires = Some(Instant::now() - Duration::from_millis(1));
        assert!(app.poll_workers());
        assert!(!app.poll_workers());
        assert!(!screen(&mut app).contains("Clipboard: copied"));
        assert!(screen(&mut app).contains("Git: git@host:team/repo.git"));
    }

    #[test]
    fn new_notices_restart_the_timeout_and_progress_cancels_an_old_timeout() {
        let (_dir, mut app) = fixture();
        app.notify("Deletion complete.");
        let old_deadline = Instant::now() - Duration::from_millis(1);
        app.status_expires = Some(old_deadline);
        app.notify("Clipboard: copied a new path");
        assert!(!app.expire_status(old_deadline));
        assert_eq!(app.status, "Clipboard: copied a new path");
        let deadline = app.status_expires.unwrap();
        app.set_status("Clipboard: copying selected path…");
        assert!(!app.expire_status(deadline + NOTICE_DURATION));
        assert_eq!(app.status, "Clipboard: copying selected path…");
        app.set_status("Scan failed: permission denied. Press r to retry.");
        assert!(!app.expire_status(deadline + NOTICE_DURATION));
        assert!(app.status.starts_with("Scan failed:"));
    }

    #[test]
    fn expiring_a_notice_preserves_active_scan_and_deletion_feedback() {
        let (dir, mut app) = fixture();
        app.scanning = Some(ScanTask::start(dir.path().into(), Options::default()));
        app.notify("Deletion complete. Disk usage refreshed.");
        assert!(app.expire_status(app.status_expires.unwrap()));
        assert_eq!(app.status, "Scanning directory…");
        app.scanning = None;
        let (_sender, receiver) = mpsc::channel();
        app.deleting = Some(receiver);
        app.notify("Wait for the current operation before deleting.");
        assert!(app.expire_status(app.status_expires.unwrap()));
        assert_eq!(app.status, "Deleting…");
    }
}
