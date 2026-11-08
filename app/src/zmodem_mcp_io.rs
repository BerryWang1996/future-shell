//! Local IO for MCP terminal transfers. No protocol, task, or GUI policy lives here.
use fs_sshengine::sandbox;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// Only the application's own default root is provisioned. Explicit user roots
/// retain their existing validation/permission semantics.
pub(crate) fn prepare_default_download_root(
    root: &Path,
    data_dir: &Path,
    profile_id: &str,
) -> Result<PathBuf, String> {
    if root != data_dir.join("downloads").join(profile_id) {
        return Ok(root.to_path_buf());
    }
    uuid::Uuid::parse_str(profile_id).map_err(|_| "invalid profile id")?;
    let mut directory = data_dir.canonicalize().map_err(|e| e.to_string())?;
    for component in ["downloads", profile_id] {
        directory.push(component);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(format!("create default download directory: {e}")),
        }
        let metadata = std::fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("default download root contains a link or non-directory".into());
        }
    }
    Ok(directory)
}

fn validate_basename(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name != name.trim()
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err("invalid protocol basename".into());
    }
    Ok(())
}

pub(crate) struct UploadSource {
    file: File,
    basename: String,
    size: u64,
    failure: Option<String>,
}

impl UploadSource {
    pub(crate) fn new(file: File, basename: String, captured_size: u64) -> Result<Self, String> {
        validate_basename(&basename)?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("source metadata: {e}"))?;
        if !metadata.is_file() || metadata.len() != captured_size {
            return Err("source must be a regular file with the captured size".into());
        }
        Ok(Self {
            file,
            basename,
            size: captured_size,
            failure: None,
        })
    }
    pub(crate) fn basename(&self) -> &str {
        &self.basename
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    /// Always read the opened source, even if its original path has been replaced.
    pub(crate) fn read_chunk(&mut self, offset: u64, max_len: usize) -> Result<Vec<u8>, String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let result: Result<Vec<u8>, String> = (|| {
            if offset > self.size || max_len == 0 {
                return Err("invalid source read range".into());
            }
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| format!("source seek: {e}"))?;
            let len = (self.size - offset).min(max_len.min(64 * 1024) as u64) as usize;
            let mut bytes = vec![0; len];
            self.file
                .read_exact(&mut bytes)
                .map_err(|e| format!("source read: {e}"))?;
            Ok(bytes)
        })();
        if let Err(e) = &result {
            self.failure = Some(e.clone());
        }
        result
    }
}

pub(crate) struct DownloadSink {
    file: File,
    root: PathBuf,
    destination: PathBuf,
    partial: PathBuf,
    relative: PathBuf,
    bytes: u64,
    failure: Option<String>,
    committed: bool,
}

