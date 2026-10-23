use spacewalk::{
    files::{Directory, FileEvent, FileItem, FileRequest, FileTask, TransferMode, TransferQueue},
    scan::{self, Options},
};
use std::{fs, path::Path, sync::atomic::AtomicBool, time::Duration};

fn queue(root: &Path, paths: &[&str], mode: TransferMode) -> TransferQueue {
    let data = scan::scan(root, Options::default(), &AtomicBool::new(false), |_| {}).unwrap();
    TransferQueue::new(
        &data,
        paths
            .iter()
            .map(|path| data.find_path(&root.join(path)).unwrap()),
        mode,
    )
    .unwrap()
}

fn paste(queue: TransferQueue, destination: &Path) -> spacewalk::files::FileOutcome {
    FileRequest::Paste {
        queue,
        destination: Directory::capture(destination).unwrap(),
    }
    .execute(&AtomicBool::new(false), |_| {})
}

#[test]
fn recursive_copy_deduplicates_marks_and_can_be_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("source/nested")).unwrap();
    fs::write(
        root.join("source/nested/café 'quoted' $(literal)"),
        b"contents",
    )
    .unwrap();
    fs::create_dir(root.join("one")).unwrap();
    fs::create_dir(root.join("two")).unwrap();
    let queue = queue(root, &["source", "source/nested"], TransferMode::Copy);
    assert_eq!(queue.items.len(), 1);
    for destination in ["one", "two"] {
        let result = paste(queue.clone(), &root.join(destination));
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(
            fs::read(
                root.join(destination)
                    .join("source/nested/café 'quoted' $(literal)")
            )
            .unwrap(),
            b"contents"
        );
    }
    assert!(root.join("source/nested").exists());
}

#[test]
fn collisions_are_preflighted_before_any_items_change() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("one"), b"source1").unwrap();
    fs::write(root.join("two"), b"source2").unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    fs::write(root.join("dest/two"), b"keep").unwrap();
    let result = paste(
        queue(root, &["one", "two"], TransferMode::Move),
        &root.join("dest"),
    );
    assert!(result.error.unwrap().contains("already exists"));
    assert!(!root.join("dest/one").exists());
    assert!(root.join("one").exists());
    assert_eq!(fs::read(root.join("dest/two")).unwrap(), b"keep");
}

#[test]
fn moves_files_and_directories_and_never_moves_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("source")).unwrap();
    fs::write(root.join("source/file"), b"contents").unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let result = paste(
        queue(root, &["source"], TransferMode::Move),
        &root.join("dest"),
    );
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(!root.join("source").exists());
    assert!(root.join("dest/source/file").exists());
    let data = scan::scan(root, Options::default(), &AtomicBool::new(false), |_| {}).unwrap();
    assert!(TransferQueue::new(&data, [0], TransferMode::Move).is_err());
}

#[test]
fn refuses_self_descendants_and_replaced_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("source/nested")).unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let queued = queue(root, &["source"], TransferMode::Copy);
    assert!(
        paste(queued.clone(), &root.join("source/nested"))
            .error
            .unwrap()
            .contains("descendant")
    );
    fs::rename(root.join("source"), root.join("old")).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    assert!(
        paste(queued, &root.join("dest"))
            .error
            .unwrap()
            .contains("changed")
    );
    assert!(!root.join("dest/source").exists());
}

#[test]
fn create_and_rename_validate_names_and_refuse_existing_targets() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "one/two",
        "bad\\name",
    ] {
        let result = FileRequest::Create {
            parent: Directory::capture(root).unwrap(),
            name: name.into(),
        }
        .execute(&AtomicBool::new(false), |_| {});
        assert!(result.error.is_some(), "{name}");
    }
    let result = FileRequest::Create {
        parent: Directory::capture(root).unwrap(),
        name: "new café".into(),
    }
    .execute(&AtomicBool::new(false), |_| {});
    assert!(result.error.is_none());
    let data = scan::scan(root, Options::default(), &AtomicBool::new(false), |_| {}).unwrap();
    let item =
        FileItem::from_snapshot(&data, data.find_path(&root.join("new café")).unwrap()).unwrap();
    fs::create_dir(root.join("existing")).unwrap();
    let request = FileRequest::Rename {
        item,
        parent: Directory::capture(root).unwrap(),
        name: "existing".into(),
    };
    assert!(
        request
            .clone()
            .execute(&AtomicBool::new(false), |_| {})
            .error
            .is_some()
    );
    if let FileRequest::Rename { item, parent, .. } = request {
        let result = FileRequest::Rename {
            item,
            parent,
            name: "renamed".into(),
        }
        .execute(&AtomicBool::new(false), |_| {});
        assert!(result.error.is_none(), "{:?}", result.error);
    }
    assert!(root.join("renamed").is_dir());
    assert!(!root.join("new café").exists());
}

