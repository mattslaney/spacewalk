//! Checked, non-overwriting filesystem operations. Workers never interpret shell text.
use crate::scan::{Identity, Kind, NodeId, Snapshot, kind};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Copy,
    Move,
}

impl TransferMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Move => "Move",
        }
    }
}

#[derive(Clone, Debug)]
struct Stamp {
    path: PathBuf,
    identity: Identity,
    kind: Kind,
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn capture(path: PathBuf) -> io::Result<Self> {
        let meta = fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            identity: Identity::of(&meta),
            kind: kind(&meta),
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
    fn validate(&self, contents: bool) -> io::Result<()> {
        let now = Self::capture(self.path.clone())?;
        if self.identity != now.identity
            || self.kind != now.kind
            || (contents && (self.len != now.len || self.modified != now.modified))
        {
            return Err(io::Error::other(format!(
                "{} changed; rescan and try again",
                self.path.display()
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct FileItem {
    pub path: PathBuf,
    chain: Vec<Stamp>,
}

impl FileItem {
    pub fn from_snapshot(data: &Snapshot, id: NodeId) -> io::Result<Self> {
        let mut chain = Vec::new();
        let mut current = Some(id);
        while let Some(id) = current {
            let stamp = Stamp::capture(data.path(id))?;
            if stamp.identity != data.nodes[id].identity || stamp.kind != data.nodes[id].kind {
                return Err(io::Error::other("Selection changed; rescan and try again"));
            }
            chain.push(stamp);
            current = data.nodes[id].parent;
        }
        chain.reverse();
        Ok(Self {
            path: data.path(id),
            chain,
        })
    }
    fn validate(&self) -> io::Result<()> {
        for (index, stamp) in self.chain.iter().enumerate() {
            stamp.validate(index + 1 == self.chain.len())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TransferQueue {
    pub mode: TransferMode,
    pub items: Vec<FileItem>,
}

impl TransferQueue {
    pub fn new(
        data: &Snapshot,
        ids: impl IntoIterator<Item = NodeId>,
        mode: TransferMode,
    ) -> io::Result<Self> {
        let ids: HashSet<_> = ids.into_iter().collect();
        if mode == TransferMode::Move && ids.contains(&0) {
            return Err(io::Error::other("The scan root cannot be moved"));
        }
        let mut ids_sorted: Vec<_> = ids.iter().copied().collect();
        ids_sorted.sort_unstable();
        let mut items = Vec::new();
        for id in ids_sorted {
            let mut parent = data.nodes[id].parent;
            let mut covered = false;
            while let Some(p) = parent {
                if ids.contains(&p) {
                    covered = true;
                    break;
                }
                parent = data.nodes[p].parent;
            }
            if !covered {
                items.push(FileItem::from_snapshot(data, id)?);
            }
        }
        if items.is_empty() {
            return Err(io::Error::other("No items selected"));
        }
        Ok(Self { mode, items })
    }
}

#[derive(Clone, Debug)]
pub struct Directory {
    pub path: PathBuf,
    chain: Vec<Stamp>,
}

impl Directory {
    pub fn capture(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        if !path.is_dir() {
            return Err(io::Error::other(
                "Destination must be an existing directory",
            ));
        }
        let mut chain = Vec::new();
        for ancestor in path.ancestors() {
            chain.push(Stamp::capture(ancestor.to_path_buf())?);
        }
        Ok(Self { path, chain })
    }
    pub(crate) fn validate(&self) -> io::Result<()> {
        for stamp in &self.chain {
            stamp.validate(false)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum FileRequest {
    Paste {
        queue: TransferQueue,
        destination: Directory,
    },
    Create {
        parent: Directory,
        name: String,
    },
    Rename {
        item: FileItem,
        parent: Directory,
        name: String,
    },
}

pub fn validate_name(name: &str) -> io::Result<()> {
    let mut parts = Path::new(name).components();
    if name.is_empty()
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
        || name.contains(['/', '\\', '\0'])
    {
        Err(io::Error::other(
            "Enter a single folder or file name, without path separators",
        ))
    } else {
        Ok(())
    }
}

fn absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; nothing will be overwritten",
                path.display()
            ),
        )),
    }
}

pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        let _ = (source, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Non-overwriting rename is supported on Linux and macOS",
        ))
    }
}

#[derive(Default, Debug)]
pub struct FileOutcome {
    pub completed: Vec<PathBuf>,
    pub selected: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum FileEvent {
    Progress(String),
    Finished(FileOutcome),
}

pub struct FileTask {
    pub receiver: Receiver<FileEvent>,
    cancel: Arc<AtomicBool>,
}

impl FileTask {
    pub fn start(request: FileRequest) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let mut last_update = Instant::now() - Duration::from_secs(1);
            let result = request.execute(&worker_cancel, |text| {
                if last_update.elapsed() >= Duration::from_millis(50) {
                    let _ = sender.send(FileEvent::Progress(text));
                    last_update = Instant::now();
                }
            });
            let _ = sender.send(FileEvent::Finished(result));
        });
        Self { receiver, cancel }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Cancelled; completed items are kept",
        ))
    } else {
        Ok(())
    }
}

struct CopyStaging(tempfile::TempDir);

impl Drop for CopyStaging {
    fn drop(&mut self) {
        // Copied directory modes may be read-only. Make only our private staging
        // directories writable before TempDir removes a failed/cancelled copy.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut pending = vec![self.0.path().to_path_buf()];
            while let Some(path) = pending.pop() {
                if let Ok(meta) = fs::symlink_metadata(&path)
                    && meta.is_dir()
                {
                    let _ = fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(meta.permissions().mode() | 0o700),
                    );
                    if let Ok(entries) = fs::read_dir(path) {
                        pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
                    }
                }
            }
        }
    }
}

