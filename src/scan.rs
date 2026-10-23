#[cfg(unix)]
use std::collections::HashSet;
use std::{
    ffi::OsString,
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

pub type NodeId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
}

impl Identity {
    pub fn of(meta: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                device: meta.dev(),
                inode: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                created: meta.created().ok(),
            }
        }
    }
}

#[derive(Debug)]
pub struct Node {
    pub name: OsString,
    pub parent: Option<NodeId>,
    pub kind: Kind,
    pub size: u64,
    pub files: u64,
    pub children: Vec<NodeId>,
    pub error: Option<String>,
    pub hard_link: bool,
    pub identity: Identity,
    /// Newest modification in this subtree; None if metadata is incomplete.
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub apparent_size: bool,
}

#[derive(Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub errors: u64,
    pub elapsed: Duration,
    pub options: Options,
}

impl Snapshot {
    pub fn find_path(&self, path: &Path) -> Option<NodeId> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut id = 0;
        for part in relative.components() {
            id = *self.nodes[id]
                .children
                .iter()
                .find(|&&child| self.nodes[child].name == part.as_os_str())?;
        }
        Some(id)
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            parts.push(&self.nodes[current].name);
            current = parent;
        }
        let mut path = self.root.clone();
        for part in parts.into_iter().rev() {
            path.push(part);
        }
        path
    }

    pub fn is_within(&self, mut id: NodeId, ancestor: NodeId) -> bool {
        loop {
            if id == ancestor {
                return true;
            }
            match self.nodes[id].parent {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub entries: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current: PathBuf,
}

pub enum ScanEvent {
    Progress(Progress),
    Finished(io::Result<Snapshot>),
}

pub struct ScanTask {
    pub receiver: Receiver<ScanEvent>,
    cancel: Arc<AtomicBool>,
}

impl ScanTask {
    pub fn start(root: PathBuf, options: Options) -> Self {
        let (sender, receiver) = mpsc::sync_channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let result = scan(&root, options, &worker_cancel, |progress| {
                let _ = sender.try_send(ScanEvent::Progress(progress));
            });
            let _ = sender.send(ScanEvent::Finished(result));
        });
        Self { receiver, cancel }
    }
}

