//! On-demand Git reports and mutations, isolated from the automatic status worker.
use crate::{
    display_name,
    files::Directory,
    scan::{Identity, Kind, kind},
};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitAction {
    Pull,
    Push,
    Clean,
    Reset,
}

impl GitAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pull => "Pull",
            Self::Push => "Push",
            Self::Clean => "Clean checkout",
            Self::Reset => "Reset to remote",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitCommand {
    Status,
    Diff,
    Branches,
    LocalCommits,
    Stashes,
    Fetch,
}

impl GitCommand {
    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Diff => "Diff",
            Self::Branches => "Branches",
            Self::LocalCommits => "Local commits",
            Self::Stashes => "Stashes",
            Self::Fetch => "Fetch",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GitOptions {
    pub force: bool,
    pub discard_commits: bool,
    pub delete_branches: bool,
    pub clear_stashes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    pub remote: String,
    pub remote_ref: String,
    pub tracking_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<Upstream>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PathState {
    path: PathBuf,
    identity: Identity,
    kind: Kind,
    len: u64,
    modified: Option<SystemTime>,
    nested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    status: Vec<u8>,
    refs: Vec<u8>,
    worktrees: Vec<u8>,
    config: Vec<u8>,
    head: Vec<u8>,
    files: Vec<PathState>,
}

#[derive(Clone, Debug)]
pub struct GitPreview {
    pub repository: Repository,
    pub action: GitAction,
    pub options: GitOptions,
    pub text: String,
    target: String,
    remote_expected: String,
    delete_branches: Vec<(String, String)>,
    fingerprint: Fingerprint,
    anchor: Directory,
}

pub enum GitJob {
    Inspect(PathBuf),
    Report {
        path: PathBuf,
        command: GitCommand,
    },
    Preview {
        path: PathBuf,
        action: GitAction,
        options: GitOptions,
    },
    Execute(Box<GitPreview>),
}
pub enum GitEvent {
    Progress(String),
    Inspected(Result<Repository, String>),
    Reported {
        command: GitCommand,
        result: Result<String, String>,
    },
    Previewed(Result<Box<GitPreview>, String>),
    Finished(Result<String, String>),
}
pub struct GitActionTask {
    pub receiver: Receiver<GitEvent>,
    cancel: Arc<AtomicBool>,
}

impl GitActionTask {
    pub fn start(job: GitJob) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let runner = Runner {
                cancel: &worker_cancel,
                events: Some(&sender),
                read_only: false,
            };
            let event = match job {
                GitJob::Inspect(path) => GitEvent::Inspected(inspect_with(&path, &runner)),
                GitJob::Report { path, command } => GitEvent::Reported {
                    command,
                    result: report_with(&path, command, &runner),
                },
                GitJob::Preview { path, action, options } => GitEvent::Previewed(prepare_with(&path, action, options, &runner).map(Box::new)),
                GitJob::Execute(preview) => GitEvent::Finished(execute_with(&preview, &runner).map_err(|e| format!("{e}\n\nSome steps may have completed. Refresh and review before retrying."))),
            };
            let _ = sender.send(event);
        });
        Self { receiver, cancel }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct Runner<'a> {
    cancel: &'a AtomicBool,
    events: Option<&'a Sender<GitEvent>>,
    read_only: bool,
}

