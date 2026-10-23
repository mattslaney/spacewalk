use spacewalk::git_actions::{self, GitAction, GitCommand, GitOptions};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-C"])
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim_end().into()
}

struct Fixture {
    _temp: tempfile::TempDir,
    work: PathBuf,
    peer: PathBuf,
    remote: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().join("work café");
        let peer = temp.path().join("peer");
        let remote = temp.path().join("remote.git");
        fs::create_dir(&work).unwrap();
        git(&work, &["init", "--initial-branch=main"]);
        identity(&work);
        fs::write(work.join("tracked"), "initial\n").unwrap();
        fs::write(work.join(".gitignore"), "cache/\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "initial"]);
        git(
            temp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["push", "-u", "origin", "main"]);
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
        );
        identity(&peer);
        Self {
            _temp: temp,
            work,
            peer,
            remote,
        }
    }
    fn prepare(&self, action: GitAction, options: GitOptions) -> git_actions::GitPreview {
        git_actions::prepare(&self.work, action, options, &AtomicBool::new(false)).unwrap()
    }
    fn stash_and_local_commit(&self) {
        fs::write(self.work.join("tracked"), "stashed\n").unwrap();
        git(&self.work, &["stash", "push", "-m", "keep me"]);
        git(&self.work, &["commit", "--allow-empty", "-m", "local-only"]);
        git(&self.work, &["branch", "local-feature"]);
    }
    fn dirty(&self) {
        fs::write(self.work.join("tracked"), "staged\n").unwrap();
        git(&self.work, &["add", "tracked"]);
        fs::write(self.work.join("tracked"), "unstaged\n").unwrap();
        fs::write(self.work.join("untracked"), "remove").unwrap();
        fs::create_dir_all(self.work.join("cache/nested")).unwrap();
        fs::write(self.work.join("cache/nested/output"), "remove").unwrap();
    }
}
fn identity(path: &Path) {
    git(path, &["config", "user.name", "Spacewalk Test"]);
    git(path, &["config", "user.email", "test@example.invalid"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

fn report(path: &Path, command: GitCommand) -> String {
    git_actions::report(path, command, &AtomicBool::new(false)).unwrap()
}

#[test]
fn inspection_reports_explain_saved_and_unsaved_work_without_changing_it() {
    let f = Fixture::new();
    f.stash_and_local_commit();
    f.dirty();
    let before_status = git(&f.work, &["status", "--porcelain=v1", "--ignored"]);
    let before_refs = git(&f.work, &["show-ref"]);
    let before_index = fs::read(f.work.join(".git/index")).unwrap();
    let status = report(&f.work, GitCommand::Status);
    assert!(status.contains("Worktree: DIRTY"));
    assert!(status.contains("Ahead: 1"));
    assert!(status.contains("Behind: 0"));
    assert!(!status.contains("MM tracked"));
    assert!(!status.contains("?? untracked"));
    let branches = report(&f.work, GitCommand::Branches);
    assert!(branches.contains("local-feature"));
    assert!(branches.contains("origin/main"));
    assert!(branches.contains("↑1 ahead"));
    let commits = report(&f.work, GitCommand::LocalCommits);
    assert!(commits.contains("local-only"));
    assert!(!commits.contains(" initial"));
    let stashes = report(&f.work, GitCommand::Stashes);
    assert!(stashes.contains("stash@{0}"));
    assert!(stashes.contains("keep me"));
    assert_eq!(
        git(&f.work, &["status", "--porcelain=v1", "--ignored"]),
        before_status
    );
    assert_eq!(git(&f.work, &["show-ref"]), before_refs);
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), before_index);
    assert_eq!(
        fs::read_to_string(f.work.join("tracked")).unwrap(),
        "unstaged\n"
    );
    assert!(f.work.join("cache/nested/output").exists());
}

#[test]
fn reports_handle_unborn_detached_and_untracked_branches() {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "--initial-branch=main"]);
    identity(temp.path());
    fs::write(temp.path().join("new"), "first\n").unwrap();
    git(temp.path(), &["add", "new"]);
    assert!(report(temp.path(), GitCommand::Status).contains("Worktree: DIRTY"));
    assert!(report(temp.path(), GitCommand::Branches).contains("No local-only branches"));
    assert!(report(temp.path(), GitCommand::LocalCommits).contains("No commits yet"));
    assert!(report(temp.path(), GitCommand::Stashes).contains("No stashes"));
    assert!(report(temp.path(), GitCommand::Fetch).contains("No remotes configured"));
    git(temp.path(), &["commit", "-m", "first"]);
    assert!(report(temp.path(), GitCommand::LocalCommits).contains("first"));
    git(temp.path(), &["switch", "--detach"]);
    assert!(report(temp.path(), GitCommand::Status).contains("detached HEAD"));
    assert!(report(temp.path(), GitCommand::LocalCommits).contains("detached HEAD"));
}