impl Drop for ScanTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn kind(meta: &Metadata) -> Kind {
    if meta.is_symlink() {
        Kind::Symlink
    } else if meta.is_dir() {
        Kind::Directory
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn file_size(meta: &Metadata, options: Options) -> u64 {
    if options.apparent_size {
        return meta.len();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Iterative traversal. Symlinks are recorded, never followed. Excluding directory
/// metadata makes file rectangles account exactly for directory totals.
pub fn scan(
    root: &Path,
    options: Options,
    cancel: &AtomicBool,
    report: impl FnMut(Progress),
) -> io::Result<Snapshot> {
    scan_excluding(root, options, cancel, &[], report)
}

pub(crate) fn scan_excluding(
    root: &Path,
    options: Options,
    cancel: &AtomicBool,
    excluded: &[PathBuf],
    mut report: impl FnMut(Progress),
) -> io::Result<Snapshot> {
    let started = Instant::now();
    let root = root.canonicalize()?;
    let meta = fs::symlink_metadata(&root)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scan target must be a directory",
        ));
    }
    let mut snapshot = Snapshot {
        nodes: vec![Node {
            name: root.as_os_str().to_owned(),
            parent: None,
            kind: Kind::Directory,
            size: 0,
            files: 0,
            children: Vec::new(),
            error: None,
            hard_link: false,
            identity: Identity::of(&meta),
            modified: meta.modified().ok(),
        }],
        root: root.clone(),
        errors: 0,
        elapsed: Duration::ZERO,
        options,
    };
    let mut pending = vec![(0, root)];
    #[cfg(unix)]
    let mut hard_links = HashSet::new();
    let mut progress = Progress::default();
    let mut last_report = Instant::now();
    while let Some((parent, path)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
        }
        progress.current = path.clone();
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                snapshot.nodes[parent].error = Some(error.to_string());
                snapshot.errors += 1;
                continue;
            }
        };
        let mut names = Vec::new();
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
            }
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                Err(error) => {
                    snapshot.nodes[parent].error = Some(error.to_string());
                    snapshot.errors += 1;
                }
            }
        }
        names.sort();
        for name in names {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
            }
            let child_path = path.join(&name);
            if excluded.contains(&child_path) {
                continue;
            }
            let meta = match fs::symlink_metadata(&child_path) {
                Ok(meta) => meta,
                Err(error) => {
                    snapshot.nodes[parent].error =
                        Some(format!("{}: {error}", crate::display_name(&name)));
                    snapshot.errors += 1;
                    continue;
                }
            };
            let kind = kind(&meta);
            let identity = Identity::of(&meta);
            #[cfg(unix)]
            let hard_link = {
                use std::os::unix::fs::MetadataExt;
                kind == Kind::File
                    && meta.nlink() > 1
                    && !hard_links.insert((meta.dev(), meta.ino()))
            };
            #[cfg(not(unix))]
            let hard_link = false;
            let size = if hard_link || matches!(kind, Kind::Directory | Kind::Other) {
                0
            } else {
                file_size(&meta, options)
            };
            let id = snapshot.nodes.len();
            snapshot.nodes.push(Node {
                name,
                parent: Some(parent),
                kind,
                size,
                files: u64::from(kind != Kind::Directory),
                children: Vec::new(),
                error: None,
                hard_link,
                identity,
                modified: meta.modified().ok(),
            });
            snapshot.nodes[parent].children.push(id);
            if kind == Kind::Directory {
                pending.push((id, child_path));
            }
            progress.entries += 1;
            progress.bytes = progress.bytes.saturating_add(size);
            progress.errors = snapshot.errors;
            if last_report.elapsed() >= Duration::from_millis(80) {
                report(progress.clone());
                last_report = Instant::now();
            }
        }
    }
    for id in (1..snapshot.nodes.len()).rev() {
        if snapshot.nodes[id].error.is_some() {
            snapshot.nodes[id].modified = None;
        }
        let node = &snapshot.nodes[id];
        let (parent, size, files, modified) =
            (node.parent.unwrap(), node.size, node.files, node.modified);
        snapshot.nodes[parent].modified = snapshot.nodes[parent]
            .modified
            .zip(modified)
            .map(|(a, b)| a.max(b));
        snapshot.nodes[parent].size = snapshot.nodes[parent].size.saturating_add(size);
        snapshot.nodes[parent].files = snapshot.nodes[parent].files.saturating_add(files);
    }
    if snapshot.nodes[0].error.is_some() {
        snapshot.nodes[0].modified = None;
    }
    snapshot.elapsed = started.elapsed();
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn run(path: &Path) -> Snapshot {
        scan(
            path,
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
    }
    #[test]
    fn aggregates_nested_hidden_and_empty_entries() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("nested/empty")).unwrap();
        fs::write(dir.path().join("nested/large.bin"), [0; 4096]).unwrap();
        fs::write(dir.path().join(".hidden"), [0; 10]).unwrap();
        let data = run(dir.path());
        assert_eq!(data.nodes[0].size, 4106);
        assert_eq!(data.nodes[0].files, 2);
        let id = data
            .nodes
            .iter()
            .position(|n| n.name == "large.bin")
            .unwrap();
        assert_eq!(data.path(id), dir.path().join("nested/large.bin"));
        assert!(data.is_within(id, 0));
        assert_eq!(data.errors, 0);
    }
    #[test]
    fn rejects_invalid_roots_and_obeys_cancellation() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file"), b"x").unwrap();
        for path in [dir.path().join("missing"), dir.path().join("file")] {
            assert!(scan(&path, Options::default(), &AtomicBool::new(false), |_| {}).is_err());
        }
        assert_eq!(
            scan(
                dir.path(),
                Options::default(),
                &AtomicBool::new(true),
                |_| {}
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[cfg(unix)]
    #[test]
    fn never_follows_symlinks_and_counts_hard_links_once() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a"), [0; 500]).unwrap();
        fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
        symlink(dir.path(), dir.path().join("loop")).unwrap();
        let data = run(dir.path());
        assert_eq!(data.nodes.len(), 4);
        assert_eq!(data.nodes.iter().filter(|n| n.hard_link).count(), 1);
        assert_eq!(
            data.nodes[0].size,
            500 + fs::symlink_metadata(dir.path().join("loop")).unwrap().len()
        );
    }
    #[cfg(unix)]
    #[test]
    fn allocated_size_handles_sparse_files() {
        let dir = tempdir().unwrap();
        let file = fs::File::create(dir.path().join("sparse")).unwrap();
        file.set_len(1024 * 1024).unwrap();
        let actual = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(actual.nodes[0].size < run(dir.path()).nodes[0].size);
    }
}
