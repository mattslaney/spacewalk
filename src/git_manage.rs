//! Selectable Git reports and previewed branch/stash operations.
use crate::{
    display_name,
    files::Directory,
    git_actions::{
        self, GitAction, GitCommand, GitOptions, GitPreview, Repository, Runner, Upstream, visible,
    },
    worktrees,
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn interrupted_branches(common: &Path, runner: &Runner<'_>) -> Result<HashSet<String>, String> {
    let mut directories = vec![common.to_path_buf()];
    match fs::read_dir(common.join("worktrees")) {
        Ok(entries) => {
            for entry in entries {
                directories.push(entry.map_err(|e| e.to_string())?.path());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect worktree operation state: {error}")),
    }
    let mut branches = HashSet::new();
    for directory in directories {
        runner.check()?;
        for marker in [
            "rebase-merge/head-name",
            "rebase-apply/head-name",
            "BISECT_START",
        ] {
            match fs::read_to_string(directory.join(marker)) {
                Ok(name) => {
                    branches.insert(
                        name.trim_end_matches('\n')
                            .strip_prefix("refs/heads/")
                            .unwrap_or(name.trim_end_matches('\n'))
                            .to_owned(),
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("Cannot inspect interrupted Git operation: {error}"));
                }
            }
        }
    }
    Ok(branches)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub oid: String,
    pub upstream: Option<Upstream>,
    pub tracking: String,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub unpushed: u64,
    pub checkout: Option<PathBuf>,
    pub busy: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub selector: String,
    pub oid: String,
    pub subject: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Branch(Branch),
    Stash(Stash),
    Commit { oid: String, subject: String },
}
impl Entry {
    pub fn identity(&self) -> String {
        match self {
            Self::Branch(b) => format!("branch:{}", b.name),
            Self::Stash(s) => format!("stash:{}", s.oid),
            Self::Commit { oid, .. } => format!("commit:{oid}"),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Branch(b) => format!(
                "{}  {} · {} unpushed{}",
                b.name,
                if b.tracking.is_empty() {
                    "local-only"
                } else {
                    &b.tracking
                },
                b.unpushed,
                if b.checkout.is_some() {
                    " · checked out"
                } else if b.busy {
                    " · operation in progress"
                } else {
                    ""
                }
            ),
            Self::Stash(s) => format!("{}  {}", s.selector, s.subject),
            Self::Commit { oid, subject } => format!("{}  {subject}", &oid[..oid.len().min(10)]),
        }
    }
    pub fn detail(&self) -> String {
        match self {
            Self::Branch(b) => format!(
                "↑{} ahead · ↓{} behind · {}",
                b.ahead.map_or("?".into(), |n| n.to_string()),
                b.behind.map_or("?".into(), |n| n.to_string()),
                b.checkout.as_ref().map_or_else(
                    || if b.busy {
                        "in-progress Git operation".into()
                    } else {
                        "not checked out".into()
                    },
                    |p| display_name(p.as_os_str())
                )
            ),
            Self::Stash(s) => format!("{} · repository-wide saved changes", s.oid),
            Self::Commit { oid, .. } => format!("{oid} · absent from all fetched remote refs"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Report {
    pub repository: Repository,
    pub text: String,
    pub entries: Vec<Entry>,
    pub summary: Option<Result<ReclaimSummary, String>>,
}

#[derive(Clone, Debug)]
pub struct ReclaimSummary {
    pub files: Option<worktrees::LocalState>,
    pub ignored_items: usize,
    pub commits_not_on_remote: u64,
    pub branches_without_upstream: usize,
    pub stashes: usize,
    pub has_remote_refs: bool,
}

#[derive(Clone, Debug)]
pub struct Operations {
    pub repository: Repository,
    pub summary: Result<ReclaimSummary, String>,
}

/// Read only Git metadata/path classifications. Never scan checkout sizes or fetch.
pub(crate) fn reclaim_summary(
    repo: &Repository,
    runner: &Runner<'_>,
) -> Result<ReclaimSummary, String> {
    let local = runner.local();
    let runner = &local;
    let root = &repo.root;
    let files = if repo.bare {
        None
    } else {
        Some(worktrees::local_state(root, runner)?)
    };
    let ignored_items = if repo.bare {
        0
    } else {
        runner
            .run(
                root,
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--directory",
                    "-z",
                ],
            )?
            .split(|&byte| byte == 0)
            .filter(|path| !path.is_empty())
            .count()
    };
    let refs = runner.text(root, &["for-each-ref", "--format=%(refname)"])?;
    let refs: HashSet<_> = refs.lines().collect();
    let has_remote_refs = refs.iter().any(|name| name.starts_with("refs/remotes/"));
    let branches = runner.text(
        root,
        &[
            "for-each-ref",
            "--format=%(upstream)%00%(upstream:remotename)",
            "refs/heads/",
        ],
    )?;
    let mut branches_without_upstream = 0;
    for branch in branches.lines() {
        let (upstream, remote) = branch
            .split_once('\0')
            .ok_or("Cannot inspect branch upstreams")?;
        if remote.is_empty()
            || remote == "."
            || !upstream.starts_with("refs/remotes/")
            || !refs.contains(upstream)
        {
            branches_without_upstream += 1;
        }
    }
    let mut args = vec!["rev-list", "--count", "--branches"];
    if repo.head.is_some() {
        args.push("HEAD");
    }
    args.extend(["--not", "--remotes", "--"]);
    let commits_not_on_remote = runner
        .text(root, &args)?
        .parse()
        .map_err(|_| "Cannot count local commits")?;
    let stashes = if repo.bare {
        if refs.contains("refs/stash") {
            runner
                .text(root, &["reflog", "show", "--format=%H", "refs/stash"])?
                .lines()
                .count()
        } else {
            0
        }
    } else {
        stashes(root, runner)?.len()
    };
    Ok(ReclaimSummary {
        files,
        ignored_items,
        commits_not_on_remote,
        branches_without_upstream,
        stashes,
        has_remote_refs,
    })
}

pub(crate) fn operations(repository: Repository, runner: &Runner<'_>) -> Operations {
    let summary = reclaim_summary(&repository, runner);
    Operations {
        repository,
        summary,
    }
}

fn counts(root: &Path, left: &str, right: &str, runner: &Runner<'_>) -> Result<(u64, u64), String> {
    let raw = runner.text(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{left}...{right}"),
            "--",
        ],
    )?;
    let mut numbers = raw.split_whitespace().map(str::parse::<u64>);
    Ok((
        numbers
            .next()
            .ok_or("Missing ahead count")?
            .map_err(|e| e.to_string())?,
        numbers
            .next()
            .ok_or("Missing behind count")?
            .map_err(|e| e.to_string())?,
    ))
}
fn branches(repo: &Repository, runner: &Runner<'_>) -> Result<Vec<Branch>, String> {
    let raw = runner.text(&repo.root, &["for-each-ref", "--sort=refname", "--format=%(refname:strip=2)%00%(objectname)%00%(upstream)%00%(upstream:short)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(if)%(worktreepath)%(then)yes%(else)no%(end)", "refs/heads/"])?;
    let registry = worktrees::discover(&repo.root, runner.cancel_flag())?;
    let interrupted = interrupted_branches(&repo.common_dir, runner)?;
    let mut rows = Vec::new();
    for line in raw.lines().filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split('\0').collect();
        if fields.len() != 7 {
            return Err("Cannot parse local branches".into());
        }
        let upstream = (!fields[4].is_empty()
            && fields[4] != "."
            && fields[2].starts_with("refs/remotes/")
            && fields[5].starts_with("refs/heads/"))
        .then(|| Upstream {
            remote: fields[4].into(),
            remote_ref: fields[5].into(),
            tracking_ref: fields[2].into(),
        });
        let comparison = if fields[2].is_empty() {
            None
        } else {
            counts(&repo.root, fields[1], fields[2], runner).ok()
        };
        let unpushed = runner
            .text(
                &repo.root,
                &["rev-list", "--count", fields[1], "--not", "--remotes", "--"],
            )?
            .parse()
            .map_err(|_| "Cannot count unpushed commits")?;
        rows.push(Branch {
            name: fields[0].into(),
            oid: fields[1].into(),
            upstream,
            tracking: fields[3].into(),
            ahead: comparison.map(|c| c.0),
            behind: comparison.map(|c| c.1),
            unpushed,
            busy: fields[6] == "yes" || interrupted.contains(fields[0]),
            checkout: if !repo.bare && repo.branch.as_deref() == Some(fields[0]) {
                Some(repo.root.clone())
            } else {
                registry
                    .records
                    .iter()
                    .find(|r| !r.bare && r.branch.as_deref() == Some(fields[0]))
                    .map(|r| r.path.clone())
            },
        });
    }
    Ok(rows)
}
fn stashes(root: &Path, runner: &Runner<'_>) -> Result<Vec<Stash>, String> {
    let raw = runner.run(
        root,
        &[
            "stash",
            "list",
            "--no-color",
            "-z",
            "--format=%gd%x00%H%x00%gs",
        ],
    )?;
    let mut fields = raw.split(|&b| b == 0).peekable();
    let mut rows = Vec::new();
    while let Some(selector) = fields.next() {
        if selector.is_empty() && fields.peek().is_none() {
            break;
        }
        let oid = fields.next().ok_or("Cannot parse stash object")?;
        let subject = fields.next().ok_or("Cannot parse stash subject")?;
        rows.push(Stash {
            selector: String::from_utf8(selector.to_vec()).map_err(|_| "Invalid stash selector")?,
            oid: String::from_utf8(oid.to_vec()).map_err(|_| "Invalid stash object")?,
            subject: visible(subject),
        });
    }
    Ok(rows)
}
pub(crate) fn list(
    repo: Repository,
    command: GitCommand,
    runner: &Runner<'_>,
) -> Result<Report, String> {
    let mut entries = Vec::new();
    let note = match command {
        GitCommand::Branches => {
            entries = branches(&repo, runner)?
                .into_iter()
                .filter(|b| {
                    b.upstream.is_none()
                        || b.ahead.is_none()
                        || b.ahead.is_some_and(|n| n > 0)
                        || b.unpushed > 0
                })
                .map(Entry::Branch)
                .collect();
            "Local-only branches and branches with unpushed commits (last-fetched refs)."
        }
        GitCommand::Stashes => {
            entries = stashes(&repo.root, runner)?
                .into_iter()
                .map(Entry::Stash)
                .collect();
            "Stashes are shared by this repository and its linked worktrees."
        }
        GitCommand::LocalCommits => {
            if repo.head.is_some() {
                let raw = runner.run(
                    &repo.root,
                    &[
                        "log",
                        "-z",
                        "--format=%H%x00%s",
                        "--no-color",
                        "--no-show-signature",
                        "-200",
                        "HEAD",
                        "--not",
                        "--remotes",
                        "--",
                    ],
                )?;
                let mut fields = raw.split(|&b| b == 0);
                while let Some(oid) = fields.next() {
                    if oid.is_empty() {
                        break;
                    }
                    let subject = fields.next().ok_or("Cannot parse commit subject")?;
                    entries.push(Entry::Commit {
                        oid: String::from_utf8(oid.to_vec())
                            .map_err(|_| "Invalid commit object")?,
                        subject: visible(subject),
                    });
                }
            }
            if repo.head.is_none() {
                "No commits yet."
            } else {
                "Commits on HEAD absent from all fetched remote refs (up to 200). No automatic fetch. Push publishes all pending commits on this branch."
            }
        }
        _ => return Err("This command is not a selectable list".into()),
    };
    let mut text = format!(
        "Repository: {}\nBranch: {}\n\n{note}\n",
        display_name(repo.root.as_os_str()),
        repo.branch.as_deref().unwrap_or("detached HEAD")
    );
    if entries.is_empty() {
        text.push_str(match command {
            GitCommand::Branches => "No local-only branches or unpushed commits.\n",
            GitCommand::Stashes => "No stashes.\n",
            _ => "No unpushed commits.\n",
        });
    }
    for entry in &entries {
        text.push_str(&format!("{}\n{}\n", entry.label(), entry.detail()));
    }
    Ok(Report {
        repository: repo,
        text,
        entries,
        summary: None,
    })
}
pub(crate) fn load(
    path: &Path,
    command: GitCommand,
    runner: &Runner<'_>,
) -> Result<Report, String> {
    let local = runner.local();
    let runner = &local;
    let repository = git_actions::inspect_root(path, runner)?;
    if command == GitCommand::Status && !repository.bare {
        let summary = reclaim_summary(&repository, runner);
        let text = if let Ok(summary) = &summary {
            git_actions::status_text(
                &repository,
                summary
                    .files
                    .as_ref()
                    .ok_or("Checkout status is unavailable")?,
            )
        } else {
            git_actions::report_with(path, command, runner)?
        };
        return Ok(Report {
            repository,
            text,
            entries: Vec::new(),
            summary: Some(summary),
        });
    }
    if matches!(
        command,
        GitCommand::Branches | GitCommand::Stashes | GitCommand::LocalCommits
    ) {
        let mut report = list(repository, command, runner)?;
        report.summary = Some(reclaim_summary(&report.repository, runner));
        return Ok(report);
    }
    let text = git_actions::report_with(path, command, runner)?;
    Ok(Report {
        summary: Some(reclaim_summary(&repository, runner)),
        repository,
        text,
        entries: Vec::new(),
    })
}

#[derive(Clone, Debug)]
pub enum Action {
    Sync(Branch),
    Push(Branch),
    PushHead(String),
    DeleteBranch(Branch),
    DropStash(Stash),
}
impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Sync(_) => "Sync branch",
            Self::Push(_) | Self::PushHead(_) => "Push branch",
            Self::DeleteBranch(_) => "Delete local branch",
            Self::DropStash(_) => "Delete stash",
        }
    }
}
#[derive(Clone, Debug)]
struct PushTarget {
    remote: String,
    reference: String,
    expected: String,
    target: String,
    pull: Option<Box<GitPreview>>,
    publish: bool,
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub repository: Repository,
    pub action: Action,
    pub text: String,
    anchor: Directory,
    metadata: Vec<Vec<u8>>,
    target: Option<PushTarget>,
}
fn metadata(root: &Path, runner: &Runner<'_>) -> Result<Vec<Vec<u8>>, String> {
    Ok(vec![
        runner.run(root, &["for-each-ref", "--format=%(refname) %(objectname)"])?,
        runner.run(root, &["config", "--local", "--null", "--list"])?,
        runner.run(root, &["worktree", "list", "--porcelain", "-z"])?,
        runner.run(
            root,
            &[
                "stash",
                "list",
                "--no-color",
                "-z",
                "--format=%gd%x00%H%x00%gs",
            ],
        )?,
        runner
            .run(root, &["symbolic-ref", "--quiet", "HEAD"])
            .unwrap_or_default(),
    ])
}
fn remote_oid(
    root: &Path,
    remote: &str,
    reference: &str,
    runner: &Runner<'_>,
) -> Result<String, String> {
    let raw = runner.text(root, &["ls-remote", "--refs", "--", remote, reference])?;
    Ok(raw
        .lines()
        .find_map(|line| {
            line.split_once('\t')
                .filter(|(_, name)| *name == reference)
                .map(|(oid, _)| oid.to_owned())
        })
        .unwrap_or_default())
}
fn branch_now(repo: &Repository, selected: &Branch, runner: &Runner<'_>) -> Result<Branch, String> {
    let current = branches(repo, runner)?
        .into_iter()
        .find(|b| b.name == selected.name)
        .ok_or("Selected branch no longer exists; refresh the list")?;
    if current.oid != selected.oid
        || current.upstream != selected.upstream
        || current.checkout != selected.checkout
        || current.busy != selected.busy
    {
        return Err("Selected branch changed; refresh the list before preparing an action".into());
    }
    Ok(current)
}
pub(crate) fn prepare_with(
    path: &Path,
    action: Action,
    runner: &Runner<'_>,
) -> Result<Preview, String> {
    let local = runner.local();
    let runner = &local;
    let repo = git_actions::inspect_root(path, runner)?;
    if let Action::PushHead(expected) = &action {
        let branch = current_branch(&repo, runner)?;
        if branch.oid != *expected {
            return Err("Branch changed since the commit list was loaded; refresh it first".into());
        }
        return prepare_with(path, Action::Push(branch), runner);
    }
    let anchor = Directory::capture(path).map_err(|e| e.to_string())?;
    let mut text = format!(
        "{}\nRepository: {}\n",
        action.label(),
        display_name(path.as_os_str())
    );
    let mut target = None;
    match &action {
        Action::PushHead(_) => unreachable!(),
        Action::DropStash(stash) => {
            if !stashes(path, runner)?.contains(stash) {
                return Err("Selected stash changed; refresh the list".into());
            }
            text.push_str(&format!("\nDELETE {}\n{}\nObject: {}\n\nThis deletes this one saved stash for the entire repository, including linked worktrees. Working files are unchanged.\n", stash.selector, stash.subject, stash.oid));
        }
        Action::DeleteBranch(branch) => {
            let branch = branch_now(&repo, branch, runner)?;
            if branch.checkout.is_some()
                || branch.busy
                || repo.branch.as_deref() == Some(&branch.name)
            {
                return Err("This branch is checked out. Remove its worktree or switch branches in the shell before deleting it.".into());
            }
            text.push_str(&format!("\nDELETE local branch: {}\nHEAD: {}\n{} commits absent from fetched remote refs.\n\nUnpushed work may lose its branch reference. The remote branch and working files are unchanged.\n", branch.name, branch.oid, branch.unpushed));
            text.push_str(&visible(&runner.run(
                path,
                &[
                    "log",
                    "--oneline",
                    "--no-color",
                    "--no-show-signature",
                    "-50",
                    &branch.oid,
                    "--not",
                    "--remotes",
                    "--",
                ],
            )?));
        }
        Action::Sync(selected) | Action::Push(selected) => {
            let branch = branch_now(&repo, selected, runner)?;
            if branch.busy && branch.checkout.is_none() {
                return Err("This branch is in use by an interrupted operation. Finish it in the shell before syncing.".into());
            }
            let publish = branch.upstream.is_none();
            let (remote, reference, tracking) = if let Some(u) = &branch.upstream {
                (
                    u.remote.clone(),
                    u.remote_ref.clone(),
                    u.tracking_ref.clone(),
                )
            } else {
                let remotes = runner.text(path, &["remote"])?;
                let names: Vec<_> = remotes.lines().collect();
                let remote = if names.contains(&"origin") {
                    "origin"
                } else if let [name] = names.as_slice() {
                    name
                } else {
                    return Err("Choose a remote upstream in the shell first: there is no unambiguous publishing remote.".into());
                };
                (
                    remote.into(),
                    format!("refs/heads/{}", branch.name),
                    format!("refs/remotes/{remote}/{}", branch.name),
                )
            };
            let expected = remote_oid(path, &remote, &reference, runner)?;
            let mut next = branch.oid.clone();
            let mut pull = None;
            if !expected.is_empty() {
                runner.run(
                    path,
                    &[
                        "fetch",
                        "--progress",
                        "--no-tags",
                        "--no-prune",
                        "--no-prune-tags",
                        "--no-recurse-submodules",
                        "--",
                        &remote,
                        &format!("+{reference}:{tracking}"),
                    ],
                )?;
                if runner.text(path, &["rev-parse", "--verify", &tracking])? != expected {
                    return Err("Remote changed during preparation; preview again".into());
                }
                let (ahead, behind) = counts(path, &branch.oid, &expected, runner)?;
                if behind > 0 {
                    if matches!(action, Action::Push(_)) {
                        return Err("The branch is behind its remote. Use Sync for a fast-forward, or resolve divergence in the shell.".into());
                    }
                    if ahead > 0 {
                        return Err("The branch and remote have diverged. Resolve this in the shell; Sync never rebases or force-pushes.".into());
                    }
                    next = expected.clone();
                    if let Some(checkout) = &branch.checkout {
                        let state = worktrees::local_state(checkout, runner)?;
                        if state.dirty() {
                            return Err("The branch's checkout is dirty. Commit or stash changes in the shell before syncing.".into());
                        }
                        if publish {
                            return Err("An unpublished checked-out branch is behind an existing remote branch. Configure its upstream in the shell before syncing.".into());
                        }
                        pull = Some(Box::new(git_actions::prepare_with(
                            checkout,
                            GitAction::Pull,
                            GitOptions::default(),
                            runner,
                        )?));
                        if pull.as_ref().unwrap().target != expected {
                            return Err(
                                "Remote changed while preparing fast-forward; preview again".into(),
                            );
                        }
                    }
                }
            }
            branch_now(&repo, &branch, runner)?;
            text.push_str(&format!("\nBranch: {}\nLocal HEAD: {}\nRemote: {}\nDestination: {}\nRemote HEAD: {}\nFinal local / pushed commit: {}\n\n{}\nNormal push only; no force, rebase, branch switch or automatic stash.\n", branch.name, branch.oid, remote, reference, if expected.is_empty() { "(new remote branch)" } else { &expected }, next, if publish { "Publish this branch and configure its upstream after a successful push." } else { "Fast-forward if needed, then push the branch's pending commits." }));
            if let Some(pull) = &pull {
                text.push_str(&pull.text);
            }
            target = Some(PushTarget {
                remote,
                reference,
                expected,
                target: next,
                pull,
                publish,
            });
        }
    }
    let snapshot = metadata(path, runner)?;
    Ok(Preview {
        repository: repo,
        action,
        text,
        anchor,
        metadata: snapshot,
        target,
    })
}
pub(crate) fn execute_with(preview: &Preview, runner: &Runner<'_>) -> Result<String, String> {
    let local = runner.local();
    let runner = &local;
    preview.anchor.validate().map_err(|e| e.to_string())?;
    let repo = git_actions::inspect_root(&preview.repository.root, runner)?;
    let root = &repo.root;
    if repo != preview.repository || metadata(root, runner)? != preview.metadata {
        return Err(
            "Repository changed since preview; nothing was executed. Refresh and preview again."
                .into(),
        );
    }
    match &preview.action {
        Action::PushHead(_) => unreachable!(),
        Action::DropStash(stash) => {
            if !stashes(root, runner)?.contains(stash) {
                return Err("Stash list changed since preview".into());
            }
            runner.run(root, &["stash", "drop", "--", &stash.selector])?;
        }
        Action::DeleteBranch(branch) => {
            let current = branch_now(&repo, branch, runner)?;
            if current.checkout.is_some()
                || current.busy
                || repo.branch.as_deref() == Some(&branch.name)
            {
                return Err("Branch is now checked out; it was not deleted".into());
            }
            runner.run(
                root,
                &[
                    "update-ref",
                    "-d",
                    &format!("refs/heads/{}", branch.name),
                    &branch.oid,
                ],
            )?;
            let prefix = format!("branch.{}.", branch.name);
            let configured = preview.metadata[1].split(|&b| b == 0).any(|entry| {
                entry
                    .split(|&b| b == b'\n')
                    .next()
                    .and_then(|key| key.strip_prefix(prefix.as_bytes()))
                    .is_some_and(|key| !key.contains(&b'.'))
            });
            if configured {
                runner.run(
                    root,
                    &[
                        "config",
                        "--local",
                        "--remove-section",
                        &format!("branch.{}", branch.name),
                    ],
                )?;
            }
        }
        Action::Sync(branch) | Action::Push(branch) => {
            let target = preview.target.as_ref().ok_or("Missing push target")?;
            branch_now(&repo, branch, runner)?;
            if remote_oid(root, &target.remote, &target.reference, runner)? != target.expected {
                return Err(
                    "Remote changed since preview; nothing was executed. Preview again.".into(),
                );
            }
            if let Some(pull) = &target.pull {
                git_actions::execute_with(pull, runner)?;
            } else if target.target != branch.oid {
                if branches(&repo, runner)?
                    .iter()
                    .any(|b| b.name == branch.name && (b.checkout.is_some() || b.busy))
                {
                    return Err("Branch is now checked out; fast-forward was not applied".into());
                }
                runner.run(
                    root,
                    &[
                        "update-ref",
                        &format!("refs/heads/{}", branch.name),
                        &target.target,
                        &branch.oid,
                    ],
                )?;
            }
            runner.run(root, &["push", "--porcelain", "--progress", "--no-follow-tags", "--recurse-submodules=no", "--", &target.remote, &format!("{}:{}", target.target, target.reference)])
                .map_err(|e| format!("{e}\nA fast-forward may have completed; local work is kept. Refresh before retrying."))?;
            if target.publish {
                if runner.text(
                    root,
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("refs/heads/{}", branch.name),
                    ],
                )? != target.target
                {
                    return Err(
                        "Push succeeded, but branch changed; configure its upstream in the shell."
                            .into(),
                    );
                }
                runner.run(
                    root,
                    &[
                        "config",
                        "--local",
                        &format!("branch.{}.remote", branch.name),
                        &target.remote,
                    ],
                )?;
                runner.run(
                    root,
                    &[
                        "config",
                        "--local",
                        &format!("branch.{}.merge", branch.name),
                        &target.reference,
                    ],
                )?;
            }
        }
    }
    Ok(format!(
        "{} complete. Refresh the list to see the current state.",
        preview.action.label()
    ))
}
pub fn read(path: &Path, command: GitCommand, cancel: &AtomicBool) -> Result<Report, String> {
    load(path, command, &Runner::readonly(cancel))
}
pub fn prepare(path: &Path, action: Action, cancel: &AtomicBool) -> Result<Preview, String> {
    prepare_with(path, action, &Runner::readonly(cancel))
}
impl Preview {
    pub fn execute(&self, cancel: &AtomicBool) -> Result<String, String> {
        execute_with(self, &Runner::readonly(cancel))
    }
}
pub(crate) fn current_branch(repo: &Repository, runner: &Runner<'_>) -> Result<Branch, String> {
    branches(repo, runner)?
        .into_iter()
        .find(|b| Some(&b.name) == repo.branch.as_ref())
        .ok_or_else(|| {
            "Push needs an attached local branch. Use the shell for detached commits.".into()
        })
}