fn command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "--no-pager",
            "-c",
            "color.status=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotePath=true",
            "-c",
            "submodule.recurse=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-C",
        ])
        .arg(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_OPTIONAL_LOCKS",
    ] {
        command.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

fn stop_child(child: &mut std::process::Child) {
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

impl Runner<'_> {
    pub(crate) fn readonly(cancel: &AtomicBool) -> Runner<'_> {
        Runner {
            cancel,
            events: None,
            read_only: true,
        }
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err("Git operation cancelled; no rollback is implied".into())
        } else {
            Ok(())
        }
    }
    fn progress(&self, message: String) {
        if let Some(events) = self.events {
            let _ = events.send(GitEvent::Progress(message));
        }
    }
    pub(crate) fn run(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        self.run_os(root, &args.iter().map(OsString::from).collect::<Vec<_>>())
    }
    pub(crate) fn run_os(&self, root: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
        self.check()?;
        self.progress(visible(
            format!(
                "git {}",
                args.iter()
                    .map(|arg| display_name(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .as_bytes(),
        ));
        let mut cmd = command(root);
        if self.read_only || matches!(args.first().and_then(|arg| arg.to_str()), Some("status" | "diff" | "log")) {
            cmd.env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_NO_LAZY_FETCH", "1");
        }
        let verb = args.first().and_then(|arg| arg.to_str()).unwrap_or("");
        if matches!(verb, "fetch" | "push" | "ls-remote") {
            // Preserve configured SSH identities/options, while preventing an unseen
            // terminal passphrase prompt. Interactive auth remains in the shell.
            let configured = std::env::var("GIT_SSH_COMMAND")
                .ok()
                .or_else(|| {
                    command(root)
                        .args(["config", "--get", "core.sshCommand"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .map(|s| s.trim_end().to_owned())
                })
                .or_else(|| {
                    std::env::var("GIT_SSH")
                        .ok()
                        .map(|value| format!("'{}'", value.replace('\'', "'\\''")))
                })
                .unwrap_or_else(|| "ssh".into());
            cmd.env(
                "GIT_SSH_COMMAND",
                format!("{configured} -o BatchMode=yes -o ConnectTimeout=15"),
            );
        }
        let mut child = cmd.args(args).spawn().map_err(|e| {
            format!("Cannot start Git: {e}. Use the shell to check your Git installation.")
        })?;
        let mut stdout = child.stdout.take().unwrap();
        let stream_stdout = matches!(
            verb,
            "fetch" | "push" | "merge" | "reset" | "clean" | "branch" | "worktree"
        );
        let recent_stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
        let worker_stdout = Arc::clone(&recent_stdout);
        let (stdout_sender, stdout_receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> Result<Vec<u8>, String> {
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                loop {
                    let count = stdout.read(&mut buffer).map_err(|e| e.to_string())?;
                    if count == 0 {
                        break;
                    }
                    if bytes.len() + count > OUTPUT_LIMIT {
                        return Err(
                            "Git output is too large to preview safely; use the shell".into()
                        );
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if stream_stdout {
                        let mut recent = worker_stdout.lock().unwrap();
                        recent.extend_from_slice(&buffer[..count]);
                        if recent.len() > 8192 {
                            let excess = recent.len() - 8192;
                            recent.drain(..excess);
                        }
                    }
                }
                Ok(bytes)
            })();
            let _ = stdout_sender.send(result);
        });
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
        let worker_stderr = Arc::clone(&stderr);
        let mut pipe = child.stderr.take().unwrap();
        let (stderr_sender, stderr_receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(n) = pipe.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut output = worker_stderr.lock().unwrap();
                output.extend_from_slice(&buffer[..n]);
                if output.len() > 1024 * 1024 {
                    let excess = output.len() - 1024 * 1024;
                    output.drain(..excess);
                }
            }
            let _ = stderr_sender.send(());
        });
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut last_progress = Instant::now();
        let mut last_stdout = Vec::new();
        let mut last_stderr = Vec::new();
        let result = loop {
            if self.cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                stop_child(&mut child);
                break Err("Git operation cancelled or timed out. Its remote outcome may be unknown; review in the shell before retrying.".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    if last_progress.elapsed() >= Duration::from_millis(250) {
                        for (buffer, previous) in [
                            (&recent_stdout, &mut last_stdout),
                            (&stderr, &mut last_stderr),
                        ] {
                            let output = buffer.lock().unwrap();
                            if !output.is_empty() && *output != *previous {
                                self.progress(
                                    visible(&output)
                                        .lines()
                                        .last()
                                        .unwrap_or("Working…")
                                        .to_owned(),
                                );
                                previous.clone_from(&output);
                            }
                        }
                        last_progress = Instant::now();
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    stop_child(&mut child);
                    break Err(e.to_string());
                }
            }
        };
        let status = result?;
        let _ = stderr_receiver.recv_timeout(Duration::from_secs(1));
        let output = stdout_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| {
                "Git output did not close; inspect the repository in the shell".to_owned()
            })??;
        if !status.success() {
            return Err(format!(
                "git {} failed: {}\n{}\nInteractive authentication or conflict resolution can be handled with t in the shell.",
                display_name(&args[0]),
                visible(&stderr.lock().unwrap()),
                visible(&output)
            ));
        }
        Ok(output)
    }

    pub(crate) fn text(&self, root: &Path, args: &[&str]) -> Result<String, String> {
        String::from_utf8(self.run(root, args)?)
            .map(|s| s.trim_end_matches('\n').to_owned())
            .map_err(|_| "Git reference is not valid UTF-8; use the shell".into())
    }
}