#[test]
fn fetch_refreshes_tracking_refs_without_changing_local_work() {
    let f = Fixture::new();
    git(&f.peer, &["commit", "--allow-empty", "-m", "remote-only"]);
    git(&f.peer, &["push"]);
    f.dirty();
    let head = git(&f.work, &["rev-parse", "HEAD"]);
    let index = fs::read(f.work.join(".git/index")).unwrap();
    let old_tracking = git(&f.work, &["rev-parse", "origin/main"]);
    report(&f.work, GitCommand::Status);
    report(&f.work, GitCommand::Branches);
    assert_eq!(git(&f.work, &["rev-parse", "origin/main"]), old_tracking);
    assert!(report(&f.work, GitCommand::Fetch).contains("Behind: 1"));
    assert_eq!(
        git(&f.work, &["rev-parse", "origin/main"]),
        git(&f.peer, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&f.work, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(f.work.join("tracked")).unwrap(),
        "unstaged\n"
    );
    assert!(f.work.join("untracked").exists());
}

#[test]
fn clean_preserves_commits_branches_and_stashes_by_default() {
    let f = Fixture::new();
    f.stash_and_local_commit();
    f.dirty();
    let before = git(&f.work, &["rev-parse", "HEAD"]);
    let preview = f.prepare(GitAction::Clean, GitOptions::default());
    assert!(preview.text.contains("cache/"));
    assert!(preview.text.contains("untracked"));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(git(&f.work, &["rev-parse", "HEAD"]), before);
    assert!(git(&f.work, &["branch", "--list", "local-feature"]).contains("local-feature"));
    assert!(!git(&f.work, &["stash", "list"]).is_empty());
    assert_eq!(
        fs::read_to_string(f.work.join("tracked")).unwrap(),
        "initial\n"
    );
    assert!(!f.work.join("untracked").exists());
    assert!(!f.work.join("cache").exists());
}

#[test]
fn reset_and_optional_clean_discard_local_commits_but_keep_other_history_by_default() {
    for action in [GitAction::Reset, GitAction::Clean] {
        let f = Fixture::new();
        f.stash_and_local_commit();
        f.dirty();
        let options = GitOptions {
            discard_commits: true,
            ..Default::default()
        };
        let preview = f.prepare(action, options);
        assert!(preview.text.contains("local-only"));
        preview.execute(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            git(&f.work, &["rev-parse", "HEAD"]),
            git(&f.peer, &["rev-parse", "HEAD"])
        );
        assert!(!git(&f.work, &["stash", "list"]).is_empty());
        assert!(!git(&f.work, &["branch", "--list", "local-feature"]).is_empty());
    }
}

#[test]
fn reset_optionally_deletes_other_branches_and_stashes_but_skips_occupied_branches() {
    let f = Fixture::new();
    f.stash_and_local_commit();
    let other = f._temp.path().join("other-worktree");
    git(
        &f.work,
        &["worktree", "add", "-b", "occupied", other.to_str().unwrap()],
    );
    f.dirty();
    let preview = f.prepare(
        GitAction::Reset,
        GitOptions {
            delete_branches: true,
            clear_stashes: true,
            ..Default::default()
        },
    );
    assert!(preview.text.contains("DELETE local-feature"));
    assert!(preview.text.contains("KEEP occupied"));
    assert!(preview.text.contains("keep me"));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(git(&f.work, &["branch", "--list", "local-feature"]).is_empty());
    assert!(!git(&f.work, &["branch", "--list", "occupied"]).is_empty());
    assert!(git(&f.work, &["stash", "list"]).is_empty());
    assert!(other.join("tracked").exists());
}

#[test]
fn nested_working_and_bare_repositories_are_not_cleaned() {
    let f = Fixture::new();
    let nested = f.work.join("cache/nested repo [keep]");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "--initial-branch=main"]);
    fs::write(nested.join("precious"), "keep").unwrap();
    git(&f.work, &["init", "--bare", "nested-bare.git"]);
    fs::write(f.work.join("untracked"), "remove").unwrap();
    let preview = f.prepare(GitAction::Clean, GitOptions::default());
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(fs::read_to_string(nested.join("precious")).unwrap(), "keep");
    assert!(f.work.join("nested-bare.git/HEAD").exists());
    assert!(!f.work.join("untracked").exists());
}

