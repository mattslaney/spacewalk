use spacewalk::worktree_actions::{self, WorktreeRequest};
use spacewalk::{
    scan::Options,
    worktrees::{self, WorktreeManager, WorktreeRow},
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim_end().into()
}

struct Fixture {
    temp: tempfile::TempDir,
    main: PathBuf,
    linked: PathBuf,
}
impl Fixture {
    fn removal(&self, paths: Vec<PathBuf>, force: bool) -> WorktreeRequest {
        WorktreeRequest::Remove {
            common_dir: self.main.clone(),
            paths,
            force,
            protected: vec![self.main.clone()],
            options: Options {
                apparent_size: true,
            },
        }
    }
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        let linked = temp.path().join("scattered café tree");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--initial-branch=main"]);
        git(&main, &["config", "user.name", "Spacewalk Test"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        fs::write(main.join("data"), b"1234").unwrap();
        git(&main, &["add", "data"]);
        git(&main, &["commit", "-m", "initial"]);
        git(
            &main,
            &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
        );
        Self { temp, main, linked }
    }
}

#[test]
fn discovers_worktrees_outside_the_selected_checkout_without_fetching() {
    let f = Fixture::new();
    fs::create_dir(f.linked.join("nested")).unwrap();
    git(
        &f.main,
        &[
            "remote",
            "add",
            "origin",
            "/missing/spacewalk-no-network.git",
        ],
    );
    let registry = worktrees::discover(&f.linked.join("nested"), &AtomicBool::new(false)).unwrap();
    assert_eq!(registry.common_dir, f.main.join(".git"));
    assert_eq!(registry.records.len(), 2);
    assert!(registry.records[0].main);
    assert_eq!(registry.records[1].path, f.linked);
    assert_eq!(registry.records[1].branch.as_deref(), Some("topic"));
}

#[test]
fn sizes_include_ignored_build_output_but_exclude_shared_metadata() {
    let f = Fixture::new();
    fs::write(f.main.join(".git/info/exclude"), "target/\n").unwrap();
    for (root, count) in [(&f.main, 1024), (&f.linked, 4096)] {
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/output"), vec![0; count]).unwrap();
    }
    fs::write(
        f.main.join(".git/large-shared-fixture"),
        vec![0; 128 * 1024],
    )
    .unwrap();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    for record in &registry.records {
        let row = worktrees::measure(
            record,
            &registry.common_dir,
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
        );
        assert!(row.error.is_none(), "{:?}", row.error);
        assert!(!row.state.unwrap().dirty());
        assert_eq!(row.bytes, Some(if record.main { 1028 } else { 4100 }));
    }
}

#[test]
fn reports_local_changes_locks_missing_paths_and_detached_heads() {
    let f = Fixture::new();
    git(
        &f.main,
        &[
            "worktree",
            "lock",
            "--reason",
            "portable drive",
            f.linked.to_str().unwrap(),
        ],
    );
    fs::write(f.linked.join("data"), "staged").unwrap();
    git(&f.linked, &["add", "data"]);
    fs::write(f.linked.join("data"), "modified").unwrap();
    fs::write(f.linked.join("untracked"), "keep").unwrap();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    let record = &registry.records[1];
    assert_eq!(record.locked.as_deref(), Some("portable drive"));
    let state = worktrees::measure(
        record,
        &registry.common_dir,
        Options::default(),
        &AtomicBool::new(false),
    )
    .state
    .unwrap();
    assert_eq!((state.staged, state.modified, state.untracked), (1, 1, 1));
    fs::remove_dir_all(&f.linked).unwrap();
    assert!(
        worktrees::measure(
            record,
            &registry.common_dir,
            Options::default(),
            &AtomicBool::new(false)
        )
        .error
        .unwrap()
        .contains("missing")
    );
    let detached = f.temp.path().join("detached");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().unwrap(),
            "HEAD",
        ],
    );
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    assert!(
        registry
            .records
            .iter()
            .any(|record| record.path == detached && record.branch.is_none())
    );
}

