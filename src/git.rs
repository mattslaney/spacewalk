use crate::scan::{Kind, NodeId, Snapshot};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum RepoState {
    Checking,
    Ready(RepoStatus),
    Unavailable(String),
}

#[derive(Debug, Default)]
pub struct RepoStatus {
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    pub no_upstream: usize,
    pub gone: usize,
    pub branches: usize,
    pub has_remote: bool,
    pub fetched: bool,
    pub detached: bool,
    pub unborn: bool,
    pub stashed: bool,
    pub bare: bool,
}

impl RepoStatus {
    pub fn synced(&self) -> bool {
        self.has_remote
            && self.fetched
            && self.branches > 0
            && !self.dirty
            && self.ahead == 0
            && self.behind == 0
            && self.no_upstream == 0
            && self.gone == 0
            && !self.detached
            && !self.unborn
            && !self.stashed
    }
}

impl RepoState {
    pub fn icons(&self) -> String {
        let Self::Ready(status) = self else {
            return match self {
                Self::Checking => "⎇ …".into(),
                _ => "⎇ ?".into(),
            };
        };
        if status.synced() {
            return "⎇ ✓".into();
        }
        let mut icons = String::from("⎇ ");
        if !status.has_remote {
            icons.push('∅');
        }
        if status.dirty {
            icons.push('*');
        }
        if status.ahead > 0 {
            icons.push('↑');
        }
        if status.behind > 0 {
            icons.push('↓');
        }
        if status.detached {
            icons.push('◇');
        }
        if status.stashed {
            icons.push('≡');
        }
        if status.has_remote
            && (!status.fetched
                || status.no_upstream > 0
                || status.gone > 0
                || status.unborn
                || status.branches == 0)
        {
            icons.push('?');
        }
        icons
    }

    pub fn summary(&self) -> String {
        let status = match self {
            Self::Checking => return "Git: checking repository in background…".into(),
            Self::Unavailable(error) => return format!("Git: status unavailable ({error})"),
            Self::Ready(status) => status,
        };
        if status.synced() {
            return "Git: clean; every local branch matches its remote upstream (last-fetched refs)".into();
        }
        let mut details = vec![if status.bare {
            "bare repository".into()
        } else if status.dirty {
            "working tree changes".into()
        } else {
            "clean working tree".into()
        }];
        if !status.has_remote {
            details.push("no configured remote".into());
        } else if !status.fetched {
            details.push("no fetched remote refs".into());
        }
        for (count, label) in [
            (status.ahead, "ahead"),
            (status.behind, "behind"),
            (status.no_upstream, "without a remote upstream"),
            (status.gone, "with a missing upstream"),
        ] {
            if count > 0 {
                let branch = if count == 1 { "branch" } else { "branches" };
                details.push(format!("{count} {branch} {label}"));
            }
        }
        if status.detached {
            details.push("detached HEAD; sync unverified".into());
        }
        if status.unborn {
            details.push("no commits".into());
        }
        if status.stashed {
            details.push("stash present".into());
        }
        if status.has_remote && status.fetched {
            details.push("last-fetched refs".into());
        }
        format!("Git: {}", details.join(" · "))
    }
}

/// Detect repository roots from the existing scan, including .git files used
/// by worktrees/submodules and bare repositories. No filesystem calls here.
pub fn discover(data: &Snapshot) -> HashMap<NodeId, RepoState> {
    let mut repositories = HashMap::new();
    let mut metadata = vec![false; data.nodes.len()];
    for (id, node) in data.nodes.iter().enumerate() {
        metadata[id] = node.name == ".git" || node.parent.is_some_and(|p| metadata[p]);
        if node.kind != Kind::Directory || metadata[id] {
            continue;
        }
        let has = |name: &str, kind: Kind| {
            node.children.iter().any(|&child| {
                let child = &data.nodes[child];
                child.name == name && child.kind == kind
            })
        };
        if has(".git", Kind::Directory)
            || has(".git", Kind::File)
            || (has("HEAD", Kind::File)
                && has("objects", Kind::Directory)
                && has("refs", Kind::Directory))
        {
            repositories.insert(id, RepoState::Checking);
        }
    }
    repositories
}

pub struct GitTask {
    pub receiver: Receiver<(NodeId, RepoState)>,
    cancel: Arc<AtomicBool>,
}

