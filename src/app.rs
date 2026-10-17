use crate::{
    browser::{Browser, Panel, Sort},
    clipboard,
    delete::DeleteRequest,
    display_name,
    git::{GitTask, RepoState},
    scan::{Options, Progress, ScanEvent, ScanTask, Snapshot},
    treemap::Direction,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Instant,
};

pub enum Modal {
    Help,
    Delete(DeleteRequest),
    Errors,
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
            exit_after_delete: false,
            last_click: None,
        }
    }

    pub fn rescan(&mut self) {
        if self.deleting.is_some() {
            return;
        }
        if !self.status.starts_with("Deletion") {
            self.status = "Scanning directory…".into();
        }
        self.git = None;
        self.scanning = Some(ScanTask::start(self.root.clone(), self.options));
        self.progress = Progress::default();
        self.started = Instant::now();
    }

    /// Process worker updates and report whether the screen needs redrawing.
    pub fn poll_workers(&mut self) -> bool {
        let mut changed = false;
        let copied = match self.copying.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("Clipboard worker stopped unexpectedly".into()))
            }
            _ => None,
        };
        if let Some(result) = copied {
            self.copying = None;
            self.status = match result {
                Ok(path) => format!("Clipboard: copied {}", display_name(path.as_os_str())),
                Err(error) => format!("Clipboard unavailable: {error}"),
            };
            changed = true;
        }
        loop {
            let event = match self.scanning.as_ref().map(|task| task.receiver.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => {
                    changed = true;
                    self.scanning = None;
                    self.status = "Scanner stopped unexpectedly. Press r to retry.".into();
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
                            self.status = format!("Scan failed: {error}. Press r to retry.")
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
            self.status = match result {
                Ok(()) => "Deletion complete. Disk usage refreshed.".into(),
                Err(error) => format!("Deletion failed: {error}. Refreshing usage."),
            };
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
        changed
    }

    fn accept_snapshot(&mut self, data: Snapshot) {
        let mut b = Browser::new(data);
        if let Some(restore) = self.browser.as_ref().map(Restore::capture) {
            b = restore.apply(b);
        }
        if self.status == "Scanning directory…" {
            self.status = "Ready. Select an item to explore its space usage.".into();
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
        if self.deleting.is_some() {
            self.exit_after_delete = true;
            self.status = "Finishing deletion before exit…".into();
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
        if self.modal.is_some() {
            if matches!(self.modal, Some(Modal::Delete(_))) {
                // Ignore repeat events for confirmation; holding d/y must not delete twice.
                if key.kind != KeyEventKind::Press {
                    return;
                }
                if key.code == KeyCode::Char('y') {
                    if let Some(Modal::Delete(request)) = self.modal.take() {
                        let (sender, receiver) = mpsc::channel();
                        self.status =
                            format!("Deleting {}…", display_name(request.path.as_os_str()));
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
            KeyCode::Char('c')
                if key.kind == KeyEventKind::Press
                    && !b.rows.is_empty()
                    && self.copying.is_none() =>
            {
                let path = b.data.path(b.selected);
                let (sender, receiver) = mpsc::channel();
                self.copying = Some(receiver);
                self.status = "Clipboard: copying selected path…".into();
                thread::spawn(move || {
                    let result = clipboard::copy_path(&path).map(|()| path);
                    let _ = sender.send(result);
                });
            }
            KeyCode::Char('v') if !b.rows.is_empty() => {
                b.toggle_visibility(b.selected);
                self.last_click = None;
            }
            KeyCode::Char('V') => {
                b.show_all();
                self.last_click = None;
            }
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
                if self.scanning.is_some() || self.deleting.is_some() {
                    self.status = "Wait for the current operation before deleting.".into();
                } else if !b.rows.is_empty() {
                    match DeleteRequest::new(&b.data, b.selected) {
                        Ok(request) => {
                            self.modal = Some(Modal::Delete(request));
                            self.modal_scroll = 0;
                        }
                        Err(error) => self.status = error.to_string(),
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
            KeyCode::Left | KeyCode::Char('h') => {
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