#[test]
fn stale_preview_refuses_to_change_checkout_or_new_branches() {
    let f = Fixture::new();
    f.dirty();
    let preview = f.prepare(GitAction::Clean, GitOptions::default());
    fs::write(f.work.join("untracked"), "new content").unwrap();
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    assert!(f.work.join("cache").exists());
    let preview = f.prepare(
        GitAction::Reset,
        GitOptions {
            delete_branches: true,
            ..Default::default()
        },
    );
    git(&f.work, &["branch", "new-after-preview"]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    assert!(f.work.join("untracked").exists());
}

#[test]
fn pull_fast_forwards_and_rejects_divergence() {
    let f = Fixture::new();
    git(&f.peer, &["commit", "--allow-empty", "-m", "remote change"]);
    git(&f.peer, &["push"]);
    f.prepare(GitAction::Pull, GitOptions::default())
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        git(&f.work, &["rev-parse", "HEAD"]),
        git(&f.peer, &["rev-parse", "HEAD"])
    );
    git(&f.work, &["commit", "--allow-empty", "-m", "local"]);
    git(
        &f.peer,
        &["commit", "--allow-empty", "-m", "remote divergence"],
    );
    git(&f.peer, &["push"]);
    let before = git(&f.work, &["rev-parse", "HEAD"]);
    assert!(
        f.prepare(GitAction::Pull, GitOptions::default())
            .execute(&AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(git(&f.work, &["rev-parse", "HEAD"]), before);
}

#[test]
fn push_is_normal_by_default_and_force_uses_a_fixed_lease() {
    let f = Fixture::new();
    git(&f.work, &["commit", "--allow-empty", "-m", "local"]);
    f.prepare(GitAction::Push, GitOptions::default())
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        git(&f.remote, &["rev-parse", "refs/heads/main"]),
        git(&f.work, &["rev-parse", "HEAD"])
    );
    git(&f.work, &["reset", "--hard", "HEAD~1"]);
    git(&f.work, &["commit", "--allow-empty", "-m", "replacement"]);
    assert!(
        f.prepare(GitAction::Push, GitOptions::default())
            .execute(&AtomicBool::new(false))
            .is_err()
    );
    let preview = f.prepare(
        GitAction::Push,
        GitOptions {
            force: true,
            ..Default::default()
        },
    );
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        git(&f.remote, &["rev-parse", "refs/heads/main"]),
        git(&f.work, &["rev-parse", "HEAD"])
    );
}