#[test]
fn supports_bare_hubs_and_nul_delimited_unusual_paths() {
    let f = Fixture::new();
    let hub = f.temp.path().join("hub.git");
    git(
        f.temp.path(),
        &[
            "clone",
            "--bare",
            f.main.to_str().unwrap(),
            hub.to_str().unwrap(),
        ],
    );
    let linked = f.temp.path().join("bare-linked");
    git(
        &hub,
        &[
            "worktree",
            "add",
            "-b",
            "other",
            linked.to_str().unwrap(),
            "main",
        ],
    );
    let registry = worktrees::discover(&hub, &AtomicBool::new(false)).unwrap();
    assert!(registry.records[0].bare);
    assert_eq!(registry.common_dir, hub);
    let mut manager = WorktreeManager::new(hub.clone());
    manager.apply_registry(registry);
    assert_eq!(manager.rows.len(), 1);
    assert_eq!(manager.rows[0].record.path, linked);
    let records = worktrees::parse_list(b"worktree /tmp/one\nwith newline\0HEAD abc\0branch refs/heads/topic\0locked custom\nreason\0\0worktree /tmp/missing\0detached\0prunable missing gitdir\0\0").unwrap();
    assert_eq!(records[0].path, PathBuf::from("/tmp/one\nwith newline"));
    assert_eq!(records[0].locked.as_deref(), Some("custom\nreason"));
    assert!(records[1].prunable.is_some());
}

