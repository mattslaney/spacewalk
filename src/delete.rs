use crate::scan::{Identity, Kind, NodeId, Snapshot, kind};
use std::{collections::HashSet, fs, io, path::PathBuf};

#[derive(Debug)]
pub struct DeleteRequest {
    pub path: PathBuf,
    pub size: u64,
    pub files: u64,
    pub kind: Kind,
    chain: Vec<(PathBuf, Identity, Kind)>,
}

impl DeleteRequest {
    pub fn new(data: &Snapshot, id: NodeId) -> io::Result<Self> {
        if id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The scan root cannot be deleted",
            ));
        }
        let node = &data.nodes[id];
        let mut chain = Vec::new();
        let mut current = Some(id);
        while let Some(id) = current {
            let node = &data.nodes[id];
            chain.push((data.path(id), node.identity.clone(), node.kind));
            current = node.parent;
        }
        chain.reverse();
        Ok(Self {
            path: data.path(id),
            size: node.size,
            files: node.files,
            kind: node.kind,
            chain,
        })
    }

    /// Recheck the target and its ancestors before deleting. A path replaced or
    /// redirected since the scan must be rescanned and confirmed again.
    fn validate(&self) -> io::Result<()> {
        for (path, identity, expected_kind) in &self.chain {
            let meta = fs::symlink_metadata(path)?;
            if Identity::of(&meta) != *identity || kind(&meta) != *expected_kind {
                return Err(io::Error::other(
                    "The selected item or an ancestor changed since scanning; rescan and try again",
                ));
            }
        }
        Ok(())
    }

    pub fn execute(self) -> io::Result<()> {
        self.validate()?;
        if self.kind == Kind::Directory {
            fs::remove_dir_all(self.path)
        } else {
            fs::remove_file(self.path)
        }
    }
}

#[derive(Debug)]
pub struct DeleteBatch {
    pub items: Vec<DeleteRequest>,
    pub size: u64,
    pub files: u64,
}

impl DeleteBatch {
    pub fn new(data: &Snapshot, ids: impl IntoIterator<Item = NodeId>) -> io::Result<Self> {
        let marked: HashSet<_> = ids.into_iter().collect();
        if marked.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The scan root cannot be deleted",
            ));
        }
        let mut ids: Vec<_> = marked.iter().copied().collect();
        ids.sort_unstable();
        let mut items = Vec::new();
        for id in ids {
            let mut parent = data.nodes[id].parent;
            let mut covered = false;
            while let Some(id) = parent {
                if marked.contains(&id) {
                    covered = true;
                    break;
                }
                parent = data.nodes[id].parent;
            }
            if !covered {
                items.push(DeleteRequest::new(data, id)?);
            }
        }
        if items.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "No items selected for deletion",
            ));
        }
        let size = items
            .iter()
            .fold(0_u64, |total, item| total.saturating_add(item.size));
        let files = items
            .iter()
            .fold(0_u64, |total, item| total.saturating_add(item.files));
        Ok(Self { items, size, files })
    }

    pub fn execute(self) -> io::Result<()> {
        // Validate every target before starting, then recheck each immediately
        // before removing it. An invalid later target must not start a batch.
        for item in &self.items {
            item.validate().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Cannot delete {}: {error}",
                        crate::display_name(item.path.as_os_str())
                    ),
                )
            })?;
        }
        let total = self.items.len();
        for (deleted, item) in self.items.into_iter().enumerate() {
            let path = item.path.clone();
            if let Err(error) = item.execute() {
                return Err(io::Error::other(format!(
                    "Deleted {deleted} of {total} items; failed at {}: {error}",
                    crate::display_name(path.as_os_str())
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::sync::atomic::AtomicBool;

    fn snapshot(path: &std::path::Path) -> Snapshot {
        scan(
            path,
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
    }

    #[test]
    fn batches_deduplicate_marked_descendants_and_delete_only_the_confirmed_targets() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub/nested")).unwrap();
        fs::write(dir.path().join("sub/nested/file"), b"12345").unwrap();
        fs::write(dir.path().join("other"), b"123").unwrap();
        fs::write(dir.path().join("keep"), b"keep").unwrap();
        let data = snapshot(dir.path());
        let sub = data.find_path(&dir.path().join("sub")).unwrap();
        let child = data.find_path(&dir.path().join("sub/nested/file")).unwrap();
        let other = data.find_path(&dir.path().join("other")).unwrap();
        let batch = DeleteBatch::new(&data, [child, sub, other, other]).unwrap();
        assert_eq!(batch.items.len(), 2);
        assert_eq!(batch.size, 8);
        assert_eq!(batch.files, 2);
        batch.execute().unwrap();
        assert!(!dir.path().join("sub").exists());
        assert!(!dir.path().join("other").exists());
        assert_eq!(fs::read(dir.path().join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn batches_validate_every_target_before_deleting_anything() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), b"keep first").unwrap();
        fs::write(dir.path().join("b"), b"keep second").unwrap();
        let data = snapshot(dir.path());
        let a = data.find_path(&dir.path().join("a")).unwrap();
        let b = data.find_path(&dir.path().join("b")).unwrap();
        let batch = DeleteBatch::new(&data, [a, b]).unwrap();
        fs::rename(dir.path().join("b"), dir.path().join("saved-b")).unwrap();
        fs::write(dir.path().join("b"), b"replacement").unwrap();
        assert!(batch.execute().is_err());
        assert_eq!(fs::read(dir.path().join("a")).unwrap(), b"keep first");
        assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"replacement");
        assert_eq!(
            fs::read(dir.path().join("saved-b")).unwrap(),
            b"keep second"
        );
        assert!(DeleteBatch::new(&data, [a, 0]).is_err());
        assert!(DeleteBatch::new(&data, []).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn batch_deletion_keeps_symbolic_link_targets() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        fs::write(dir.path().join("remove"), b"remove").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let data = snapshot(dir.path());
        let link = data.find_path(&dir.path().join("link")).unwrap();
        let remove = data.find_path(&dir.path().join("remove")).unwrap();
        DeleteBatch::new(&data, [link, remove])
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(dir.path().join("link")).is_err());
        assert!(!dir.path().join("remove").exists());
    }

    #[test]
    fn protects_root_and_deletes_only_selected_subtree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("remove/nested")).unwrap();
        fs::write(dir.path().join("remove/nested/file"), b"delete me").unwrap();
        fs::write(dir.path().join("keep"), b"keep me").unwrap();
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(DeleteRequest::new(&data, 0).is_err());
        let id = data.find_path(&dir.path().join("remove")).unwrap();
        DeleteRequest::new(&data, id).unwrap().execute().unwrap();
        assert!(!dir.path().join("remove").exists());
        assert!(dir.path().join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn deleting_symlink_preserves_external_target() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let id = data.find_path(&dir.path().join("link")).unwrap();
        DeleteRequest::new(&data, id).unwrap().execute().unwrap();
        assert!(outside.path().join("keep").exists());
        assert!(fs::symlink_metadata(dir.path().join("link")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_an_ancestor_replaced_with_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/file"), b"keep").unwrap();
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let req = DeleteRequest::new(&data, data.find_path(&dir.path().join("sub/file")).unwrap())
            .unwrap();
        fs::rename(dir.path().join("sub"), dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("moved"), dir.path().join("sub")).unwrap();
        assert!(req.execute().is_err());
        assert!(dir.path().join("moved/file").exists());
    }
}