impl GitTask {
    pub fn start(repositories: Vec<(NodeId, PathBuf)>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        // One worker bounds Git subprocess load even on scans with many repos.
        thread::spawn(move || {
            for (id, path) in repositories {
                if worker_cancel.load(Ordering::Relaxed) {
                    break;
                }
                let state = match inspect(&path, &worker_cancel) {
                    Ok(status) => RepoState::Ready(status),
                    Err(error) => RepoState::Unavailable(error),
                };
                if sender.send((id, state)).is_err() {
                    break;
                }
            }
        });
        Self { receiver, cancel }
    }
}

impl Drop for GitTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn inspect(path: &Path, cancel: &AtomicBool) -> Result<RepoStatus, String> {
    let bare = run(path, &["rev-parse", "--is-bare-repository"], cancel)?;
    let mut status = RepoStatus {
        bare: bare == b"true\n",
        ..RepoStatus::default()
    };
    let remotes = run(path, &["remote"], cancel)?;
    status.has_remote = !remotes.is_empty();
    if !status.bare {
        let output = run(
            path,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=normal",
                "--ignore-submodules=none",
            ],
            cancel,
        )?;
        for record in output.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            if record == b"# branch.oid (initial)" {
                status.unborn = true;
            } else if record == b"# branch.head (detached)" {
                status.detached = true;
            } else if !record.starts_with(b"# ") {
                status.dirty = true;
                break;
            }
        }
    }
    let output = run(
        path,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(upstream)%00%(upstream:track)%00",
            "refs/heads",
            "refs/remotes",
            "refs/stash",
        ],
        cancel,
    )?;
    for line in output.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<_> = line.split(|&b| b == 0).collect();
        if fields.len() != 4 {
            return Err("unexpected Git branch output".into());
        }
        if fields[0].starts_with(b"refs/remotes/") {
            status.fetched = true;
        } else if fields[0] == b"refs/stash" {
            status.stashed = true;
        } else if fields[0].starts_with(b"refs/heads/") {
            status.branches += 1;
            if !fields[1].starts_with(b"refs/remotes/") {
                status.no_upstream += 1;
                continue;
            }
            let track =
                std::str::from_utf8(fields[2]).map_err(|_| "invalid Git tracking output")?;
            status.ahead += usize::from(track.contains("ahead "));
            status.behind += usize::from(track.contains("behind "));
            status.gone += usize::from(track == "[gone]");
        }
    }
    Ok(status)
}

