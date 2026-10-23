use spacewalk::{
    git_actions::{self, GitCommand},
    git_manage::{self, Action, Branch, Entry},
};
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
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim_end().into()
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    peer: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work café");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]);
        identity(&root);
        fs::write(root.join("tracked"), "initial").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "initial"]);
        let remote = temp.path().join("remote.git");
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
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&root, &["push", "-u", "origin", "main"]);
        let peer = temp.path().join("peer");
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
        );
        identity(&peer);
        Self {
            _temp: temp,
            root,
            remote,
            peer,
        }
    }
    fn read(&self, command: GitCommand) -> git_manage::Report {
        git_manage::read(&self.root, command, &AtomicBool::new(false)).unwrap()
    }
    fn branch(&self, name: &str) -> Branch {
        self.read(GitCommand::Branches)
            .entries
            .into_iter()
            .find_map(|e| match e {
                Entry::Branch(b) if b.name == name => Some(b),
                _ => None,
            })
            .unwrap()
    }
    fn prepare(&self, action: Action) -> git_manage::Preview {
        git_manage::prepare(&self.root, action, &AtomicBool::new(false)).unwrap()
    }
}
fn identity(path: &Path) {
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "user.email", "test@example.invalid"]);
}
fn commit(path: &Path, subject: &str) {
    git(path, &["commit", "--allow-empty", "-m", subject]);
}