/// Remove terminal control bytes and embedded HTTP credentials from user-visible output.
pub fn visible(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut result = String::new();
    for part in text.split_inclusive(char::is_whitespace) {
        let mut word = part.to_owned();
        for scheme in ["https://", "http://"] {
            if let Some(start) = word.to_ascii_lowercase().find(scheme) {
                let authority = start + scheme.len();
                let end = word[authority..]
                    .find(['/', '?', '#'])
                    .map_or(word.len(), |n| authority + n);
                if let Some(at) = word[authority..end].rfind('@') {
                    word.replace_range(authority..=authority + at, "");
                }
            }
        }
        result.extend(word.chars().map(|c| {
            if matches!(c, '\n' | '\r') {
                '\n'
            } else if c.is_control() {
                ' '
            } else {
                c
            }
        }));
    }
    result
}

fn path_from_output(mut bytes: Vec<u8>) -> PathBuf {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn inspect_with(path: &Path, runner: &Runner<'_>) -> Result<Repository, String> {
    let root = path_from_output(
        runner
            .run(path, &["rev-parse", "--show-toplevel"])
            .map_err(|e| {
                format!(
                    "Select a working-tree repository. Bare repositories are not supported.\n{e}"
                )
            })?,
    );
    let branch = runner
        .text(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok();
    let head = runner.text(&root, &["rev-parse", "--verify", "HEAD"]).ok();
    let upstream = if let Some(branch) = &branch {
        let values = runner.text(
            &root,
            &[
                "for-each-ref",
                "--format=%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        let values: Vec<_> = values.split('\0').collect();
        if values.len() == 3
            && !values[0].is_empty()
            && values[0] != "."
            && !values[0].starts_with('-')
            && values[1].starts_with("refs/heads/")
            && values[2].starts_with("refs/remotes/")
        {
            Some(Upstream {
                remote: values[0].into(),
                remote_ref: values[1].into(),
                tracking_ref: values[2].into(),
            })
        } else {
            None
        }
    } else {
        None
    };
    Ok(Repository {
        root,
        branch,
        head,
        upstream,
    })
}

pub fn inspect(path: &Path, cancel: &AtomicBool) -> Result<Repository, String> {
    inspect_with(
        path,
        &Runner {
            cancel,
            events: None,
            read_only: false,
        },
    )
}

/// Keep reports small enough to wrap and scroll responsively in a floating panel.
fn report_output(bytes: &[u8], empty: &str) -> String {
    let text = visible(bytes);
    if text.trim().is_empty() {
        return empty.into();
    }
    let mut output = String::new();
    for (index, line) in text.lines().enumerate() {
        if index >= 1000 || output.len() + line.len() > 128 * 1024 {
            output.push_str("\n[Display limited; use the shell for the full output.]\n");
            break;
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

pub fn report(path: &Path, command: GitCommand, cancel: &AtomicBool) -> Result<String, String> {
    report_with(
        path,
        command,
        &Runner {
            cancel,
            events: None,
        },
    )
}

fn report_with(path: &Path, command: GitCommand, runner: &Runner<'_>) -> Result<String, String> {
    let repo = inspect_with(path, runner)?;
    let root = &repo.root;
    let mut text = format!(
        "Repository: {}\nBranch: {}\n\n",
        display_name(root.as_os_str()),
        repo.branch.as_deref().unwrap_or("detached HEAD"),
    );
    match command {
        GitCommand::Status => {
            text.push_str(&report_output(
                &runner.run(
                    root,
                    &[
                        "status",
                        "--short",
                        "--branch",
                        "--show-stash",
                        "--untracked-files=normal",
                    ],
                )?,
                "Clean working tree.",
            ));
            text.push_str("\nFile columns: index / working tree; ?? means untracked.\nRemote comparisons use last-fetched refs; F Fetch updates them.");
        }
        GitCommand::Diff => {
            // Ignore external diff/text conversion tools and terminal color settings.
            // Show both sections even when one patch exceeds the display limit.
            for (heading, cached) in [("UNSTAGED CHANGES", false), ("STAGED CHANGES", true)] {
                let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
                if cached {
                    args.push("--cached");
                }
                text.push_str(heading);
                text.push('\n');
                args.push("--stat");
                text.push_str(&report_output(&runner.run(root, &args)?, "No changes.\n"));
                args.pop();
                args.push("--patch");
                text.push_str(&report_output(&runner.run(root, &args)?, ""));
                text.push('\n');
            }
            text.push_str("Untracked files are listed in Status; they are not included in Diff.");
        }
        GitCommand::Branches => {
            text.push_str(&report_output(
                &runner.run(root, &["branch", "--list", "-vv", "--no-color"])?,
                "No local branches yet.\n",
            ));
            text.push_str("\n* current branch; + checked out in another worktree.\nTracking information uses last-fetched refs; F Fetch updates them.");
        }
        GitCommand::LocalCommits => {
            if repo.head.is_none() {
                text.push_str("No commits yet.");
            } else if let Some(upstream) = &repo.upstream {
                text.push_str("Current-branch commits absent from its upstream (last-fetched refs).\nAt most 200 commits are shown. F Fetch updates the comparison.\n\n");
                text.push_str(&report_output(&runner.run(root, &[
                    "log", "--oneline", "--no-decorate", "--no-color", "--no-show-signature",
                    "-200", &format!("{}..HEAD", upstream.tracking_ref), "--",
                ]).map_err(|error| format!("Cannot compare with the upstream. Try F Fetch, then reopen Local commits.\n{error}"))?, "No local commits ahead of the upstream."));
            } else {
                text.push_str("No remote upstream is configured for the current branch, or HEAD is detached.\nCheck Branches for other local work; configure an upstream in the shell to compare commits.");
            }
        }
        GitCommand::Stashes => {
            text.push_str("Saved changes shared by this repository and its linked worktrees.\n\n");
            text.push_str(&report_output(
                &runner.run(root, &["stash", "list", "--no-color", "--format=%gd %s"])?,
                "No stashes.",
            ));
        }
        GitCommand::Fetch => {
            if runner.run(root, &["remote"])?.is_empty() {
                text.push_str("No remotes configured. Add a remote in the shell before fetching.");
            } else {
                runner.run(
                    root,
                    &[
                        "fetch",
                        "--all",
                        "--progress",
                        "--no-tags",
                        "--no-prune",
                        "--no-prune-tags",
                        "--no-recurse-submodules",
                    ],
                )?;
                text.push_str("Fetch complete. Remote comparisons have been refreshed.\n\n");
                text.push_str(&report_output(
                    &runner.run(root, &["status", "--short", "--branch"])?,
                    "Clean working tree.",
                ));
            }
        }
    }
    Ok(text)
}

fn fingerprint(root: &Path, runner: &Runner<'_>) -> Result<Fingerprint, String> {
    let status = runner.run(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    let refs = runner.run(root, &["for-each-ref", "--format=%(refname) %(objectname)"])?;
    let worktrees = runner.run(root, &["worktree", "list", "--porcelain", "-z"])?;
    let config = runner.run(root, &["config", "--local", "--null", "--list"])?;
    let head = runner.run(root, &["rev-parse", "HEAD"])?;
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        runner.check()?;
        if files.len() % 4096 == 0 {
            runner.progress(format!(
                "Checking repository files… {} entries",
                files.len()
            ));
        }
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("Cannot preview {}: {e}", path.display()))?;
        let entry_kind = kind(&meta);
        let nested = entry_kind == Kind::Directory
            && path != root
            && (path.join(".git").exists()
                || (path.join("HEAD").is_file()
                    && path.join("objects").is_dir()
                    && path.join("refs").is_dir()));
        files.push(PathState {
            path: path.clone(),
            identity: Identity::of(&meta),
            kind: entry_kind,
            len: meta.len(),
            modified: meta.modified().ok(),
            nested,
        });
        if entry_kind == Kind::Directory && !nested {
            let mut children = fs::read_dir(&path)
                .map_err(|e| e.to_string())?
                .filter_map(|entry| match entry {
                    Ok(entry) if entry.file_name() != OsStr::new(".git") => Some(Ok(entry.path())),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            children.sort();
            children.reverse();
            pending.extend(children);
        }
    }
    Ok(Fingerprint {
        status,
        refs,
        worktrees,
        config,
        head,
        files,
    })
}

fn run_clean(
    root: &Path,
    state: &Fingerprint,
    dry_run: bool,
    runner: &Runner<'_>,
) -> Result<Vec<u8>, String> {
    let mut args = vec![
        "clean".to_owned(),
        if dry_run { "-ndx" } else { "-fdx" }.to_owned(),
    ];
    for item in state.files.iter().filter(|item| item.nested) {
        let relative = item
            .path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_str()
            .ok_or("Nested repository path is not UTF-8; clean it from the shell")?;
        if relative.chars().any(char::is_control) {
            return Err(
                "Nested repository path contains control characters; clean it from the shell"
                    .into(),
            );
        }
        let mut pattern = String::from("/");
        for ch in relative.chars() {
            if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        pattern.push('/');
        args.extend(["-e".into(), pattern]);
    }
    runner.run(root, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn protect_nested(
    root: &Path,
    target: &str,
    state: &Fingerprint,
    runner: &Runner<'_>,
) -> Result<(), String> {
    let nested: Vec<_> = state.files.iter().filter(|item| item.nested).collect();
    if nested.is_empty() {
        return Ok(());
    }
    for reference in ["HEAD", target] {
        for entry in runner
            .run(root, &["ls-tree", "-rz", "--full-tree", reference])?
            .split(|&b| b == 0)
            .filter(|part| !part.is_empty())
        {
            let split = entry
                .iter()
                .position(|&b| b == b'\t')
                .ok_or("Cannot parse target tree")?;
            if entry[..split].starts_with(b"160000 ") {
                continue;
            }
            let mut raw = entry[split + 1..].to_vec();
            raw.push(b'\n');
            let path = root.join(path_from_output(raw));
            if let Some(item) = nested
                .iter()
                .find(|item| path.starts_with(&item.path) || item.path.starts_with(&path))
            {
                return Err(format!(
                    "Tracked paths overlap nested repository {}. Resolve this in the shell; it will not be reset recursively.",
                    display_name(item.path.as_os_str())
                ));
            }
        }
    }
    Ok(())
}

pub fn prepare(
    path: &Path,
    action: GitAction,
    options: GitOptions,
    cancel: &AtomicBool,
) -> Result<GitPreview, String> {
    prepare_with(
        path,
        action,
        options,
        &Runner {
            cancel,
            events: None,
            read_only: false,
        },
    )
}

fn prepare_with(
    path: &Path,
    action: GitAction,
    options: GitOptions,
    runner: &Runner<'_>,
) -> Result<GitPreview, String> {
    let mut repo = inspect_with(path, runner)?;
    let root = repo.root.clone();
    let head = repo
        .head
        .clone()
        .ok_or("This repository has no commits; use the shell first")?;
    if repo.branch.is_none() && action != GitAction::Clean {
        return Err("HEAD is detached; select a branch in the shell first".into());
    }
    let needs_remote = action != GitAction::Clean || options.discard_commits;
    let mut target = head.clone();
    let mut remote_expected = String::new();
    if needs_remote {
        let before_fetch = repo.clone();
        let upstream = repo
            .upstream
            .clone()
            .ok_or("No remote upstream is configured for this branch; configure it in the shell")?;
        let refspec = format!("+{}:{}", upstream.remote_ref, upstream.tracking_ref);
        runner.run(
            &root,
            &[
                "fetch",
                "--progress",
                "--no-tags",
                "--no-prune-tags",
                "--no-recurse-submodules",
                "--",
                &upstream.remote,
                &refspec,
            ],
        )?;
        repo = inspect_with(&root, runner)?;
        if repo != before_fetch {
            return Err("Branch or upstream changed while fetching; open a fresh preview".into());
        }
        if action == GitAction::Push {
            let remote = runner.text(
                &root,
                &[
                    "ls-remote",
                    "--refs",
                    "--",
                    &upstream.remote,
                    &upstream.remote_ref,
                ],
            )?;
            remote_expected = remote.split_whitespace().next().unwrap_or("").to_owned();
            if !remote_expected.is_empty()
                && !remote_expected.chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err("Invalid remote response".into());
            }
            let fetched = runner.text(&root, &["rev-parse", "--verify", &upstream.tracking_ref])?;
            if fetched != remote_expected {
                return Err("The remote changed while preparing the preview; try again".into());
            }
        } else {
            target = runner.text(
                &root,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", upstream.tracking_ref),
                ],
            )?;
        }
    }
    // Require deliberate resolution of interrupted merge/rebase/cherry-pick work.
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        let path = path_from_output(runner.run(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-path", marker],
        )?);
        if path.exists() {
            return Err(
                "A Git operation is already in progress; finish or abort it in the shell first"
                    .into(),
            );
        }
    }
    let original_fingerprint = fingerprint(&root, runner)?;
    if matches!(action, GitAction::Clean | GitAction::Reset) {
        protect_nested(&root, &target, &original_fingerprint, runner)?;
    }
    let mut description = format!(
        "Repository: {}\nBranch: {}\nHEAD: {}\n",
        display_name(root.as_os_str()),
        repo.branch.as_deref().unwrap_or("detached"),
        head
    );
    if let Some(upstream) = &repo.upstream {
        description.push_str(&format!(
            "Upstream: {} / {}\n",
            upstream.remote, upstream.remote_ref
        ));
    }
    let mut branches = Vec::new();
    match action {
        GitAction::Pull => {
            description.push_str(&format!(
                "\nFast-forward only to {target}. Divergence is left for the shell.\n"
            ));
        }
        GitAction::Push => {
            description.push_str(&format!("\nPush HEAD to the displayed upstream.\nForce with explicit lease: {}\nExpected remote commit: {}\n", options.force, if remote_expected.is_empty() { "(branch absent)" } else { &remote_expected }));
            if !remote_expected.is_empty() {
                description.push_str(&format!(
                    "\nLocal commits to send:\n{}\n",
                    visible(&runner.run(
                        &root,
                        &[
                            "log",
                            "--oneline",
                            "--no-decorate",
                            &format!("{remote_expected}..{head}")
                        ]
                    )?)
                ));
                if options.force {
                    description.push_str(&format!(
                        "\nRemote-only commits that will lose this branch reference:\n{}\n",
                        visible(&runner.run(
                            &root,
                            &[
                                "log",
                                "--oneline",
                                "--no-decorate",
                                &format!("{head}..{remote_expected}")
                            ]
                        )?)
                    ));
                }
            }
        }
        GitAction::Clean | GitAction::Reset => {
            description.push_str(&format!("\nRestore tracked files to {target}.\nRemove untracked AND ignored files (including build output).\nNested repositories, submodules, and other worktrees are not recursively cleaned.\n\nTracked changes:\n{}\n", visible(&runner.run(&root, &["status", "--short", "--untracked-files=no"])?)));
            description.push_str(&format!(
                "\nCleanup preview:\n{}\n",
                visible(&run_clean(&root, &original_fingerprint, true, runner)?)
            ));
            if target != head {
                description.push_str(&format!(
                    "\nCurrent-branch commits being discarded:\n{}\n",
                    visible(&runner.run(
                        &root,
                        &[
                            "log",
                            "--oneline",
                            "--no-decorate",
                            &format!("{target}..{head}")
                        ]
                    )?)
                ));
            }
            if action == GitAction::Reset && options.delete_branches {
                let occupied = runner.text(&root, &["worktree", "list", "--porcelain"])?;
                let occupied: HashSet<_> = occupied
                    .lines()
                    .filter_map(|line| line.strip_prefix("branch "))
                    .collect();
                let values = runner.text(
                    &root,
                    &[
                        "for-each-ref",
                        "--format=%(refname)%00%(objectname)",
                        "refs/heads",
                    ],
                )?;
                description.push_str("\nOther local branches:\n");
                for line in values.lines() {
                    let (reference, oid) =
                        line.split_once('\0').ok_or("Cannot parse branch list")?;
                    let name = reference
                        .strip_prefix("refs/heads/")
                        .ok_or("Invalid branch reference")?;
                    if Some(name) == repo.branch.as_deref() {
                        continue;
                    }
                    if occupied.contains(reference) {
                        description
                            .push_str(&format!("KEEP {name}: checked out in another worktree\n"));
                        continue;
                    }
                    description.push_str(&format!("DELETE {name} ({oid})\n"));
                    let commits = runner.run(
                        &root,
                        &[
                            "log",
                            "--oneline",
                            "--no-decorate",
                            reference,
                            "--not",
                            "--remotes",
                        ],
                    )?;
                    if !commits.is_empty() {
                        description.push_str(&format!(
                            "Commits not in locally fetched remote refs:\n{}\n",
                            visible(&commits)
                        ));
                    }
                    branches.push((name.to_owned(), oid.to_owned()));
                }
            }
            if action == GitAction::Reset && options.clear_stashes {
                description.push_str(&format!(
                    "\nDELETE repository-wide stashes:\n{}\n",
                    visible(&runner.run(&root, &["stash", "list", "--format=%gd %H %gs"])?)
                ));
            }
            description.push_str("\nThis resets in place. Git configuration, tags, reflogs, and retained objects stay; storage used by discarded history may remain.\n");
        }
    }
    let anchor = Directory::capture(&root).map_err(|e| e.to_string())?;
    let fingerprint = fingerprint(&root, runner)?;
    if fingerprint != original_fingerprint {
        return Err("Repository changed while preparing the preview; try again".into());
    }
    Ok(GitPreview {
        repository: repo,
        action,
        options,
        text: description,
        target,
        remote_expected,
        delete_branches: branches,
        fingerprint,
        anchor,
    })
}

impl GitPreview {
    pub fn execute(&self, cancel: &AtomicBool) -> Result<String, String> {
        execute_with(
            self,
            &Runner {
                cancel,
                events: None,
                read_only: false,
            },
        )
    }
}

fn execute_with(preview: &GitPreview, runner: &Runner<'_>) -> Result<String, String> {
    preview.anchor.validate().map_err(|e| e.to_string())?;
    let root = &preview.repository.root;
    if fingerprint(root, runner)? != preview.fingerprint {
        return Err("Repository changed since preview. Open the action again for a fresh preview; nothing was executed.".into());
    }
    let mut output = String::new();
    match preview.action {
        GitAction::Pull => {
            output.push_str(&visible(&runner.run(
                root,
                &[
                    "merge",
                    "--ff-only",
                    "--no-edit",
                    "--no-autostash",
                    &preview.target,
                ],
            )?));
        }
        GitAction::Push => {
            let upstream = preview
                .repository
                .upstream
                .as_ref()
                .ok_or("Missing upstream")?;
            let refspec = format!("HEAD:{}", upstream.remote_ref);
            let lease = format!(
                "--force-with-lease={}:{}",
                upstream.remote_ref, preview.remote_expected
            );
            let mut args = vec!["push", "--porcelain", "--progress"];
            if preview.options.force {
                args.push(&lease);
            }
            args.extend(["--", &upstream.remote, &refspec]);
            output.push_str(&visible(&runner.run(root, &args)?));
        }
        GitAction::Clean | GitAction::Reset => {
            output.push_str(&visible(
                &runner.run(root, &["reset", "--hard", &preview.target])?,
            ));
            output.push('\n');
            output.push_str(&visible(&run_clean(
                root,
                &preview.fingerprint,
                false,
                runner,
            )?));
            for (name, expected) in &preview.delete_branches {
                let actual = runner.text(
                    root,
                    &["rev-parse", "--verify", &format!("refs/heads/{name}")],
                )?;
                if &actual != expected {
                    return Err(format!(
                        "Branch {name} changed; stopped before deleting it. Earlier steps have completed."
                    ));
                }
                output.push('\n');
                output.push_str(&visible(&runner.run(root, &["branch", "-D", "--", name])?));
            }
            if preview.action == GitAction::Reset
                && preview.options.clear_stashes
                && let Some(line) = preview
                    .fingerprint
                    .refs
                    .split(|&b| b == b'\n')
                    .find(|line| line.starts_with(b"refs/stash "))
            {
                let expected = std::str::from_utf8(&line[b"refs/stash ".len()..])
                    .map_err(|e| e.to_string())?;
                runner.run(
                    root,
                    &["update-ref", "--no-deref", "-d", "refs/stash", expected],
                )?;
                output.push_str("\nPreviewed stashes cleared.");
            }
        }
    }
    Ok(format!("{} complete.\n\n{output}", preview.action.label()))
}