fn run(path: &Path, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Git check cancelled".into());
    }
    let mut command = Command::new("git");
    command
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start Git: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped Git output");
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        let mut too_large = false;
        loop {
            let count = stdout.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if output.len() + count <= 4 * 1024 * 1024 {
                output.extend_from_slice(&buffer[..count]);
            } else {
                too_large = true;
            }
        }
        if too_large {
            Err("Git output exceeded the status limit".into())
        } else {
            Ok(output)
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Git check cancelled or timed out".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return reader.join().map_err(|_| "Git output reader stopped")?;
            }
            Ok(Some(_)) => return Err(format!("git {} failed", args[0])),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::fs;

    fn git(path: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/nonexistent",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn fixture(remote: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "--initial-branch=main"]);
        git(&path, &["config", "user.name", "Spacewalk Test"]);
        git(&path, &["config", "user.email", "test@example.invalid"]);
        fs::write(path.join("tracked"), "initial").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-m", "initial"]);
        if remote {
            let bare = dir.path().join("remote.git");
            git(
                dir.path(),
                &[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    bare.to_str().unwrap(),
                ],
            );
            git(&path, &["remote", "add", "origin", bare.to_str().unwrap()]);
            git(&path, &["push", "-u", "origin", "main"]);
        }
        (dir, path)
    }

    fn status(path: &Path) -> RepoStatus {
        inspect(path, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn synced_requires_a_clean_worktree_and_remote_tracking_branches() {
        let (_dir, path) = fixture(true);
        assert!(status(&path).synced());
        fs::write(path.join("untracked"), "local").unwrap();
        assert!(status(&path).dirty);
        fs::remove_file(path.join("untracked")).unwrap();
        fs::write(path.join("tracked"), "unstaged").unwrap();
        assert!(status(&path).dirty);
        git(&path, &["add", "tracked"]);
        assert!(status(&path).dirty);
        git(&path, &["reset", "--hard", "HEAD"]);
        assert!(status(&path).synced());
    }

    #[test]
    fn non_current_branches_with_unpushed_commits_are_detected() {
        let (_dir, path) = fixture(true);
        git(&path, &["switch", "-c", "feature"]);
        git(&path, &["push", "-u", "origin", "feature"]);
        git(&path, &["commit", "--allow-empty", "-m", "local feature"]);
        git(&path, &["switch", "main"]);
        let state = status(&path);
        assert!(!state.dirty);
        assert_eq!(state.branches, 2);
        assert_eq!(state.ahead, 1);
        assert!(!state.synced());
        assert_eq!(RepoState::Ready(state).icons(), "⎇ ↑");
        git(&path, &["branch", "untracked-branch"]);
        assert_eq!(status(&path).no_upstream, 1);
    }

    #[test]
    fn behind_and_missing_upstreams_are_not_reported_as_synced() {
        let (_dir, path) = fixture(true);
        git(&path, &["commit", "--allow-empty", "-m", "remote commit"]);
        git(&path, &["push"]);
        git(&path, &["reset", "--hard", "HEAD~1"]);
        assert_eq!(status(&path).behind, 1);
        git(&path, &["update-ref", "-d", "refs/remotes/origin/main"]);
        let state = status(&path);
        assert_eq!(state.gone, 1);
        assert!(state.has_remote);
        assert!(!state.fetched);
        assert!(!state.synced());
    }

    #[test]
    fn no_remote_has_a_distinct_icon_from_unfetched_or_untracked_repositories() {
        let (_dir, path) = fixture(false);
        let state = status(&path);
        assert!(!state.has_remote);
        assert_eq!(RepoState::Ready(state).icons(), "⎇ ∅");
        git(
            &path,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/unreachable.git",
            ],
        );
        let state = status(&path);
        assert!(state.has_remote);
        assert!(!state.fetched);
        assert_eq!(RepoState::Ready(state).icons(), "⎇ ?");
    }

    #[test]
    fn detached_heads_stashes_and_unborn_branches_are_not_marked_synced() {
        let (_dir, path) = fixture(true);
        git(&path, &["switch", "--detach"]);
        let state = status(&path);
        assert!(state.detached);
        assert!(!state.synced());
        git(&path, &["switch", "main"]);
        fs::write(path.join("tracked"), "stashed work").unwrap();
        git(&path, &["stash", "push"]);
        let state = status(&path);
        assert!(!state.dirty);
        assert!(state.stashed);
        assert!(!state.synced());
        let empty = path.parent().unwrap().join("empty");
        fs::create_dir(&empty).unwrap();
        git(&empty, &["init", "--initial-branch=main"]);
        let state = status(&empty);
        assert!(state.unborn);
        assert!(!state.synced());
    }

    #[test]
    fn discovers_worktrees_bare_repositories_and_nested_roots_without_metadata_duplicates() {
        let (dir, path) = fixture(true);
        let worktree = dir.path().join("worktree");
        git(
            &path,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                worktree.to_str().unwrap(),
            ],
        );
        assert_eq!(status(&worktree).no_upstream, 1);
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let roots = discover(&data);
        for path in [&path, &worktree, &dir.path().join("remote.git")] {
            assert!(roots.contains_key(&data.find_path(path).unwrap()));
        }
        assert_eq!(roots.len(), 3);
        assert!(status(&dir.path().join("remote.git")).bare);
        assert!(!roots.contains_key(&data.find_path(&path.join(".git")).unwrap()));
        assert!(!roots.contains_key(&0));
    }

    #[test]
    fn cancelled_and_invalid_checks_remain_unknown() {
        let (_dir, path) = fixture(true);
        assert!(inspect(&path, &AtomicBool::new(true)).is_err());
        let invalid = tempfile::tempdir().unwrap();
        assert!(inspect(invalid.path(), &AtomicBool::new(false)).is_err());
        assert_eq!(RepoState::Unavailable("Git missing".into()).icons(), "⎇ ?");
        assert_eq!(RepoState::Checking.icons(), "⎇ …");
    }
}