#[test]
fn force_lease_rejects_remote_changes_after_preview() {
    let f = Fixture::new();
    git(&f.work, &["commit", "--allow-empty", "-m", "local"]);
    let preview = f.prepare(
        GitAction::Push,
        GitOptions {
            force: true,
            ..Default::default()
        },
    );
    git(
        &f.peer,
        &["commit", "--allow-empty", "-m", "remote won race"],
    );
    git(&f.peer, &["push"]);
    let error = preview.execute(&AtomicBool::new(false)).unwrap_err();
    assert!(error.contains("stale info"), "{error}");
    assert_eq!(
        git(&f.remote, &["rev-parse", "refs/heads/main"]),
        git(&f.peer, &["rev-parse", "HEAD"])
    );
}

#[test]
fn discovery_supports_bare_hubs_but_checkout_actions_require_a_checkout() {
    let f = Fixture::new();
    fs::create_dir(f.work.join("sub")).unwrap();
    assert_eq!(
        git_actions::inspect(&f.work.join("sub"), &AtomicBool::new(false))
            .unwrap()
            .root,
        f.work
    );
    git(&f.work, &["branch", "--unset-upstream"]);
    assert!(
        git_actions::prepare(
            &f.work,
            GitAction::Pull,
            GitOptions::default(),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("upstream")
    );
    git(&f.work, &["switch", "--detach"]);
    assert!(
        git_actions::prepare(
            &f.work,
            GitAction::Reset,
            GitOptions::default(),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("detached")
    );
    assert!(
        git_actions::inspect(&f.remote, &AtomicBool::new(false))
            .unwrap()
            .bare
    );
    assert!(
        git_actions::prepare(
            &f.remote,
            GitAction::Clean,
            GitOptions::default(),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("Bare")
    );
}

#[test]
fn cancellation_and_network_failures_do_not_clean_any_files() {
    let f = Fixture::new();
    f.dirty();
    assert!(
        git_actions::prepare(
            &f.work,
            GitAction::Clean,
            GitOptions::default(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    git(
        &f.work,
        &[
            "remote",
            "set-url",
            "origin",
            "/definitely/missing/spacewalk-remote.git",
        ],
    );
    let error = git_actions::prepare(
        &f.work,
        GitAction::Reset,
        GitOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("shell"));
    assert!(f.work.join("untracked").exists());
    assert!(f.work.join("cache").exists());
}

#[test]
fn output_hides_credentials_and_terminal_controls() {
    let text = git_actions::visible(
        b"fatal: https://user:secret@example.invalid/repo.git\n\x1b[31merror\rnext",
    );
    assert!(!text.contains("secret"));
    assert!(!text.contains('\x1b'));
    assert!(text.contains("https://example.invalid/repo.git"));
    assert!(
        !git_actions::visible(b"HTTPS://user:secret@example.invalid/repo.git").contains("secret")
    );
}

#[test]
fn submodules_are_not_reset_or_cleaned_even_with_recursive_git_configuration() {
    let f = Fixture::new();
    git(
        &f.work,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            f.remote.to_str().unwrap(),
            "module",
        ],
    );
    git(&f.work, &["commit", "-m", "add module"]);
    git(&f.work, &["config", "submodule.recurse", "true"]);
    fs::write(f.work.join("module/tracked"), "keep module changes").unwrap();
    fs::write(f.work.join("module/untracked"), "keep module file").unwrap();
    f.prepare(GitAction::Clean, GitOptions::default())
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.work.join("module/tracked")).unwrap(),
        "keep module changes"
    );
    assert!(f.work.join("module/untracked").exists());
    git(&f.work, &["config", "fetch.recurseSubmodules", "true"]);
    git(
        &f.work.join("module"),
        &[
            "remote",
            "set-url",
            "origin",
            "/missing/submodule-remote.git",
        ],
    );
    f.prepare(GitAction::Reset, GitOptions::default())
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.work.join("module/tracked")).unwrap(),
        "keep module changes"
    );
    assert!(f.work.join("module/untracked").exists());
}

#[test]
fn interrupted_git_operations_are_left_for_the_shell() {
    let f = Fixture::new();
    f.dirty();
    fs::write(
        f.work.join(".git/MERGE_HEAD"),
        git(&f.work, &["rev-parse", "HEAD"]),
    )
    .unwrap();
    let error = git_actions::prepare(
        &f.work,
        GitAction::Clean,
        GitOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("in progress"));
    assert!(f.work.join("untracked").exists());
}

#[test]
fn reset_fetch_preserves_local_tags_even_with_aggressive_fetch_configuration() {
    let f = Fixture::new();
    git(&f.work, &["tag", "local-only"]);
    git(&f.work, &["config", "fetch.pruneTags", "true"]);
    git(
        &f.work,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    );
    f.prepare(GitAction::Reset, GitOptions::default())
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(git(&f.work, &["tag", "--list", "local-only"]), "local-only");
}

#[test]
fn cleanup_refuses_tracked_files_overlapping_a_nested_repository() {
    let f = Fixture::new();
    fs::create_dir(f.work.join("nested")).unwrap();
    fs::write(f.work.join("nested/file"), "parent tracks this").unwrap();
    git(&f.work, &["add", "nested/file"]);
    git(&f.work, &["commit", "-m", "tracked nested content"]);
    git(&f.work.join("nested"), &["init", "--initial-branch=main"]);
    let error = git_actions::prepare(
        &f.work,
        GitAction::Clean,
        GitOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("overlap nested"));
    assert_eq!(
        fs::read_to_string(f.work.join("nested/file")).unwrap(),
        "parent tracks this"
    );
}

#[cfg(unix)]
#[test]
fn authentication_errors_are_noninteractive_and_action_workers_can_be_cancelled() {
    use spacewalk::git_actions::{GitActionTask, GitEvent, GitJob};
    use std::{
        os::unix::fs::PermissionsExt,
        thread,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let script = f._temp.path().join("ssh-stub");
    let arguments = f._temp.path().join("ssh-arguments");
    fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'Authentication required\\n' >&2\nexit 255\n", arguments.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.work,
        &["config", "core.sshCommand", script.to_str().unwrap()],
    );
    git(
        &f.work,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@fixture.invalid/repo.git",
        ],
    );
    let error = git_actions::prepare(
        &f.work,
        GitAction::Push,
        GitOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("Authentication required"));
    assert!(error.contains("shell"));
    assert!(
        fs::read_to_string(&arguments)
            .unwrap()
            .contains("BatchMode=yes")
    );
    fs::remove_file(&arguments).unwrap();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'started' > '{}'\nsleep 60\n",
            arguments.display()
        ),
    )
    .unwrap();
    let task = GitActionTask::start(GitJob::Preview {
        path: f.work.clone(),
        action: GitAction::Push,
        options: GitOptions::default(),
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !arguments.exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    task.cancel();
    loop {
        match task.receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
            GitEvent::Previewed(Err(error)) => {
                assert!(error.contains("cancelled"));
                break;
            }
            GitEvent::Progress(_) => {}
            _ => panic!("unexpected worker result"),
        }
    }
}

#[cfg(unix)]
#[test]
fn git_action_reports_stdout_before_the_command_finishes() {
    use spacewalk::git_actions::{GitActionTask, GitEvent, GitJob};
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let f = Fixture::new();
    git(&f.work, &["config", "core.hooksPath", ".git/hooks"]);
    let hook = f.work.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf 'Uploading fixture payload...\\n'\nsleep 1\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let preview = f.prepare(GitAction::Push, GitOptions::default());
    let task = GitActionTask::start(GitJob::Execute(Box::new(preview)));
    let mut saw_output = false;
    loop {
        match task.receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
            GitEvent::Progress(line) => {
                if line.contains("Uploading fixture payload") {
                    saw_output = true;
                }
            }
            GitEvent::Finished(result) => {
                result.unwrap();
                assert!(
                    saw_output,
                    "command output was not delivered until completion"
                );
                break;
            }
            _ => panic!("unexpected event"),
        }
    }
}