#[test]
fn branch_list_filters_synced_and_remote_only_branches_without_fetching() {
    let f = Fixture::new();
    git(&f.root, &["branch", "local-only"]);
    commit(&f.root, "unpushed");
    git(&f.root, &["branch", "pending"]);
    git(
        &f.root,
        &["branch", "--set-upstream-to=origin/main", "pending"],
    );
    git(&f.root, &["reset", "--hard", "origin/main"]);
    git(
        &f.root,
        &["remote", "set-url", "origin", "/missing/no-network.git"],
    );
    let before = git(&f.root, &["show-ref"]);
    let index = fs::read(f.root.join(".git/index")).unwrap();
    let rows = f.read(GitCommand::Branches).entries;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|e| matches!(e, Entry::Branch(b) if b.name == "local-only" && b.unpushed == 0 && b.upstream.is_none())));
    assert!(rows.iter().any(|e| matches!(e, Entry::Branch(b) if b.name == "pending" && b.unpushed == 1 && b.ahead == Some(1))));
    assert_eq!(git(&f.root, &["show-ref"]), before);
    assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), index);
}
#[test]
fn deletion_previews_unpushed_work_and_keeps_files_remotes_and_other_branches() {
    let f = Fixture::new();
    commit(&f.root, "save-this-commit");
    git(&f.root, &["branch", "pending"]);
    git(&f.root, &["reset", "--hard", "origin/main"]);
    git(
        &f.root,
        &["branch", "--set-upstream-to=origin/main", "pending"],
    );
    let branch = f.branch("pending");
    let preview = f.prepare(Action::DeleteBranch(branch));
    assert!(preview.text.contains("save-this-commit"));
    assert!(preview.text.contains("1 commits absent"));
    assert!(git(&f.root, &["branch", "--list", "pending"]).contains("pending"));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(git(&f.root, &["branch", "--list", "pending"]).is_empty());
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&f.root)
            .args(["config", "--get", "branch.pending.remote"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(f.root.join("tracked")).unwrap(),
        "initial"
    );
    assert_eq!(
        git(&f.remote, &["rev-parse", "main"]),
        git(&f.root, &["rev-parse", "main"])
    );
}
#[test]
fn branch_delete_protects_checked_out_branches_and_refuses_stale_refs_and_worktrees() {
    let f = Fixture::new();
    commit(&f.root, "local");
    assert!(
        git_manage::prepare(
            &f.root,
            Action::DeleteBranch(f.branch("main")),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("checked out")
    );
    git(&f.root, &["branch", "pending"]);
    let preview = f.prepare(Action::DeleteBranch(f.branch("pending")));
    git(&f.root, &["branch", "-f", "pending", "origin/main"]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    git(&f.root, &["branch", "--unset-upstream", "pending"]);
    let preview = f.prepare(Action::DeleteBranch(f.branch("pending")));
    let linked = f._temp.path().join("linked");
    git(
        &f.root,
        &["worktree", "add", linked.to_str().unwrap(), "pending"],
    );
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    assert!(
        git_manage::prepare(
            &f.root,
            Action::DeleteBranch(f.branch("pending")),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("checked out")
    );
    assert!(linked.join("tracked").exists());
}
#[test]
fn stash_selection_deletes_only_selected_entry_and_rejects_shifted_selectors() {
    let f = Fixture::new();
    for message in ["oldest", "middle", "newest"] {
        fs::write(f.root.join("tracked"), message).unwrap();
        git(&f.root, &["stash", "push", "-m", message]);
    }
    let stashes = f.read(GitCommand::Stashes).entries;
    let Entry::Stash(selected) = &stashes[1] else {
        panic!()
    };
    assert!(selected.subject.contains("middle"));
    let preview = f.prepare(Action::DropStash(selected.clone()));
    assert!(preview.text.contains("entire repository"));
    fs::write(f.root.join("tracked"), "fourth").unwrap();
    git(&f.root, &["stash", "push", "-m", "fourth"]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    let rows = f.read(GitCommand::Stashes).entries;
    let Entry::Stash(selected) = &rows[2] else {
        panic!()
    };
    f.prepare(Action::DropStash(selected.clone()))
        .execute(&AtomicBool::new(false))
        .unwrap();
    let after = git(&f.root, &["stash", "list"]);
    assert!(!after.contains("middle"));
    for name in ["oldest", "newest", "fourth"] {
        assert!(after.contains(name));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("tracked")).unwrap(),
        "initial"
    );
}
#[test]
fn sync_publishes_local_only_branch_and_configures_its_upstream_without_switching() {
    let f = Fixture::new();
    git(&f.root, &["branch", "publish-me"]);
    fs::write(f.root.join("tracked"), "unsaved changes").unwrap();
    let preview = f.prepare(Action::Sync(f.branch("publish-me")));
    assert!(preview.text.contains("refs/heads/publish-me"));
    assert!(preview.text.contains("new remote branch"));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(git(&f.root, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(
        git(&f.root, &["rev-parse", "publish-me@{upstream}"]),
        git(&f.remote, &["rev-parse", "publish-me"])
    );
    assert_eq!(
        fs::read_to_string(f.root.join("tracked")).unwrap(),
        "unsaved changes"
    );
    assert!(f.read(GitCommand::Branches).entries.is_empty());
}
#[test]
fn commit_list_excludes_commits_pushed_to_any_remote_and_push_publishes_branch() {
    let f = Fixture::new();
    commit(&f.root, "already-on-backup");
    git(
        &f.root,
        &["remote", "add", "backup", f.remote.to_str().unwrap()],
    );
    git(&f.root, &["push", "backup", "HEAD:refs/heads/backup"]);
    commit(&f.root, "not-pushed-anywhere");
    let report = f.read(GitCommand::LocalCommits);
    assert_eq!(report.entries.len(), 1);
    assert!(report.entries[0].label().contains("not-pushed-anywhere"));
    let preview = f.prepare(Action::PushHead(report.repository.head.unwrap()));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        git(&f.remote, &["rev-parse", "main"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    assert!(f.read(GitCommand::LocalCommits).entries.is_empty());
}
#[test]
fn sync_fast_forwards_a_checked_out_branch_then_pushes_without_touching_unrelated_files() {
    let f = Fixture::new();
    commit(&f.root, "local-first");
    let selected = f.branch("main");
    git(&f.root, &["push"]);
    git(&f.peer, &["pull", "--ff-only"]);
    commit(&f.peer, "remote-next");
    git(&f.peer, &["push"]);
    let preview = f.prepare(Action::Sync(selected));
    let remote = git(&f.peer, &["rev-parse", "HEAD"]);
    assert!(preview.text.contains(&remote));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), remote);
    assert_eq!(
        fs::read_to_string(f.root.join("tracked")).unwrap(),
        "initial"
    );
}
#[test]
fn sync_can_fast_forward_an_unchecked_branch_and_never_switches_current_checkout() {
    let f = Fixture::new();
    git(&f.root, &["branch", "publish-me"]);
    git(&f.peer, &["switch", "-c", "publish-me"]);
    commit(&f.peer, "remote-next");
    git(&f.peer, &["push", "-u", "origin", "publish-me"]);
    let old_head = git(&f.root, &["rev-parse", "HEAD"]);
    f.prepare(Action::Sync(f.branch("publish-me")))
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        git(&f.root, &["rev-parse", "publish-me"]),
        git(&f.peer, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), old_head);
    assert_eq!(git(&f.root, &["symbolic-ref", "--short", "HEAD"]), "main");
}
#[test]
fn sync_refuses_divergence_and_dirty_fast_forwards_and_push_refuses_remote_changes() {
    let f = Fixture::new();
    commit(&f.root, "local");
    let selected = f.branch("main");
    commit(&f.peer, "remote");
    git(&f.peer, &["push"]);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    assert!(
        git_manage::prepare(
            &f.root,
            Action::Sync(selected.clone()),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("diverged")
    );
    assert!(
        git_manage::prepare(
            &f.root,
            Action::Push(selected.clone()),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("behind")
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    git(&f.peer, &["fetch", f.root.to_str().unwrap(), "main"]);
    git(&f.peer, &["reset", "--hard", "FETCH_HEAD"]);
    commit(&f.peer, "remote-descendant");
    git(&f.peer, &["push", "--force"]);
    fs::write(f.root.join("tracked"), "dirty").unwrap();
    assert!(
        git_manage::prepare(
            &f.root,
            Action::Sync(selected.clone()),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("dirty")
    );
    git(&f.root, &["restore", "tracked"]);
    let preview = f.prepare(Action::Sync(selected));
    commit(&f.peer, "later");
    git(&f.peer, &["push"]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("Remote changed since preview")
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
}
#[test]
fn cancellation_and_a_changed_commit_list_never_publish_unreviewed_commits() {
    let f = Fixture::new();
    commit(&f.root, "first");
    let selected = f.branch("main");
    assert!(git_manage::prepare(&f.root, Action::Sync(selected), &AtomicBool::new(true)).is_err());
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    commit(&f.root, "second");
    assert!(
        git_manage::prepare(&f.root, Action::PushHead(head), &AtomicBool::new(false))
            .unwrap_err()
            .contains("Branch changed")
    );
    assert_ne!(
        git(&f.remote, &["rev-parse", "main"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
}
#[test]
fn status_is_only_a_cleanliness_and_ahead_behind_summary() {
    let f = Fixture::new();
    let clean = git_actions::report(&f.root, GitCommand::Status, &AtomicBool::new(false)).unwrap();
    assert!(clean.contains("Worktree: CLEAN"));
    assert!(clean.contains("Ahead: 0"));
    assert!(clean.contains("Behind: 0"));
    fs::write(f.root.join("private-name"), "not included").unwrap();
    commit(&f.root, "pending");
    let dirty = git_actions::report(&f.root, GitCommand::Status, &AtomicBool::new(false)).unwrap();
    assert!(dirty.contains("Worktree: DIRTY"));
    assert!(dirty.contains("Ahead: 1"));
    assert!(!dirty.contains("private-name"));
}

#[test]
fn sync_updates_the_selected_branch_in_its_own_worktree() {
    let f = Fixture::new();
    let linked = f._temp.path().join("topic-tree");
    git(
        &f.root,
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    git(&linked, &["push", "-u", "origin", "topic"]);
    commit(&linked, "local-topic");
    let selected = f.branch("topic");
    git(&linked, &["push"]);
    git(&f.peer, &["fetch", "origin"]);
    git(&f.peer, &["switch", "topic"]);
    commit(&f.peer, "remote-topic");
    git(&f.peer, &["push"]);
    let main = git(&f.root, &["rev-parse", "HEAD"]);
    f.prepare(Action::Sync(selected))
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        git(&linked, &["rev-parse", "HEAD"]),
        git(&f.peer, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), main);
    assert_eq!(git(&linked, &["symbolic-ref", "--short", "HEAD"]), "topic");
}

#[test]
fn branches_in_interrupted_rebases_cannot_be_deleted_or_synced() {
    let f = Fixture::new();
    git(&f.root, &["switch", "-c", "topic"]);
    fs::write(f.root.join("tracked"), "topic change").unwrap();
    git(&f.root, &["commit", "-am", "topic"]);
    git(&f.root, &["switch", "main"]);
    fs::write(f.root.join("tracked"), "main change").unwrap();
    git(&f.root, &["commit", "-am", "main"]);
    git(&f.root, &["switch", "topic"]);
    let rebase = Command::new("git")
        .arg("-C")
        .arg(&f.root)
        .args(["rebase", "main"])
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .output()
        .unwrap();
    assert!(!rebase.status.success());
    let selected = f.branch("topic");
    assert!(
        selected.busy,
        "Git must identify the branch as in use during a rebase"
    );
    assert!(
        git_manage::prepare(
            &f.root,
            Action::DeleteBranch(selected.clone()),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("checked out")
    );
    assert!(
        git_manage::prepare(&f.root, Action::Sync(selected), &AtomicBool::new(false))
            .unwrap_err()
            .contains("interrupted operation")
    );
    assert!(git(&f.root, &["branch", "--list", "topic"]).contains("topic"));
}

#[test]
fn publishing_does_not_follow_tags_even_when_enabled_in_git_configuration() {
    let f = Fixture::new();
    commit(&f.root, "pending");
    git(&f.root, &["branch", "publish-me"]);
    git(
        &f.root,
        &[
            "-c",
            "tag.gpgSign=false",
            "tag",
            "-a",
            "keep-local",
            "-m",
            "local tag",
        ],
    );
    git(&f.root, &["config", "push.followTags", "true"]);
    f.prepare(Action::Sync(f.branch("publish-me")))
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert!(git(&f.remote, &["tag", "--list"]).is_empty());
    assert_eq!(git(&f.root, &["tag", "--list"]), "keep-local");
}

#[test]
fn branches_under_bisection_are_kept_even_with_a_detached_head() {
    let f = Fixture::new();
    commit(&f.root, "middle");
    commit(&f.root, "latest");
    git(&f.root, &["bisect", "start", "HEAD", "HEAD~2"]);
    let branch = f.branch("main");
    assert!(branch.busy);
    assert!(branch.checkout.is_none());
    assert!(
        git_manage::prepare(
            &f.root,
            Action::DeleteBranch(branch),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("checked out")
    );
    assert!(git(&f.root, &["branch", "--list", "main"]).contains("main"));
}

#[test]
fn reclaim_summary_counts_unique_local_work_across_branches_and_includes_ignored_items() {
    let f = Fixture::new();
    commit(&f.root, "local-only");
    git(&f.root, &["branch", "alias-one"]);
    git(&f.root, &["branch", "alias-two"]);
    fs::write(f.root.join("tracked"), "stash data").unwrap();
    git(&f.root, &["stash", "push", "-m", "saved"]);
    fs::write(f.root.join("tracked"), "staged").unwrap();
    git(&f.root, &["add", "tracked"]);
    fs::write(f.root.join("tracked"), "modified").unwrap();
    fs::write(f.root.join("scratch"), "local file").unwrap();
    fs::write(f.root.join(".git/info/exclude"), "cache/\nlocal.env\n").unwrap();
    fs::create_dir(f.root.join("cache")).unwrap();
    fs::write(f.root.join("cache/output"), "build output").unwrap();
    fs::write(f.root.join("local.env"), "private local settings").unwrap();
    let refs = git(&f.root, &["show-ref"]);
    let index = fs::read(f.root.join(".git/index")).unwrap();
    git(
        &f.root,
        &["remote", "set-url", "origin", "/missing/no-network.git"],
    );
    let summary = f.read(GitCommand::Branches).summary.unwrap().unwrap();
    assert_eq!(
        summary.commits_not_on_remote, 1,
        "Shared commits must not be counted once per branch"
    );
    assert_eq!(summary.branches_without_upstream, 2);
    assert_eq!(summary.stashes, 1);
    assert!(summary.has_remote_refs);
    let files = summary.files.unwrap();
    assert_eq!((files.staged, files.modified, files.untracked), (1, 1, 1));
    assert_eq!(summary.ignored_items, 2);
    assert_eq!(git(&f.root, &["show-ref"]), refs);
    assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), index);
    git(&f.root, &["reset", "--hard", "origin/main"]);
    let summary = f.read(GitCommand::Branches).summary.unwrap().unwrap();
    assert_eq!(
        summary.commits_not_on_remote, 1,
        "Local work on other branches must remain visible"
    );
    assert_eq!(summary.files.unwrap().ahead, Some(0));
}

#[test]
fn reclaim_summary_accounts_for_detached_heads_and_missing_remote_refs() {
    let f = Fixture::new();
    commit(&f.root, "detached-only");
    git(&f.root, &["switch", "--detach"]);
    git(&f.root, &["branch", "-f", "main", "origin/main"]);
    let summary = f.read(GitCommand::LocalCommits).summary.unwrap().unwrap();
    assert_eq!(summary.commits_not_on_remote, 1);
    assert!(summary.has_remote_refs);
    git(&f.root, &["update-ref", "-d", "refs/remotes/origin/main"]);
    let summary = f.read(GitCommand::LocalCommits).summary.unwrap().unwrap();
    assert!(!summary.has_remote_refs);
    assert_eq!(summary.branches_without_upstream, 1);
    let bare = git_manage::read(&f.remote, GitCommand::Branches, &AtomicBool::new(false))
        .unwrap()
        .summary
        .unwrap()
        .unwrap();
    assert!(bare.files.is_none());
    assert_eq!(bare.ignored_items, 0);
    assert!(!bare.has_remote_refs);
}
