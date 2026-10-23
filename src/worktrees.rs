//! Repository-scoped worktree discovery and checkout accounting.
use crate::{
    git_actions::Runner,
    scan::{self, Options},
};
use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub main: bool,
    pub bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Registry {
    pub common_dir: PathBuf,
    pub records: Vec<WorktreeRecord>,
}

pub(crate) fn os_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

pub fn parse_list(output: &[u8]) -> Result<Vec<WorktreeRecord>, String> {
    let mut records = Vec::new();
    let mut record: Option<WorktreeRecord> = None;
    for field in output.split(|&byte| byte == 0) {
        if field.is_empty() {
            if let Some(value) = record.take() {
                records.push(value);
            }
            continue;
        }
        if let Some(path) = field.strip_prefix(b"worktree ") {
            if record.is_some() {
                return Err("Malformed worktree registration output".into());
            }
            if !Path::new(&os_path(path)).is_absolute() {
                return Err("Git returned a non-absolute worktree path".into());
            }
            record = Some(WorktreeRecord {
                path: os_path(path),
                head: None,
                branch: None,
                main: records.is_empty(),
                bare: false,
                locked: None,
                prunable: None,
            });
        } else if let Some(record) = record.as_mut() {
            if let Some(head) = field.strip_prefix(b"HEAD ") {
                record.head = Some(String::from_utf8_lossy(head).into_owned());
            } else if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                record.branch = Some(String::from_utf8_lossy(branch).into_owned());
            } else if field == b"bare" {
                record.bare = true;
            } else if field == b"locked" || field.starts_with(b"locked ") {
                record.locked = Some(
                    String::from_utf8_lossy(field.strip_prefix(b"locked ").unwrap_or(b"locked"))
                        .into_owned(),
                );
            } else if field == b"prunable" || field.starts_with(b"prunable ") {
                record.prunable = Some(
                    String::from_utf8_lossy(
                        field
                            .strip_prefix(b"prunable ")
                            .unwrap_or(b"stale registration"),
                    )
                    .into_owned(),
                );
            }
        }
    }
    if let Some(value) = record {
        records.push(value);
    }
    if records.is_empty() {
        return Err("Git returned no worktrees".into());
    }
    let mut unique = HashSet::new();
    if records
        .iter()
        .any(|record| !unique.insert(record.path.clone()))
    {
        return Err("Duplicate worktree registrations".into());
    }
    Ok(records)
}

pub fn discover(path: &Path, cancel: &AtomicBool) -> Result<Registry, String> {
    let runner = Runner::readonly(cancel);
    let mut raw = runner.run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if raw.last() == Some(&b'\n') {
        raw.pop();
    }
    let common_dir = os_path(&raw).canonicalize().map_err(|e| e.to_string())?;
    let records =
        parse_list(&runner.run(&common_dir, &["worktree", "list", "--porcelain", "-z"])?)?;
    Ok(Registry {
        common_dir,
        records,
    })
}

#[derive(Clone, Debug, Default)]
pub struct LocalState {
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub upstream: Option<String>,
    pub remote_upstream: bool,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}
impl LocalState {
    pub fn dirty(&self) -> bool {
        self.staged + self.modified + self.untracked > 0
    }

    pub fn synced(&self) -> bool {
        self.remote_upstream
            && self.upstream.is_some()
            && self.ahead == Some(0)
            && self.behind == Some(0)
            && !self.dirty()
    }
}