fn inventory(item: &FileItem, cancel: &AtomicBool) -> io::Result<Vec<Stamp>> {
    item.validate()?;
    let mut pending = vec![item.path.clone()];
    let mut stamps = Vec::new();
    while let Some(path) = pending.pop() {
        cancelled(cancel)?;
        let stamp = Stamp::capture(path.clone())?;
        match stamp.kind {
            Kind::Directory => {
                let mut children = fs::read_dir(&path)?
                    .map(|e| e.map(|e| e.path()))
                    .collect::<io::Result<Vec<_>>>()?;
                children.sort();
                children.reverse();
                pending.extend(children);
            }
            Kind::File | Kind::Symlink => {}
            Kind::Other => {
                return Err(io::Error::other(format!(
                    "Unsupported special file: {}",
                    path.display()
                )));
            }
        }
        stamps.push(stamp);
    }
    Ok(stamps)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    entries: &[Stamp],
    cancel: &AtomicBool,
    progress: &mut impl FnMut(String),
) -> io::Result<()> {
    for (index, stamp) in entries.iter().enumerate() {
        cancelled(cancel)?;
        stamp.validate(true)?;
        let relative = stamp.path.strip_prefix(source).map_err(io::Error::other)?;
        let target = if relative.as_os_str().is_empty() {
            destination.to_path_buf()
        } else {
            destination.join(relative)
        };
        progress(format!(
            "Copying {} · {}/{} entries",
            stamp.path.display(),
            index + 1,
            entries.len()
        ));
        match stamp.kind {
            Kind::Directory => fs::create_dir(&target)?,
            Kind::File => {
                let mut input = File::open(&stamp.path)?;
                if Identity::of(&input.metadata()?) != stamp.identity {
                    return Err(io::Error::other("Source changed while opening"));
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                let mut buffer = [0; 128 * 1024];
                let mut length = 0;
                loop {
                    cancelled(cancel)?;
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    if buffer[..n].iter().all(|&byte| byte == 0) {
                        output.seek(SeekFrom::Current(n as i64))?;
                    } else {
                        output.write_all(&buffer[..n])?;
                    }
                    length += n as u64;
                }
                output.set_len(length)?;
                output.set_permissions(input.metadata()?.permissions())?;
                if let Some(modified) = stamp.modified {
                    output.set_times(fs::FileTimes::new().set_modified(modified))?;
                }
                stamp.validate(true)?;
            }
            Kind::Symlink => {
                let link = fs::read_link(&stamp.path)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(link, &target)?;
                #[cfg(windows)]
                {
                    if stamp.path.is_dir() {
                        std::os::windows::fs::symlink_dir(link, &target)?;
                    } else {
                        std::os::windows::fs::symlink_file(link, &target)?;
                    }
                }
            }
            Kind::Other => unreachable!(),
        }
    }
    // Directory permissions are applied last so read-only folders can be copied.
    for stamp in entries.iter().rev().filter(|e| e.kind == Kind::Directory) {
        let target = destination.join(stamp.path.strip_prefix(source).map_err(io::Error::other)?);
        fs::set_permissions(target, fs::symlink_metadata(&stamp.path)?.permissions())?;
    }
    Ok(())
}

impl FileRequest {
    pub fn execute(self, cancel: &AtomicBool, mut progress: impl FnMut(String)) -> FileOutcome {
        let mut outcome = FileOutcome::default();
        let result = (|| -> io::Result<()> {
            cancelled(cancel)?;
            match self {
                Self::Create { parent, name } => {
                    validate_name(&name)?;
                    parent.validate()?;
                    let target = parent.path.join(name);
                    fs::create_dir(&target)?;
                    outcome.selected = Some(target);
                }
                Self::Rename { item, parent, name } => {
                    if item.chain.len() == 1 {
                        return Err(io::Error::other("The scan root cannot be renamed"));
                    }
                    validate_name(&name)?;
                    item.validate()?;
                    parent.validate()?;
                    let target = parent.path.join(name);
                    absent(&target)?;
                    rename_noreplace(&item.path, &target)?;
                    outcome.completed.push(item.path);
                    outcome.selected = Some(target);
                }
                Self::Paste { queue, destination } => {
                    destination.validate()?;
                    let mut targets = HashSet::new();
                    let mut manifests = Vec::new();
                    for item in &queue.items {
                        if queue.mode == TransferMode::Move && item.chain.len() == 1 {
                            return Err(io::Error::other("The scan root cannot be moved"));
                        }
                        cancelled(cancel)?;
                        let target =
                            destination.path.join(item.path.file_name().ok_or_else(|| {
                                io::Error::other("Cannot copy a filesystem root")
                            })?);
                        absent(&target)?;
                        if !targets.insert(target.clone()) {
                            return Err(io::Error::other(
                                "Queued items have the same destination name",
                            ));
                        }
                        if destination.path.starts_with(&item.path) {
                            return Err(io::Error::other(
                                "Cannot paste a directory into itself or a descendant",
                            ));
                        }
                        progress(format!("Checking {}…", item.path.display()));
                        manifests.push((target, inventory(item, cancel)?));
                    }
                    for (item, (target, entries)) in queue.items.iter().zip(manifests) {
                        cancelled(cancel)?;
                        destination.validate()?;
                        item.validate()?;
                        absent(&target)?;
                        if queue.mode == TransferMode::Move {
                            match rename_noreplace(&item.path, &target) {
                                Ok(()) => {
                                    outcome.completed.push(item.path.clone());
                                    outcome.selected = Some(target);
                                    continue;
                                }
                                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
                                Err(e) => return Err(e),
                            }
                        }
                        let staging = CopyStaging(
                            tempfile::Builder::new()
                                .prefix(".spacewalk-copy-")
                                .tempdir_in(&destination.path)?,
                        );
                        let staged = staging.0.path().join("item");
                        copy_tree(&item.path, &staged, &entries, cancel, &mut progress)?;
                        cancelled(cancel)?;
                        destination.validate()?;
                        item.validate()?;
                        for stamp in &entries {
                            stamp.validate(true)?;
                        }
                        rename_noreplace(&staged, &target)?;
                        if queue.mode == TransferMode::Move {
                            // A cross-device move only removes an unchanged source after installing the entire copy.
                            for stamp in &entries {
                                stamp.validate(true)?;
                            }
                            if entries[0].kind == Kind::Directory {
                                fs::remove_dir_all(&item.path)?;
                            } else {
                                fs::remove_file(&item.path)?;
                            }
                        }
                        outcome.completed.push(item.path.clone());
                        outcome.selected = Some(target);
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            outcome.error = Some(e.to_string());
        }
        outcome
    }
}

/// Complete directory names without interpreting shell expansion or commands.
pub fn complete_directory(input: &str, base: &Path) -> io::Result<String> {
    let path = Path::new(input);
    let (parent, prefix) = if input.ends_with(std::path::MAIN_SEPARATOR) {
        (path, OsStr::new(""))
    } else {
        (
            path.parent().unwrap_or(Path::new("")),
            path.file_name().unwrap_or(OsStr::new("")),
        )
    };
    let absolute = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        base.join(parent)
    };
    let mut matches = fs::read_dir(absolute)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().is_dir()
                && entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(prefix.as_encoded_bytes())
        })
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    matches.sort();
    if matches.len() == 1 {
        Ok(format!(
            "{}{sep}",
            parent.join(&matches[0]).to_string_lossy(),
            sep = std::path::MAIN_SEPARATOR
        ))
    } else {
        Err(io::Error::other(format!(
            "{} matching directories; type more of the path",
            matches.len()
        )))
    }
}
