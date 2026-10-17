use crate::scan::{Identity, Kind, NodeId, Snapshot, kind};
use std::{fs, io, path::PathBuf};

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
    pub fn execute(self) -> io::Result<()> {
        for (path, identity, expected_kind) in &self.chain {
            let meta = fs::symlink_metadata(path)?;
            if Identity::of(&meta) != *identity || kind(&meta) != *expected_kind {
                return Err(io::Error::other(
                    "The selected item or an ancestor changed since scanning; rescan and try again",
                ));
            }
        }
        if self.kind == Kind::Directory {
            fs::remove_dir_all(self.path)
        } else {
            fs::remove_file(self.path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::sync::atomic::AtomicBool;

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