impl DownloadSink {
    pub(crate) fn new(
        root: &Path,
        protocol_basename: &str,
        local_basename: &str,
    ) -> Result<Self, String> {
        validate_basename(protocol_basename)?;
        if local_basename != sandbox::escape_local_component(protocol_basename) {
            return Err("local basename does not match safe protocol basename".into());
        }
        let canonical = root
            .canonicalize()
            .map_err(|e| format!("sandbox root: {e}"))?;
        if canonical != root || !canonical.is_dir() {
            return Err("sandbox root must be canonical directory".into());
        }
        let relative = PathBuf::from(local_basename);
        let destination =
            sandbox::resolve_within(root, &root.join(&relative)).map_err(|e| e.to_string())?;
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => return Err("destination already exists".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("destination metadata: {e}")),
        }
        let partial = sandbox::prepare_destination(
            root,
            &PathBuf::from(".incoming").join(format!("mcp-{}.part", uuid::Uuid::new_v4())),
        )
        .map_err(|e| e.to_string())?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&partial)
            .map_err(|e| format!("create partial: {e}"))?;
        Ok(Self {
            file,
            root: canonical,
            destination,
            partial,
            relative,
            bytes: 0,
            failure: None,
            committed: false,
        })
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn partial_path(&self) -> &Path {
        &self.partial
    }
    pub(crate) fn local_relative_path(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn write_chunk(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let result: Result<(), String> = (|| {
            if self.committed {
                return Err("download already committed".into());
            }
            if bytes.len() as u64 > fs_terminal::zsession::MAX_RECV_BYTES.saturating_sub(self.bytes)
            {
                return Err("download exceeds actual byte limit".into());
            }
            while !bytes.is_empty() {
                match self.file.write(bytes) {
                    Ok(0) => return Err("partial write returned zero".into()),
                    Ok(n) => {
                        self.bytes += n as u64;
                        bytes = &bytes[n..];
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(format!("partial write: {e}")),
                }
            }
            Ok(())
        })();
        if let Err(e) = &result {
            self.failure = Some(e.clone());
        }
        result
    }
    /// Call only after protocol completion. No overwrite or copy fallback is allowed.
    /// Unsupported hard links (including cross-filesystem moves) fail with partial retained.
    pub(crate) fn finish(&mut self, expected_protocol_bytes: u64) -> Result<(), String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let result: Result<(), String> = (|| {
            if self.committed {
                return Err("download already committed".into());
            }
            self.file
                .flush()
                .map_err(|e| format!("partial flush: {e}"))?;
            self.file
                .sync_all()
                .map_err(|e| format!("partial sync: {e}"))?;
            let metadata = self
                .file
                .metadata()
                .map_err(|e| format!("partial metadata: {e}"))?;
            if !metadata.is_file()
                || metadata.len() != self.bytes
                || self.bytes != expected_protocol_bytes
            {
                return Err("download byte count mismatch".into());
            }
            if self
                .root
                .canonicalize()
                .map_err(|e| format!("sandbox root: {e}"))?
                != self.root
            {
                return Err("sandbox root was replaced".into());
            }
            let resolved =
                sandbox::resolve_within(&self.root, &self.partial).map_err(|e| e.to_string())?;
            let on_disk = std::fs::symlink_metadata(&resolved)
                .map_err(|e| format!("partial path metadata: {e}"))?;
            if !on_disk.is_file() || on_disk.file_type().is_symlink() {
                return Err("partial path is not regular file".into());
            }
            let opened = same_file::Handle::from_file(
                self.file
                    .try_clone()
                    .map_err(|e| format!("partial handle: {e}"))?,
            )
            .map_err(|e| format!("partial identity: {e}"))?;
            let current = same_file::Handle::from_path(&resolved)
                .map_err(|e| format!("partial path identity: {e}"))?;
            if opened != current {
                return Err("partial path was replaced".into());
            }
            let dest = sandbox::resolve_within(&self.root, &self.destination)
                .map_err(|e| e.to_string())?;
            std::fs::hard_link(&resolved, &dest).map_err(|e| format!("no-replace commit: {e}"))?;
            std::fs::remove_file(&resolved)
                .map_err(|e| format!("commit partial cleanup (destination exists): {e}"))?;
            self.committed = true;
            Ok(())
        })();
        if let Err(e) = &result {
            self.failure = Some(e.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }
    #[test]
    fn default_download_root_is_created_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let id = "00000000-0000-0000-0000-000000000001";
        let root = dir.path().join("downloads").join(id);
        let prepared = prepare_default_download_root(&root, dir.path(), id).unwrap();
        assert!(prepared.is_dir());
        assert!(DownloadSink::new(&prepared, "new", "new").is_ok());
        assert_eq!(
            prepare_default_download_root(&root, dir.path(), id).unwrap(),
            prepared
        );
        let user = dir.path().join("user-not-created");
        assert_eq!(
            prepare_default_download_root(&user, dir.path(), id).unwrap(),
            user
        );
        assert!(!user.exists());
    }

    #[cfg(unix)]
    #[test]
    fn default_download_root_rejects_symlink_parent() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("downloads")).unwrap();
        let id = "00000000-0000-0000-0000-000000000001";
        assert!(prepare_default_download_root(
            &dir.path().join("downloads").join(id),
            dir.path(),
            id
        )
        .is_err());
        assert!(!outside.path().join(id).exists());
    }

    #[test]
    fn upload_reads_open_handle_after_path_replacement_and_bounds_growth() {
        let (_dir, root) = root();
        let path = root.join("source");
        std::fs::write(&path, b"original").unwrap();
        let mut src = UploadSource::new(File::open(&path).unwrap(), "source".into(), 8).unwrap();
        std::fs::rename(&path, root.join("old")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert_eq!(src.basename(), "source");
        assert_eq!(src.size(), 8);
        assert_eq!(src.read_chunk(0, usize::MAX).unwrap(), b"original");
        OpenOptions::new()
            .append(true)
            .open(root.join("old"))
            .unwrap()
            .write_all(b"growth")
            .unwrap();
        assert_eq!(src.read_chunk(6, 100).unwrap(), b"al");
        assert!(src.read_chunk(8, 1).unwrap().is_empty());
    }
    #[test]
    fn upload_size_directory_short_read_and_range_fail_closed() {
        let (_dir, root) = root();
        let path = root.join("source");
        std::fs::write(&path, vec![7; 70000]).unwrap();
        assert!(UploadSource::new(File::open(&path).unwrap(), "source".into(), 1).is_err());
        #[cfg(unix)]
        assert!(UploadSource::new(File::open(&root).unwrap(), "dir".into(), 0).is_err());
        let mut src =
            UploadSource::new(File::open(&path).unwrap(), "source".into(), 70000).unwrap();
        assert_eq!(src.read_chunk(0, usize::MAX).unwrap().len(), 65536);
        std::fs::write(&path, b"x").unwrap();
        assert!(src.read_chunk(0, 10).is_err());
        assert!(src.read_chunk(0, 1).is_err(), "short source remains failed");
        let mut src = UploadSource::new(File::open(&path).unwrap(), "source".into(), 1).unwrap();
        assert!(src.read_chunk(2, 1).is_err());
    }
    #[test]
    fn zero_and_binary_downloads_commit_exactly_once() {
        let (_dir, root) = root();
        for (name, payload) in [
            ("zero", vec![]),
            ("binary:%.bin", vec![0, 255, 13, 10, 0, 200]),
        ] {
            let local = sandbox::escape_local_component(name);
            let mut sink = DownloadSink::new(&root, name, &local).unwrap();
            let partial = sink.partial_path().to_path_buf();
            sink.write_chunk(&payload).unwrap();
            assert_eq!(sink.bytes(), payload.len() as u64);
            assert_eq!(sink.local_relative_path(), Path::new(&local));
            sink.finish(payload.len() as u64).unwrap();
            assert_eq!(std::fs::read(root.join(local)).unwrap(), payload);
            assert!(!partial.exists());
            assert!(sink.finish(payload.len() as u64).is_err());
        }
    }
    #[test]
    fn commit_conflict_preserves_sentinel_and_partial_and_sticks() {
        let (_dir, root) = root();
        let mut sink = DownloadSink::new(&root, "target", "target").unwrap();
        sink.write_chunk(b"download").unwrap();
        std::fs::write(root.join("target"), b"sentinel").unwrap();
        assert!(sink.finish(8).is_err());
        assert_eq!(std::fs::read(root.join("target")).unwrap(), b"sentinel");
        assert_eq!(std::fs::read(sink.partial_path()).unwrap(), b"download");
        std::fs::remove_file(root.join("target")).unwrap();
        assert!(sink.finish(8).is_err());
        assert!(!root.join("target").exists());
    }
    #[test]
    fn mismatched_counts_and_limit_leave_recoverable_partial() {
        let (_dir, root) = root();
        let mut sink = DownloadSink::new(&root, "target", "target").unwrap();
        sink.write_chunk(b"download").unwrap();
        assert!(sink.finish(7).is_err());
        assert_eq!(std::fs::read(sink.partial_path()).unwrap(), b"download");
        assert!(sink.finish(8).is_err());
        let mut sink = DownloadSink::new(&root, "limit", "limit").unwrap();
        sink.file
            .set_len(fs_terminal::zsession::MAX_RECV_BYTES)
            .unwrap();
        sink.bytes = fs_terminal::zsession::MAX_RECV_BYTES;
        assert!(sink.write_chunk(b"x").is_err());
        assert_eq!(
            sink.file.metadata().unwrap().len(),
            fs_terminal::zsession::MAX_RECV_BYTES
        );
        assert!(sink.partial_path().exists());
    }
    #[test]
    fn cancellation_drop_and_real_write_error_preserve_partial() {
        let (_dir, root) = root();
        let mut sink = DownloadSink::new(&root, "cancel", "cancel").unwrap();
        sink.write_chunk(b"recover me").unwrap();
        let partial = sink.partial_path().to_path_buf();
        drop(sink);
        assert_eq!(std::fs::read(partial).unwrap(), b"recover me");
        assert!(!root.join("cancel").exists());
        let mut sink = DownloadSink::new(&root, "write", "write").unwrap();
        sink.file = File::open(sink.partial_path()).unwrap();
        assert!(sink.write_chunk(b"cannot write").is_err());
        assert_eq!(sink.bytes(), 0);
        assert!(sink.finish(0).is_err());
        assert!(sink.partial_path().exists());
        assert!(!root.join("write").exists());
    }
    #[test]
    fn invalid_names_existing_directories_and_partial_replacement_are_rejected() {
        let (_dir, root) = root();
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            " name",
            "bad\0name",
        ] {
            assert!(
                DownloadSink::new(&root, name, &sandbox::escape_local_component(name)).is_err()
            );
        }
        assert!(DownloadSink::new(&root, "safe", "../escape").is_err());
        std::fs::create_dir(root.join("directory")).unwrap();
        assert!(DownloadSink::new(&root, "directory", "directory").is_err());
        let mut sink = DownloadSink::new(&root, "target", "target").unwrap();
        sink.write_chunk(b"original").unwrap();
        let held = root.join("held");
        std::fs::rename(sink.partial_path(), &held).unwrap();
        std::fs::write(sink.partial_path(), b"replacement").unwrap();
        assert!(sink.finish(8).is_err());
        assert!(!root.join("target").exists());
        assert_eq!(std::fs::read(held).unwrap(), b"original");
    }
    #[cfg(unix)]
    #[test]
    fn symlink_incoming_leaf_and_actual_sync_failure_do_not_commit() {
        use std::os::{
            fd::OwnedFd,
            unix::{fs::symlink, net::UnixStream},
        };
        let (_dir, root) = root();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join(".incoming")).unwrap();
        assert!(DownloadSink::new(&root, "target", "target").is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        std::fs::remove_file(root.join(".incoming")).unwrap();
        let mut sink = DownloadSink::new(&root, "target", "target").unwrap();
        sink.write_chunk(b"original").unwrap();
        std::fs::rename(sink.partial_path(), root.join("held")).unwrap();
        symlink(root.join("held"), sink.partial_path()).unwrap();
        assert!(sink.finish(8).is_err());
        assert!(!root.join("target").exists());
        let mut sink = DownloadSink::new(&root, "sync", "sync").unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        let fd: OwnedFd = socket.into();
        sink.file = File::from(fd);
        // A real socket FD cannot sync to disk; the original partial stays recoverable.
        assert!(sink.finish(0).unwrap_err().contains("partial sync"));
        assert!(sink.partial_path().exists());
        assert!(sink.finish(0).is_err());
    }
}
