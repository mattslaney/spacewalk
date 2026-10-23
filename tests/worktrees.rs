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
    let records = worktrees::parse_list(b"worktree /tmp/one\nwith newline\0HEAD abc\0branch refs/heads/topic\0locked custom\nreason\0\0worktree /tmp/missing\0detached\0prunable missing gitdir\0\0").unwrap();
    assert_eq!(records[0].path, PathBuf::from("/tmp/one\nwith newline"));
    assert_eq!(records[0].locked.as_deref(), Some("custom\nreason"));
    assert!(records[1].prunable.is_some());
}

#[test]
fn sorting_keeps_selection_and_marks_bound_to_paths() {
    let f = Fixture::new();
    let registry = worktrees::discover(&f.main, &AtomicBool::new(false)).unwrap();
    let mut manager = WorktreeManager::new(f.linked.clone());
    manager.apply_registry(registry.clone());
    manager.marked.insert(f.linked.clone());
    for (record, bytes) in registry.records.into_iter().zip([100, 500]) {
        let mut row = WorktreeRow::pending(record);
        row.bytes = Some(bytes);
        manager.update(row);
    }
    assert_eq!(manager.rows[0].record.path, f.linked);
    assert_eq!(manager.selected, f.linked);
    assert!(manager.marked.contains(&f.linked));
    manager.move_selection(1);
    assert_eq!(manager.selected, f.main);
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
