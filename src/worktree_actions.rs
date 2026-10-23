//! Git-native, previewed worktree removal and stale-registration pruning.
use crate::{
    display_name,
    files::Directory,
    git_actions::{
        Fingerprint, Runner, fingerprint, fingerprint_removal, inspect_checkout, visible,
    },
    scan::{self, Options},
    worktrees::{self, Registry, WorktreeRecord},
};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Debug)]
pub enum WorktreeRequest {
    Remove {
        common_dir: PathBuf,
        paths: Vec<PathBuf>,
        force: bool,
        remove_nested: bool,
        protected: Vec<PathBuf>,
        options: Options,
    },
    Prune {
        common_dir: PathBuf,
    },
}
impl WorktreeRequest {
    pub fn context(&self) -> &Path {
        match self {
            Self::Remove { common_dir, .. } | Self::Prune { common_dir } => common_dir,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Remove { .. } => "Remove worktrees",
            Self::Prune { .. } => "Prune stale registrations",
        }
    }
}

#[derive(Clone, Debug)]
struct RemovalTarget {
    record: WorktreeRecord,
    anchor: Directory,
    state: Fingerprint,
    nested: Option<Vec<NestedRepository>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NestedRepository {
    path: PathBuf,
    common_dir: PathBuf,
    registrations: Vec<WorktreeRecord>,
    refs: Vec<u8>,
    head: Vec<u8>,
    config: Vec<u8>,
    status: Vec<u8>,
}

fn nested_repositories(
    root: &Path,
    state: &Fingerprint,
    runner: &Runner<'_>,
) -> Result<Vec<NestedRepository>, String> {
    let mut result = Vec::new();
    for path in state.nested_repositories() {
        runner.check()?;
        let registry = worktrees::discover(&path, runner.cancel_flag())?;
        let current = registry.records.iter().find(|record| record.path == path);
        let current = if let Some(current) = current {
            current
        } else {
            // For an absorbed submodule, Git may list the administrative Git
            // directory as its primary worktree. Verify the actual checkout and
            // enclosing superproject before accepting that special layout.
            inspect_checkout(&path, runner)?;
            let mut superproject =
                runner.run(&path, &["rev-parse", "--show-superproject-working-tree"])?;
            if superproject.last() == Some(&b'\n') {
                superproject.pop();
            }
            let superproject = worktrees::os_path(&superproject);
            if !superproject.is_absolute()
                || !superproject.starts_with(root)
                || superproject == path
            {
                return Err(
                    "Nested repository root could not be verified; repair it in the shell".into(),
                );
            }
            registry
                .records
                .first()
                .filter(|record| record.main && !record.bare && record.path == registry.common_dir)
                .ok_or("Submodule registration could not be verified; repair it in the shell")?
        };
        if !current.main || current.locked.is_some() {
            return Err(format!(
                "{} is a nested linked or locked worktree. Remove it separately in its own worktree manager first.",
                display_name(path.as_os_str())
            ));
        }
        if registry
            .records
            .iter()
            .any(|record| record != current && !record.path.starts_with(root))
        {
            return Err(format!(
                "{} has registered worktrees outside the selected checkout. Remove those separately before deleting this nested repository.",
                display_name(path.as_os_str())
            ));
        }
        let status = if current.bare {
            Vec::new()
        } else {
            runner.run(
                &path,
                &[
                    "status",
                    "--short",
                    "--untracked-files=all",
                    "--ignored=matching",
                ],
            )?
        };
        result.push(NestedRepository {
            refs: runner.run(
                &path,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            )?,
            head: runner
                .run(&path, &["rev-parse", "--verify", "--quiet", "HEAD"])
                .unwrap_or_default(),
            config: runner.run(&path, &["config", "--local", "--null", "--list"])?,
            path,
            common_dir: registry.common_dir,
            registrations: registry.records,
            status,
        });
    }
    Ok(result)
}

#[derive(Clone, Debug)]
enum PreviewKind {
    Remove {
        targets: Vec<RemovalTarget>,
        force: bool,
        protected: Vec<PathBuf>,
    },
    Prune {
        expiry: String,
        dry_run: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub struct WorktreePreview {
    pub title: String,
    pub text: String,
    pub common_dir: PathBuf,
    registry: Registry,
    anchor: Directory,
    kind: PreviewKind,
}

fn safe_target(
    record: &WorktreeRecord,
    registry: &Registry,
    protected: &[PathBuf],
) -> Result<Directory, String> {
    if record.main || record.bare {
        return Err("The main worktree and bare repository hub cannot be removed".into());
    }
    if let Some(reason) = &record.locked {
        return Err(format!(
            "{} is locked: {}. Unlock it deliberately in the shell first.",
            display_name(record.path.as_os_str()),
            visible(reason.as_bytes())
        ));
    }
    let meta = fs::symlink_metadata(&record.path).map_err(|e| {
        format!(
            "{} is unavailable: {e}. Use pruning for stale registrations.",
            display_name(record.path.as_os_str())
        )
    })?;
    if !meta.is_dir() || meta.is_symlink() {
        return Err("Worktree root must be a real directory, not a symbolic link".into());
    }
    let anchor = Directory::capture(&record.path).map_err(|e| e.to_string())?;
    for path in protected {
        let path = path.canonicalize().map_err(|e| {
            format!(
                "Cannot verify protected path {}: {e}",
                display_name(path.as_os_str())
            )
        })?;
        if path.starts_with(&anchor.path) {
            return Err(format!(
                "{} contains Spacewalk's scan root or working directory and cannot be removed",
                display_name(record.path.as_os_str())
            ));
        }
    }
    if registry.common_dir.starts_with(&anchor.path) {
        return Err("This checkout contains the shared Git store and cannot be removed".into());
    }
    if registry
        .records
        .iter()
        .any(|other| other.path != record.path && other.path.starts_with(&record.path))
    {
        return Err(
            "This worktree contains another registered worktree; remove it separately first".into(),
        );
    }
    Ok(anchor)
}

fn detached_commits(
    record: &WorktreeRecord,
    common: &Path,
    runner: &Runner<'_>,
) -> Result<Vec<u8>, String> {
    if record.branch.is_some() {
        return Ok(Vec::new());
    }
    let head = record
        .head
        .as_deref()
        .ok_or("Detached worktree has no verifiable HEAD")?;
    runner.run(
        common,
        &[
            "log",
            "--oneline",
            "--no-decorate",
            head,
            "--not",
            "--branches",
            "--tags",
            "--remotes",
        ],
    )
}

fn expiry(common: &Path, runner: &Runner<'_>) -> Result<String, String> {
    runner.text(
        common,
        &[
            "config",
            "--default",
            "3.months.ago",
            "--get",
            "gc.worktreePruneExpire",
        ],
    )
}

fn prune_command(
    common: &Path,
    expiry: &str,
    dry_run: bool,
    runner: &Runner<'_>,
) -> Result<Vec<u8>, String> {
    let expiry = format!("--expire={expiry}");
    let mut args = vec!["worktree", "prune", "--verbose", &expiry];
    if dry_run {
        args.push("--dry-run");
    }
    runner.run(common, &args)
}

pub fn prepare(request: WorktreeRequest, cancel: &AtomicBool) -> Result<WorktreePreview, String> {
    prepare_with(request, &Runner::readonly(cancel))
}

pub(crate) fn prepare_with(
    request: WorktreeRequest,
    runner: &Runner<'_>,
) -> Result<WorktreePreview, String> {
    let runner = runner.local();
    let registry = worktrees::discover(request.context(), runner.cancel_flag())?;
    let common = registry.common_dir.clone();
    let anchor = Directory::capture(&common).map_err(|e| e.to_string())?;
    let title = request.label().to_owned();
    let (text, kind) = match request {
        WorktreeRequest::Remove {
            paths,
            force,
            remove_nested,
            protected,
            options,
            ..
        } => {
            if paths.is_empty() {
                return Err("No worktrees selected".into());
            }
            if remove_nested && !force {
                return Err("Removing nested repositories/submodules requires both Force and Remove nested repositories. Both options start off.".into());
            }
            let mut unique = HashSet::new();
            let mut targets = Vec::new();
            let mut reported_total = 0_u64;
            let mut text = format!(
                "Remove {} selected worktrees?\nForce: {}\nRemove nested repositories/submodules: {}\n\nAll checkout files, including ignored dependencies and configuration, will be permanently removed. Parent repository branches and shared history are kept.\n",
                paths.len(),
                if force {
                    "ON: local changes and detached-only commits may be discarded"
                } else {
                    "off"
                },
                if remove_nested { "ON" } else { "off" }
            );
            for path in paths {
                runner.check()?;
                if !unique.insert(path.clone()) {
                    continue;
                }
                let record = registry
                    .records
                    .iter()
                    .find(|record| record.path == path)
                    .ok_or("Worktree registration changed; refresh the list")?
                    .clone();
                let target_anchor = safe_target(&record, &registry, &protected)?;
                inspect_checkout(&record.path, &runner)?;
                if worktrees::discover(&record.path, runner.cancel_flag())?.common_dir != common {
                    return Err("Checkout now belongs to a different repository".into());
                }
                let state = if remove_nested {
                    fingerprint_removal(&record.path, &runner)?
                } else {
                    fingerprint(&record.path, &runner)?
                };
                let nested = state.nested_repositories();
                if !nested.is_empty() && !remove_nested {
                    return Err(format!(
                        "{} contains a nested repository or submodule:\n{}\nEnable Force AND Remove nested repositories explicitly to preview their deletion. Nested linked/locked worktrees and repositories supporting external worktrees remain protected.",
                        display_name(record.path.as_os_str()),
                        nested
                            .iter()
                            .map(|path| display_name(path.as_os_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
                let nested = remove_nested
                    .then(|| nested_repositories(&record.path, &state, &runner))
                    .transpose()?;
                let local = worktrees::local_state(&record.path, &runner)?;
                if local.dirty() && !force {
                    return Err(format!(
                        "{} has local changes or untracked files. Enable Force explicitly to preview removal.",
                        display_name(record.path.as_os_str())
                    ));
                }
                let detached = detached_commits(&record, &common, &runner)?;
                if !detached.is_empty() && !force {
                    return Err(format!(
                        "{} has detached-only commits. Preserve them on a branch in the shell, or explicitly enable Force to discard their worktree reference.\n{}",
                        display_name(record.path.as_os_str()),
                        visible(&detached)
                    ));
                }
                let snapshot = scan::scan_excluding(
                    &record.path,
                    options,
                    runner.cancel_flag(),
                    &[record.path.join(".git")],
                    |_| {},
                )
                .map_err(|e| e.to_string())?;
                if snapshot.errors > 0 {
                    return Err("Checkout could not be fully measured; resolve scan errors before removing it".into());
                }
                reported_total = reported_total.saturating_add(snapshot.nodes[0].size);
                text.push_str(&format!(
                    "\n{}\nBranch: {} · HEAD: {}\nReported checkout usage: {}\n",
                    display_name(record.path.as_os_str()),
                    record.branch.as_deref().unwrap_or("detached"),
                    record.head.as_deref().unwrap_or("unknown"),
                    crate::human_size(snapshot.nodes[0].size)
                ));
                if let Some(nested) = &nested {
                    for repository in nested {
                        text.push_str(&format!(
                            "\nNESTED REPOSITORY / SUBMODULE TO DELETE: {}\nCheckout files and Git metadata removed with this worktree are permanently lost, including any branches, stashes and unpushed commits stored there.\nGit store: {}\nHEAD: {}\nRefs (branches, tags, stashes and remote-tracking refs):\n{}\nLocal changes, untracked and ignored paths (also deleted):\n{}\n",
                            display_name(repository.path.as_os_str()),
                            display_name(repository.common_dir.as_os_str()),
                            visible(&repository.head),
                            visible(&repository.refs),
                            visible(&repository.status),
                        ));
                    }
                }
                text.push_str(&format!(
                    "Local changes and untracked files:\n{}\n",
                    visible(&runner.run(
                        &record.path,
                        &["status", "--short", "--untracked-files=all"]
                    )?)
                ));
                let ignored = runner.run(
                    &record.path,
                    &[
                        "ls-files",
                        "--others",
                        "--ignored",
                        "--exclude-standard",
                        "--directory",
                        "-z",
                    ],
                )?;
                text.push_str("Ignored content (also deleted):\n");
                for path in ignored.split(|&b| b == 0).filter(|path| !path.is_empty()) {
                    text.push_str(&format!(
                        "  {}\n",
                        display_name(worktrees::os_path(path).as_os_str())
                    ));
                }
                if !detached.is_empty() {
                    text.push_str(&format!(
                        "Detached-only commits losing their worktree reference:\n{}\n",
                        visible(&detached)
                    ));
                }
                let current = if remove_nested {
                    fingerprint_removal(&record.path, &runner)?
                } else {
                    fingerprint(&record.path, &runner)?
                };
                if !state.same_checkout(&current)
                    || nested.as_ref().is_some_and(|expected| {
                        nested_repositories(&record.path, &current, &runner).as_ref()
                            != Ok(expected)
                    })
                {
                    return Err(
                        "Checkout changed while preparing removal; make a fresh preview".into(),
                    );
                }
                targets.push(RemovalTarget {
                    record,
                    anchor: target_anchor,
                    state,
                    nested,
                });
            }
            text.push_str(&format!("\nReported selected checkout total: {}\nReported usage is not a guarantee of bytes freed. Parent repository objects and branches are retained; opted-in nested repositories may lose their own Git stores and history.\n", crate::human_size(reported_total)));
            (
                text,
                PreviewKind::Remove {
                    targets,
                    force,
                    protected,
                },
            )
        }
        WorktreeRequest::Prune { .. } => {
            let expiry = expiry(&common, &runner)?;
            let dry_run = prune_command(&common, &expiry, true, &runner)?;
            let mut text = format!(
                "Prune stale worktree registrations\nExpiry: {expiry} (gc.worktreePruneExpire)\n\nThis only removes eligible stale administrative records. It does not delete checkout directories, branches, or Git objects. Locked records remain protected.\n\nMissing/prunable entries in the registry:\n"
            );
            for record in &registry.records {
                if record.prunable.is_some() {
                    text.push_str(&format!(
                        "{} · {}{}\n",
                        display_name(record.path.as_os_str()),
                        record.prunable.as_deref().unwrap_or(""),
                        if record.locked.is_some() {
                            " (locked; retained)"
                        } else {
                            ""
                        }
                    ));
                }
            }
            text.push_str(&format!(
                "\nGit dry-run (determines actual eligibility):\n{}",
                if dry_run.is_empty() {
                    "No records are eligible for pruning.".into()
                } else {
                    visible(&dry_run)
                }
            ));
            (text, PreviewKind::Prune { expiry, dry_run })
        }
    };
    if worktrees::discover(&common, runner.cancel_flag())?.records != registry.records {
        return Err(
            "Worktree registrations changed while preparing the preview; refresh and try again"
                .into(),
        );
    }
    Ok(WorktreePreview {
        title,
        text: visible(text.as_bytes()),
        common_dir: common,
        registry,
        anchor,
        kind,
    })
}

fn validate_target(
    target: &RemovalTarget,
    registry: &Registry,
    protected: &[PathBuf],
    runner: &Runner<'_>,
) -> Result<(), String> {
    target.anchor.validate().map_err(|e| e.to_string())?;
    inspect_checkout(&target.record.path, runner)?;
    safe_target(&target.record, registry, protected)?;
    if worktrees::discover(&target.record.path, runner.cancel_flag())?.common_dir
        != registry.common_dir
    {
        return Err("Checkout repository changed since preview".into());
    }
    let current = if target.nested.is_some() {
        fingerprint_removal(&target.record.path, runner)?
    } else {
        fingerprint(&target.record.path, runner)?
    };
    if !target.state.same_checkout(&current)
        || target.nested.as_ref().is_some_and(|expected| {
            nested_repositories(&target.record.path, &current, runner).as_ref() != Ok(expected)
        })
    {
        return Err(format!(
            "{} changed since preview; nothing further will be removed",
            display_name(target.record.path.as_os_str())
        ));
    }
    Ok(())
}

impl WorktreePreview {
    pub fn execute(&self, cancel: &AtomicBool) -> Result<String, String> {
        execute_with(self, &Runner::readonly(cancel))
    }
}

pub(crate) fn execute_with(
    preview: &WorktreePreview,
    runner: &Runner<'_>,
) -> Result<String, String> {
    let runner = runner.local();
    preview.anchor.validate().map_err(|e| e.to_string())?;
    if worktrees::discover(&preview.common_dir, runner.cancel_flag())?.records
        != preview.registry.records
    {
        return Err("Worktree registrations changed since preview. No action was executed; refresh and preview again.".into());
    }
    match &preview.kind {
        PreviewKind::Prune {
            expiry: expected,
            dry_run,
        } => {
            if expiry(&preview.common_dir, &runner)? != *expected
                || prune_command(&preview.common_dir, expected, true, &runner)? != *dry_run
            {
                return Err("Prune eligibility changed since preview; preview again".into());
            }
            let output = prune_command(&preview.common_dir, expected, false, &runner)?;
            Ok(format!(
                "Stale-registration pruning complete. Checkout directories and branches were not removed.\n\n{}",
                visible(&output)
            ))
        }
        PreviewKind::Remove {
            targets,
            force,
            protected,
        } => {
            // Validate every selection before allowing any member of a batch to change.
            for target in targets {
                validate_target(target, &preview.registry, protected, &runner)?;
            }
            let mut expected = preview.registry.clone();
            let mut removed = Vec::new();
            let result = (|| -> Result<(), String> {
                for target in targets {
                    runner.check()?;
                    if worktrees::discover(&preview.common_dir, runner.cancel_flag())?.records
                        != expected.records
                    {
                        return Err("Worktree registrations changed during removal; stopped before the next checkout".into());
                    }
                    validate_target(target, &expected, protected, &runner)?;
                    let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
                    if *force {
                        args.push("--force".into());
                    }
                    args.push("--".into());
                    args.push(target.record.path.as_os_str().to_owned());
                    if let Err(error) = runner.run_os(&preview.common_dir, &args) {
                        let missing = fs::symlink_metadata(&target.record.path)
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
                        if missing
                            && worktrees::discover(&preview.common_dir, &AtomicBool::new(false))
                                .is_ok_and(|registry| {
                                    !registry
                                        .records
                                        .iter()
                                        .any(|record| record.path == target.record.path)
                                })
                        {
                            removed.push(target.record.path.clone());
                        }
                        return Err(format!(
                            "{error}\nThe last attempted checkout may have been partially removed; review the refreshed list."
                        ));
                    }
                    removed.push(target.record.path.clone());
                    expected
                        .records
                        .retain(|record| record.path != target.record.path);
                }
                Ok(())
            })();
            let summary = format!(
                "Removed {} of {} worktrees. Parent repository branches and shared Git history are kept. Opted-in nested repositories and their local history may have been deleted.\n{}",
                removed.len(),
                targets.len(),
                removed
                    .iter()
                    .map(|path| display_name(path.as_os_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            match result {
                Ok(()) => Ok(summary),
                Err(error) => Err(format!(
                    "Confirmed {summary}\n\n{error}\nCompleted removals are kept. Refresh and review before retrying."
                )),
            }
        }
    }
}
