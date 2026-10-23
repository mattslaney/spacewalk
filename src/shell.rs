use crate::scan::{Kind, NodeId, Snapshot};
use std::{
    env,
    ffi::OsStr,
    io,
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
};

#[derive(Debug)]
pub struct ShellRequest {
    pub directory: PathBuf,
}

impl ShellRequest {
    pub fn new(data: &Snapshot, selected: NodeId) -> Self {
        let node = &data.nodes[selected];
        let directory = if node.kind == Kind::Directory {
            selected
        } else {
            node.parent.unwrap_or(0)
        };
        Self {
            directory: data.path(directory),
        }
    }

    pub fn run(&self) -> io::Result<ExitStatus> {
        let (variable, fallback) = if cfg!(windows) {
            ("COMSPEC", "cmd.exe")
        } else {
            ("SHELL", "/bin/sh")
        };
        let program = env::var_os(variable)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.into());
        self.command(&program).status()
    }

    fn command(&self, program: &OsStr) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.directory)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if cfg!(unix) {
            command.arg("-i");
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn directories_open_in_place_and_files_use_their_parent() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("spaces café 'quoted' $(literal)");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file"), b"fixture").unwrap();
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        for path in [&sub, &sub.join("file")] {
            let request = ShellRequest::new(&data, data.find_path(path).unwrap());
            assert_eq!(request.directory, sub);
            let command = request.command(OsStr::new("test-shell"));
            assert_eq!(command.get_program(), "test-shell");
            assert_eq!(command.get_current_dir(), Some(sub.as_path()));
        }
        assert_eq!(ShellRequest::new(&data, 0).directory, dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn selecting_a_symbolic_link_opens_its_parent_without_following_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let data = scan(
            dir.path(),
            Options::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let request = ShellRequest::new(&data, data.find_path(&link).unwrap());
        assert_eq!(request.directory, dir.path());
    }
}