#[test]
fn cancellation_removes_staging_and_preserves_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("source"), vec![123; 2 * 1024 * 1024]).unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let request = FileRequest::Paste {
        queue: queue(root, &["source"], TransferMode::Copy),
        destination: Directory::capture(&root.join("dest")).unwrap(),
    };
    let cancel = AtomicBool::new(false);
    let result = request.execute(&cancel, |text| {
        if text.starts_with("Copying") {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    assert!(result.error.unwrap().contains("Cancelled"));
    assert!(root.join("source").exists());
    assert_eq!(fs::read_dir(root.join("dest")).unwrap().count(), 0);
}

#[test]
fn worker_reports_completion_and_partial_batches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("one"), b"one").unwrap();
    fs::write(root.join("two"), b"two").unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let request = FileRequest::Paste {
        queue: queue(root, &["one", "two"], TransferMode::Copy),
        destination: Directory::capture(&root.join("dest")).unwrap(),
    };
    let task = FileTask::start(request);
    loop {
        if let FileEvent::Finished(result) =
            task.receiver.recv_timeout(Duration::from_secs(3)).unwrap()
        {
            assert_eq!(result.completed.len(), 2);
            assert!(result.error.is_none());
            break;
        }
    }
}

#[test]
fn modified_file_after_queue_is_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("file"), b"one").unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let queued = queue(root, &["file"], TransferMode::Copy);
    fs::write(root.join("file"), b"changed").unwrap();
    assert!(
        paste(queued, &root.join("dest"))
            .error
            .unwrap()
            .contains("changed")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_copied_as_links_and_destination_replacement_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("keep"), b"outside").unwrap();
    std::os::unix::fs::symlink(external.path(), root.join("link")).unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    let result = paste(
        queue(root, &["link"], TransferMode::Copy),
        &root.join("dest"),
    );
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(
        fs::symlink_metadata(root.join("dest/link"))
            .unwrap()
            .is_symlink()
    );
    let request = FileRequest::Create {
        parent: Directory::capture(&root.join("dest")).unwrap(),
        name: "bad".into(),
    };
    fs::rename(root.join("dest"), root.join("old")).unwrap();
    std::os::unix::fs::symlink(external.path(), root.join("dest")).unwrap();
    assert!(
        request
            .execute(&AtomicBool::new(false), |_| {})
            .error
            .is_some()
    );
    assert!(!external.path().join("bad").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cross_device_moves_preserve_data_and_sparse_copies_stay_sparse() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let Ok(other) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    if root.metadata().unwrap().dev() == other.path().metadata().unwrap().dev() {
        return;
    }
    fs::create_dir(root.join("source")).unwrap();
    fs::write(root.join("source/file"), b"contents").unwrap();
    let result = paste(queue(root, &["source"], TransferMode::Move), other.path());
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(!root.join("source").exists());
    assert_eq!(
        fs::read(other.path().join("source/file")).unwrap(),
        b"contents"
    );
    let sparse = fs::File::create(root.join("sparse")).unwrap();
    sparse.set_len(8 * 1024 * 1024).unwrap();
    fs::create_dir(root.join("dest")).unwrap();
    assert!(
        paste(
            queue(root, &["sparse"], TransferMode::Copy),
            &root.join("dest")
        )
        .error
        .is_none()
    );
    let metadata = root.join("dest/sparse").metadata().unwrap();
    assert_eq!(metadata.len(), 8 * 1024 * 1024);
    assert!(metadata.blocks() * 512 < metadata.len());
}