pub(crate) fn local_state(path: &Path, runner: &Runner<'_>) -> Result<LocalState, String> {
    let raw = runner.run(
        path,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let mut fields = raw.split(|&b| b == 0);
    let mut result = LocalState::default();
    while let Some(field) = fields.next() {
        if let Some(value) = field.strip_prefix(b"# branch.upstream ") {
            result.upstream = Some(String::from_utf8_lossy(value).into_owned());
        } else if let Some(value) = field.strip_prefix(b"# branch.ab ") {
            let values = String::from_utf8_lossy(value);
            let mut values = values.split_whitespace();
            result.ahead = values
                .next()
                .and_then(|value| value.strip_prefix('+'))
                .and_then(|value| value.parse().ok());
            result.behind = values
                .next()
                .and_then(|value| value.strip_prefix('-'))
                .and_then(|value| value.parse().ok());
        } else if matches!(field.first(), Some(b'1' | b'2' | b'u')) {
            result.staged += usize::from(field.get(2).is_some_and(|&b| b != b'.'));
            result.modified += usize::from(field.get(3).is_some_and(|&b| b != b'.'));
            if field.first() == Some(&b'2') {
                fields.next();
            }
        } else if field.starts_with(b"? ") {
            result.untracked += 1;
        }
    }
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct WorktreeRow {
    pub record: WorktreeRecord,
    pub bytes: Option<u64>,
    pub scan_errors: u64,
    pub state: Option<LocalState>,
    pub error: Option<String>,
    pub measured: bool,
    pub registered: bool,
}
impl WorktreeRow {
    /// Checkout status against its configured remote's last-fetched refs.
    /// Other local branches and repository-wide stashes belong to the Git menu.
    pub fn synced(&self) -> bool {
        self.registered
            && self.measured
            && self.error.is_none()
            && self.record.branch.is_some()
            && self.record.head.is_some()
            && self.state.as_ref().is_some_and(LocalState::synced)
    }

    pub fn icons(&self) -> String {
        if !self.registered || self.error.is_some() {
            return "?".into();
        }
        if !self.measured {
            return "…".into();
        }
        if self.synced() {
            return "✓".into();
        }
        let Some(state) = &self.state else {
            return "?".into();
        };
        let mut icons = String::new();
        if state.dirty() {
            icons.push('*');
        }
        if let Some(ahead) = state.ahead.filter(|&count| count > 0) {
            icons.push_str(&format!("↑{ahead}"));
        }
        if let Some(behind) = state.behind.filter(|&count| count > 0) {
            icons.push_str(&format!("↓{behind}"));
        }
        if self.record.branch.is_none() {
            icons.push('◇');
        } else if !state.remote_upstream || state.ahead.is_none() || state.behind.is_none() {
            icons.push('?');
        }
        if icons.is_empty() {
            icons.push('?');
        }
        icons
    }

    pub fn pending(record: WorktreeRecord) -> Self {
        Self {
            record,
            bytes: None,
            scan_errors: 0,
            state: None,
            error: None,
            measured: false,
            registered: true,
        }
    }
}

pub fn measure(
    record: &WorktreeRecord,
    common_dir: &Path,
    options: Options,
    cancel: &AtomicBool,
) -> WorktreeRow {
    let mut row = WorktreeRow::pending(record.clone());
    row.measured = true;
    if record.bare {
        return row;
    }
    if !record.path.is_dir() {
        row.error = Some("Checkout missing or unavailable".into());
        return row;
    }
    let runner = Runner::readonly(cancel);
    match crate::git_actions::inspect_checkout(&record.path, &runner).and_then(|repository| {
        let mut state = local_state(&record.path, &runner)?;
        state.remote_upstream = repository.upstream.is_some();
        Ok(state)
    }) {
        Ok(state) => row.state = Some(state),
        Err(error) => row.error = Some(error),
    }
    let mut excluded = vec![record.path.join(".git")];
    if common_dir.starts_with(&record.path) {
        excluded.push(common_dir.to_path_buf());
    }
    match scan::scan_excluding(&record.path, options, cancel, &excluded, |_| {}) {
        Ok(snapshot) => {
            row.bytes = Some(snapshot.nodes[0].size);
            row.scan_errors = snapshot.errors;
        }
        Err(error) => row.error = Some(error.to_string()),
    }
    row
}

pub enum WorktreeEvent {
    Listed(Result<Registry, String>),
    Measured(Box<WorktreeRow>),
    Done,
}
pub struct WorktreeScanTask {
    pub receiver: Receiver<WorktreeEvent>,
    cancel: Arc<AtomicBool>,
}
impl WorktreeScanTask {
    pub fn start(path: PathBuf, options: Options) -> Self {
        let (sender, receiver) = mpsc::sync_channel(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let registry = match discover(&path, &worker_cancel) {
                Ok(value) => value,
                Err(error) => {
                    let _ = sender.send(WorktreeEvent::Listed(Err(error)));
                    return;
                }
            };
            if sender
                .send(WorktreeEvent::Listed(Ok(registry.clone())))
                .is_err()
            {
                return;
            }
            // Keep the full registry for safety checks, but only measure linked
            // checkouts. The primary checkout / bare hub has its own Git menu.
            for record in registry
                .records
                .into_iter()
                .filter(|record| !record.main && !record.bare)
            {
                if worker_cancel.load(Ordering::Relaxed) {
                    return;
                }
                let row = measure(&record, &registry.common_dir, options, &worker_cancel);
                if sender.send(WorktreeEvent::Measured(Box::new(row))).is_err() {
                    return;
                }
            }
            let _ = sender.send(WorktreeEvent::Done);
        });
        Self { receiver, cancel }
    }
}
impl Drop for WorktreeScanTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct WorktreeManager {
    pub common_dir: PathBuf,
    pub selected: PathBuf,
    pub rows: Vec<WorktreeRow>,
    pub marked: HashSet<PathBuf>,
    pub loading: bool,
    pub message: String,
}
impl WorktreeManager {
    pub fn toggle_mark(&mut self) {
        if !self.marked.remove(&self.selected) {
            self.marked.insert(self.selected.clone());
        }
    }
    pub fn new(path: PathBuf) -> Self {
        Self {
            common_dir: path.clone(),
            selected: path,
            rows: Vec::new(),
            marked: HashSet::new(),
            loading: true,
            message: "Discovering registered worktrees…".into(),
        }
    }
    pub fn apply_registry(&mut self, registry: Registry) {
        self.common_dir = registry.common_dir;
        let leftovers = self.rows.iter().filter(|row| {
            let retained = self.marked.contains(&row.record.path) || self.selected == row.record.path;
            let registered = registry.records.iter().any(|record| record.path == row.record.path);
            let missing = matches!(std::fs::symlink_metadata(&row.record.path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
            retained && !registered && !missing
        }).map(|row| {
            let mut row = row.clone();
            row.registered = false;
            row.measured = true;
            row.bytes = None;
            row.state = None;
            row.error = Some("No longer registered with Git; a leftover path remains or could not be inspected. Check it in the shell; branch history is retained.".into());
            row
        }).collect::<Vec<_>>();
        self.rows = registry
            .records
            .into_iter()
            .filter(|record| !record.main && !record.bare)
            .map(WorktreeRow::pending)
            .collect();
        self.rows.extend(leftovers);
        if !self.rows.iter().any(|row| row.record.path == self.selected) {
            self.selected = self
                .rows
                .iter()
                .find(|row| self.marked.contains(&row.record.path))
                .or_else(|| {
                    self.rows
                        .iter()
                        .find(|row| self.selected.starts_with(&row.record.path))
                })
                .or(self.rows.first())
                .map(|row| row.record.path.clone())
                .unwrap_or_default();
        }
        self.marked
            .retain(|path| self.rows.iter().any(|row| &row.record.path == path));
        self.message = "Measuring checkouts · shared Git metadata excluded".into();
    }
    pub fn update(&mut self, row: WorktreeRow) {
        if let Some(entry) = self
            .rows
            .iter_mut()
            .find(|entry| entry.record.path == row.record.path)
        {
            *entry = row;
        }
        self.rows.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.record.path.cmp(&b.record.path))
        });
    }
    pub fn selected_row(&self) -> Option<&WorktreeRow> {
        self.rows
            .iter()
            .find(|row| row.record.path == self.selected)
    }
    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let index = self
            .rows
            .iter()
            .position(|row| row.record.path == self.selected)
            .unwrap_or(0);
        self.selected = self.rows[index.saturating_add_signed(delta).min(self.rows.len() - 1)]
            .record
            .path
            .clone();
    }
}