#[test]
fn sorting_keeps_selection_and_marks_bound_to_paths() {
    let f = Fixture::new();
    let second = f.temp.path().join("second");
    git(
        &f.main,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    let mut manager = WorktreeManager::new(f.linked.clone());
    manager.apply_registry(registry.clone());
    manager.marked.insert(f.linked.clone());
    for (record, bytes) in registry
        .records
        .into_iter()
        .filter(|record| !record.main)
        .zip([500, 100])
    {
        let mut row = WorktreeRow::pending(record);
        row.bytes = Some(bytes);
        manager.update(row);
    }
    assert_eq!(manager.rows[0].record.path, f.linked);
    assert_eq!(manager.selected, f.linked);
    assert!(manager.marked.contains(&f.linked));
    manager.move_selection(1);
    assert_eq!(manager.selected, second);
}

#[cfg(unix)]
#[test]
fn path_parser_preserves_non_unicode_bytes_and_measurement_does_not_follow_symlinks() {
    use std::os::unix::ffi::OsStrExt;
    let records = worktrees::parse_list(b"worktree /tmp/non-utf8-\xff\0HEAD abc\0\0").unwrap();
    assert_eq!(
        records[0].path.as_os_str().as_bytes(),
        b"/tmp/non-utf8-\xff"
    );
    let f = Fixture::new();
    let external = f.temp.path().join("outside-file");
    fs::write(&external, vec![0; 10000]).unwrap();
    std::os::unix::fs::symlink(&external, f.linked.join("link")).unwrap();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    let row = worktrees::measure(
        &registry.records[1],
        &registry.common_dir,
        Options {
            apparent_size: true,
        },
        &AtomicBool::new(false),
    );
    assert_eq!(
        row.bytes,
        Some(4 + fs::symlink_metadata(f.linked.join("link")).unwrap().len())
    );
}

#[test]
fn removes_checkout_and_ignored_output_but_keeps_branch_and_shared_history() {
    let f = Fixture::new();
    fs::write(f.main.join(".git/info/exclude"), "target/\n").unwrap();
    fs::create_dir(f.linked.join("target")).unwrap();
    fs::write(f.linked.join("target/output"), b"build output").unwrap();
    let preview = worktree_actions::prepare(
        f.removal(vec![f.linked.clone()], false),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(preview.text.contains("target/"));
    assert!(f.linked.is_dir());
    let result = preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(result.contains("Removed 1 of 1"));
    assert!(!f.linked.exists());
    assert_eq!(git(&f.main, &["branch", "--list", "topic"]).trim(), "topic");
    assert_eq!(fs::read(f.main.join("data")).unwrap(), b"1234");
    assert!(f.main.join(".git/objects").is_dir());
}

#[test]
fn force_is_required_for_local_changes_and_previews_do_not_show_file_contents() {
    let f = Fixture::new();
    fs::write(f.linked.join("data"), "modified").unwrap();
    fs::write(f.linked.join("untracked"), "untracked bytes").unwrap();
    fs::write(f.main.join(".git/info/exclude"), ".env\n").unwrap();
    fs::write(f.linked.join(".env"), "secret fixture contents").unwrap();
    assert!(
        worktree_actions::prepare(
            f.removal(vec![f.linked.clone()], false),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("Force")
    );
    let preview = worktree_actions::prepare(
        f.removal(vec![f.linked.clone()], true),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(preview.text.contains(".env"));
    assert!(preview.text.contains("untracked"));
    assert!(!preview.text.contains("secret fixture contents"));
    assert!(f.linked.exists());
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(!f.linked.exists());
}

#[test]
fn protects_main_locked_active_and_nested_checkouts_even_with_force() {
    let f = Fixture::new();
    assert!(
        worktree_actions::prepare(
            f.removal(vec![f.main.clone()], true),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("main worktree")
    );
    git(
        &f.main,
        &[
            "worktree",
            "lock",
            "--reason",
            "portable drive",
            f.linked.to_str().unwrap(),
        ],
    );
    assert!(
        worktree_actions::prepare(
            f.removal(vec![f.linked.clone()], true),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("locked")
    );
    git(&f.main, &["worktree", "unlock", f.linked.to_str().unwrap()]);
    let protected = f.linked.join("inside");
    fs::create_dir(&protected).unwrap();
    let request = WorktreeRequest::Remove {
        common_dir: f.main.clone(),
        paths: vec![f.linked.clone()],
        force: true,
        protected: vec![protected],
        options: Options::default(),
    };
    assert!(
        worktree_actions::prepare(request, &AtomicBool::new(false))
            .unwrap_err()
            .contains("scan root or working directory")
    );
    let nested = f.linked.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "--initial-branch=main"]);
    assert!(
        worktree_actions::prepare(
            f.removal(vec![f.linked.clone()], true),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("nested repository")
    );
    assert!(f.linked.exists());
}

#[test]
fn detached_only_commits_need_force_while_referenced_detached_heads_are_removable() {
    let f = Fixture::new();
    let detached = f.temp.path().join("detached");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().unwrap(),
            "HEAD",
        ],
    );
    let normal = worktree_actions::prepare(
        f.removal(vec![detached.clone()], false),
        &AtomicBool::new(false),
    );
    assert!(normal.is_ok());
    git(
        &detached,
        &["commit", "--allow-empty", "-m", "detached-only fixture"],
    );
    let oid = git(&detached, &["rev-parse", "HEAD"]);
    assert!(
        worktree_actions::prepare(
            f.removal(vec![detached.clone()], false),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("detached-only")
    );
    let preview = worktree_actions::prepare(
        f.removal(vec![detached.clone()], true),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(preview.text.contains("detached-only fixture"));
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(!detached.exists());
    assert_eq!(git(&f.main, &["cat-file", "-t", &oid]), "commit");
}

#[test]
fn validates_whole_batches_and_refuses_changes_after_preview() {
    let f = Fixture::new();
    let second = f.temp.path().join("second");
    git(
        &f.main,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    let paths = vec![f.linked.clone(), second.clone()];
    let preview =
        worktree_actions::prepare(f.removal(paths.clone(), false), &AtomicBool::new(false))
            .unwrap();
    fs::write(second.join("new"), "arrived after preview").unwrap();
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("changed since preview")
    );
    assert!(f.linked.exists());
    assert!(second.exists());
    assert!(
        worktree_actions::prepare(f.removal(paths.clone(), false), &AtomicBool::new(false))
            .is_err()
    );
    fs::remove_file(second.join("new")).unwrap();
    let preview =
        worktree_actions::prepare(f.removal(paths, false), &AtomicBool::new(false)).unwrap();
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(!f.linked.exists());
    assert!(!second.exists());
    assert!(!git(&f.main, &["branch", "--list", "topic", "second"]).is_empty());
}

#[test]
fn lock_changes_and_cancelled_requests_never_remove_a_checkout() {
    let f = Fixture::new();
    let preview = worktree_actions::prepare(
        f.removal(vec![f.linked.clone()], false),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        preview
            .execute(&AtomicBool::new(true))
            .unwrap_err()
            .contains("cancelled")
    );
    assert!(f.linked.exists());
    git(&f.main, &["worktree", "lock", f.linked.to_str().unwrap()]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("registrations changed")
    );
    assert!(f.linked.exists());
}

#[test]
fn prune_uses_configured_expiry_and_keeps_locked_records_and_live_directories() {
    let f = Fixture::new();
    let locked = f.temp.path().join("locked");
    git(
        &f.main,
        &["worktree", "add", "-b", "locked", locked.to_str().unwrap()],
    );
    git(
        &f.main,
        &[
            "worktree",
            "lock",
            "--reason",
            "offline disk",
            locked.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&f.linked).unwrap();
    fs::remove_dir_all(&locked).unwrap();
    git(
        &f.main,
        &["config", "gc.worktreePruneExpire", "3.months.ago"],
    );
    let request = WorktreeRequest::Prune {
        common_dir: f.main.clone(),
    };
    let preview = worktree_actions::prepare(request.clone(), &AtomicBool::new(false)).unwrap();
    assert!(preview.text.contains("No records are eligible"));
    git(&f.main, &["config", "gc.worktreePruneExpire", "now"]);
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("eligibility changed")
    );
    let preview = worktree_actions::prepare(request, &AtomicBool::new(false)).unwrap();
    assert!(preview.text.contains(f.linked.to_str().unwrap()));
    preview.execute(&AtomicBool::new(false)).unwrap();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    assert!(!registry.records.iter().any(|row| row.path == f.linked));
    assert!(registry.records.iter().any(|row| row.path == locked));
    assert!(f.main.join("data").exists());
    assert_eq!(git(&f.main, &["branch", "--list", "topic"]).trim(), "topic");
}

#[test]
fn prune_refuses_changed_registrations_and_broken_checkout_never_targets_parent_repo() {
    let f = Fixture::new();
    let preview = worktree_actions::prepare(
        WorktreeRequest::Prune {
            common_dir: f.main.clone(),
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    let nested = f.main.join("nested-checkout");
    git(
        &f.main,
        &["worktree", "add", "-b", "nested", nested.to_str().unwrap()],
    );
    assert!(
        preview
            .execute(&AtomicBool::new(false))
            .unwrap_err()
            .contains("registrations changed")
    );
    fs::remove_file(nested.join(".git")).unwrap();
    assert!(
        worktree_actions::prepare(
            f.removal(vec![nested.clone()], true),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("registered root")
    );
    assert_eq!(fs::read(f.main.join("data")).unwrap(), b"1234");
    assert!(nested.exists());
}

#[cfg(unix)]
#[test]
fn removal_supports_non_unicode_paths() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let path = f
        .temp
        .path()
        .join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec()));
    let output = Command::new("git")
        .arg("-C")
        .arg(&f.main)
        .args(["worktree", "add", "-b", "raw"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let preview = worktree_actions::prepare(
        f.removal(vec![path.clone()], false),
        &AtomicBool::new(false),
    )
    .unwrap();
    preview.execute(&AtomicBool::new(false)).unwrap();
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn reports_partial_batches_and_retains_failed_marks_after_a_filesystem_error() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let parent = f.temp.path().join("read-only-parent");
    fs::create_dir(&parent).unwrap();
    let second = parent.join("second");
    git(
        &f.main,
        &["worktree", "add", "-b", "second", second.to_str().unwrap()],
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = parent.join("write-probe");
    if fs::write(&probe, "probe").is_ok() {
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(probe).unwrap();
        return; // Root or an ACL bypasses mode bits; do not claim a simulated I/O failure.
    }
    let preview = worktree_actions::prepare(
        f.removal(vec![f.linked.clone(), second.clone()], false),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut manager = WorktreeManager::new(f.linked.clone());
    manager.apply_registry(worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap());
    manager.marked.extend([f.linked.clone(), second.clone()]);
    let result = preview.execute(&AtomicBool::new(false));
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    assert!(error.contains("1 of 2"), "{error}");
    assert!(!f.linked.exists());
    assert!(second.exists());
    manager.apply_registry(worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap());
    assert!(!manager.marked.contains(&f.linked));
    assert!(manager.marked.contains(&second));
}

#[test]
fn bare_hub_can_remove_a_linked_checkout_and_removal_keeps_symlink_targets() {
    let f = Fixture::new();
    let hub = f.temp.path().join("hub.git");
    git(
        f.temp.path(),
        &[
            "clone",
            "--bare",
            f.main.to_str().unwrap(),
            hub.to_str().unwrap(),
        ],
    );
    let linked = f.temp.path().join("bare-checkout");
    git(
        &hub,
        &[
            "worktree",
            "add",
            "-b",
            "from-bare",
            linked.to_str().unwrap(),
            "main",
        ],
    );
    let outside = f.temp.path().join("precious");
    fs::write(&outside, "keep me").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, linked.join("link")).unwrap();
    let request = WorktreeRequest::Remove {
        common_dir: hub.clone(),
        paths: vec![linked.clone()],
        force: true,
        protected: vec![],
        options: Options::default(),
    };
    worktree_actions::prepare(request, &AtomicBool::new(false))
        .unwrap()
        .execute(&AtomicBool::new(false))
        .unwrap();
    assert!(!linked.exists());
    assert_eq!(fs::read_to_string(outside).unwrap(), "keep me");
    assert!(hub.join("objects").is_dir());
}

#[cfg(unix)]
#[test]
fn unavailable_directory_contents_are_reported_as_partial_usage() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let private = f.linked.join("private");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("data"), "unreadable").unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();
    let inaccessible = fs::read_dir(&private).is_err();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    let row = worktrees::measure(
        &registry.records[1],
        &registry.common_dir,
        Options::default(),
        &AtomicBool::new(false),
    );
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    if inaccessible {
        assert!(row.scan_errors > 0);
        assert!(row.bytes.is_some());
    }
}

#[test]
fn inventory_worker_keeps_primary_registration_but_does_not_measure_or_list_it() {
    use worktrees::{WorktreeEvent, WorktreeScanTask};
    let f = Fixture::new();
    let task = WorktreeScanTask::start(f.main.clone(), Options::default());
    let mut manager = WorktreeManager::new(f.main.clone());
    let mut measured = Vec::new();
    loop {
        match task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
        {
            WorktreeEvent::Listed(Ok(registry)) => {
                assert!(registry.records.iter().any(|record| record.main));
                manager.apply_registry(registry);
            }
            WorktreeEvent::Listed(Err(error)) => panic!("{error}"),
            WorktreeEvent::Measured(row) => {
                measured.push(row.record.path.clone());
                manager.update(*row);
            }
            WorktreeEvent::Done => break,
        }
    }
    assert_eq!(measured, vec![f.linked.clone()]);
    assert_eq!(manager.rows.len(), 1);
    assert_eq!(manager.selected, f.linked);
}

#[test]
fn sync_icons_require_a_clean_checkout_and_a_verified_remote_upstream() {
    let f = Fixture::new();
    let measure = || {
        let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
        worktrees::measure(
            registry
                .records
                .iter()
                .find(|record| record.path == f.linked)
                .unwrap(),
            &registry.common_dir,
            Options::default(),
            &AtomicBool::new(false),
        )
    };
    assert_eq!(measure().icons(), "?");
    git(&f.linked, &["branch", "--set-upstream-to=main"]);
    let local = measure();
    assert_eq!(local.state.as_ref().unwrap().ahead, Some(0));
    assert!(!local.synced(), "A local branch is not a remote upstream");
    assert_eq!(local.icons(), "?");

    let remote = f.temp.path().join("remote.git");
    git(
        f.temp.path(),
        &[
            "clone",
            "--bare",
            f.main.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(
        &f.main,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&f.linked, &["push", "-u", "origin", "topic"]);
    assert!(measure().synced());
    assert_eq!(measure().icons(), "✓");

    fs::write(f.linked.join("untracked"), b"local work").unwrap();
    assert_eq!(measure().icons(), "*");
    fs::remove_file(f.linked.join("untracked")).unwrap();
    fs::write(f.linked.join("data"), b"local commit").unwrap();
    assert_eq!(measure().icons(), "*");
    git(&f.linked, &["add", "data"]);
    assert_eq!(measure().icons(), "*");
    git(&f.linked, &["commit", "-m", "local"]);
    assert_eq!(measure().icons(), "↑1");

    let writer = f.temp.path().join("writer");
    git(
        f.temp.path(),
        &["clone", remote.to_str().unwrap(), writer.to_str().unwrap()],
    );
    git(&writer, &["config", "user.name", "Test"]);
    git(&writer, &["config", "user.email", "test@example.invalid"]);
    git(&writer, &["switch", "topic"]);
    fs::write(writer.join("data"), b"remote commit").unwrap();
    git(&writer, &["add", "data"]);
    git(&writer, &["commit", "-m", "remote"]);
    git(&writer, &["push"]);
    // Measurement never fetches: the remote change remains unseen until Fetch.
    assert_eq!(measure().icons(), "↑1");
    git(&f.main, &["fetch", "origin"]);
    assert_eq!(measure().icons(), "↑1↓1");
    fs::write(f.linked.join("untracked"), b"local work").unwrap();
    assert_eq!(measure().icons(), "*↑1↓1");
    fs::remove_file(f.linked.join("untracked")).unwrap();
    git(&f.linked, &["reset", "--hard", "origin/topic"]);
    assert_eq!(measure().icons(), "✓");
    fs::write(writer.join("data"), b"another remote commit").unwrap();
    git(&writer, &["commit", "-am", "remote again"]);
    git(&writer, &["push"]);
    git(&f.main, &["fetch", "origin"]);
    assert_eq!(measure().icons(), "↓1");
    git(&f.main, &["update-ref", "-d", "refs/remotes/origin/topic"]);
    assert_eq!(measure().icons(), "?");
    git(&f.linked, &["switch", "--detach"]);
    assert_eq!(measure().icons(), "◇");
    let record = measure().record;
    assert_eq!(WorktreeRow::pending(record.clone()).icons(), "…");
    fs::remove_dir_all(&f.linked).unwrap();
    let missing = worktrees::measure(
        &record,
        &f.main.join(".git"),
        Options::default(),
        &AtomicBool::new(false),
    );
    assert!(!missing.synced());
    assert_eq!(missing.icons(), "?");
}
