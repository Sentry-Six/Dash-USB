//! Upload-only write confinement and atomic publication. Read APIs intentionally
//! follow recording links; uploads must never follow them when writing.
use std::{ffi::CString, fs::File, io::{self, Read, Seek, Write}, path::{Component, Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}}};
use axum::{Json, extract::{Multipart, State}, http::StatusCode};
use tokio::io::AsyncWriteExt;
use crate::router::AppState;

type Reply = (StatusCode, Json<serde_json::Value>);
struct UploadError(StatusCode, String);
impl UploadError {
    fn bad(message: &str) -> Self { Self(StatusCode::BAD_REQUEST, message.into()) }
    fn reply(self) -> Reply { crate::json_error(self.0, &self.1) }
}
impl From<io::Error> for UploadError {
    fn from(error: io::Error) -> Self {
        let status = match error.kind() {
            io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
            io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
            io::ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if status == StatusCode::CONFLICT {
            "A file with this name already exists. Choose Replace to overwrite it.".into()
        } else { format!("Upload failed: {error}") };
        Self(status, message)
    }
}
fn relative_path(value: &str) -> Result<PathBuf, UploadError> {
    if value.is_empty() || value.contains(['\\', '\0']) || Path::new(value).is_absolute()
        || value.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.starts_with(".dashusb-upload-")) {
        return Err(UploadError::bad("Invalid relative upload path"));
    }
    Ok(PathBuf::from(value))
}
fn destination(directory: &str, relative: &str, bases: &[PathBuf]) -> Result<PathBuf, UploadError> {
    let root = Path::new(directory);
    if !root.is_absolute() || directory.contains(['\\', '\0'])
        || root.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err(UploadError::bad("Invalid destination folder"));
    }
    if !bases.iter().any(|base| root.starts_with(base)) {
        return Err(UploadError(StatusCode::FORBIDDEN, "Access denied".into()));
    }
    Ok(root.join(relative_path(relative)?))
}
fn temp_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!(".dashusb-upload-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}
struct UploadTemp(PathBuf);
impl Drop for UploadTemp { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
fn check_cancel(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) { Err(io::Error::new(io::ErrorKind::Interrupted, "Upload cancelled")) } else { Ok(()) }
}
struct Received { temp: UploadTemp, file: File, directory: String, relative: String, size: u64, overwrite: bool }

async fn metadata_text(mut field: axum::extract::multipart::Field<'_>) -> Result<String, UploadError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| UploadError(error.status(), "Incomplete upload metadata".into()))? {
        if bytes.len() + chunk.len() > 4096 { return Err(UploadError::bad("Upload metadata is too long")); }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| UploadError::bad("Invalid upload metadata"))
}
async fn receive(mut multipart: Multipart, temp_directory: &Path) -> Result<Received, UploadError> {
    let (mut directory, mut relative, mut overwrite) = (None, None, None);
    let mut upload = None;
    loop {
        let field = multipart.next_field().await.map_err(|error| UploadError(error.status(), "Incomplete upload".into()))?;
        let Some(mut field) = field else { break; };
        match field.name().unwrap_or("") {
            "path" => {
                if directory.is_some() { return Err(UploadError::bad("Duplicate path field")); }
                directory = Some(metadata_text(field).await?);
            }
            "relative_path" => {
                if relative.is_some() { return Err(UploadError::bad("Duplicate relative_path field")); }
                relative = Some(metadata_text(field).await?);
            }
            "overwrite" => {
                if overwrite.is_some() { return Err(UploadError::bad("Duplicate overwrite field")); }
                overwrite = Some(match metadata_text(field).await?.as_str() {
                    "true" => true, "false" => false,
                    _ => return Err(UploadError::bad("overwrite must be true or false")),
                });
            }
            "file" => {
                if upload.is_some() { return Err(UploadError::bad("Send one file per upload")); }
                let filename = field.file_name().unwrap_or("upload.bin").to_owned();
                // Create synchronously before an await so cancellation cannot
                // leave a file created by a detached open operation unguarded.
                let path = temp_directory.join(temp_name());
                let mut options = std::fs::OpenOptions::new();
                options.write(true).read(true).create_new(true);
                #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
                let file = options.open(&path)?;
                let temp = UploadTemp(path);
                let mut file = tokio::fs::File::from_std(file);
                let mut size = 0u64;
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            file.write_all(&chunk).await?;
                            size = size.checked_add(chunk.len() as u64).ok_or_else(|| UploadError::bad("File is too large"))?;
                        }
                        Ok(None) => break,
                        Err(error) => return Err(UploadError(error.status(), "Incomplete file payload".into())),
                    }
                }
                file.flush().await?;
                upload = Some((temp, file.into_std().await, filename, size));
            }
            _ => return Err(UploadError::bad("Unknown upload field")),
        }
    }
    let (temp, file, filename, size) = upload.ok_or_else(|| UploadError::bad("Missing file in upload"))?;
    let directory = directory.filter(|path: &String| !path.is_empty()).ok_or_else(|| UploadError::bad("Missing path parameter"))?;
    Ok(Received { temp, file, directory, relative: relative.unwrap_or(filename), size, overwrite: overwrite.unwrap_or(false) })
}

pub async fn upload_file(State(_state): State<AppState>, multipart: Multipart) -> Reply {
    handle(multipart, super::ALLOWED_BASES.iter().map(PathBuf::from).collect(), std::env::temp_dir()).await
}
async fn handle(multipart: Multipart, bases: Vec<PathBuf>, temp_directory: PathBuf) -> Reply {
    let received = match receive(multipart, &temp_directory).await { Ok(value) => value, Err(error) => return error.reply() };
    let destination = match destination(&received.directory, &received.relative, &bases) { Ok(value) => value, Err(error) => return error.reply() };
    let base = bases.into_iter().find(|base| destination.starts_with(base)).expect("validated upload base");
    let response = serde_json::json!({
        "name": destination.file_name().and_then(|name| name.to_str()).unwrap_or("upload.bin"),
        "path": destination.to_string_lossy(), "size": received.size.to_string(),
    });
    let cancellation = CancelOnDrop(Arc::new(AtomicBool::new(false)));
    let cancelled = cancellation.0.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut received = received;
        // Keep ownership explicit: the incoming file is removed on every exit.
        let _temporary = &received.temp;
        publish(&mut received.file, &destination, &base, received.overwrite, &cancelled)
    }).await;
    match result {
        Ok(Ok(())) => (StatusCode::OK, Json(response)),
        Ok(Err(error)) => UploadError::from(error).reply(),
        Err(_) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Upload publication task failed"),
    }
}

#[cfg(unix)]
mod confined {
    use super::*;
    use std::os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt};
    fn name(value: &std::ffi::OsStr) -> io::Result<CString> { CString::new(value.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid path")) }
    fn fd_file(fd: libc::c_int) -> io::Result<File> {
        if fd < 0 { Err(io::Error::last_os_error()) } else {
            // SAFETY: a successful open/openat returns a newly owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    pub(super) fn open_parent(path: &Path, base: &Path) -> io::Result<(File, CString)> {
        let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing destination parent"))?;
        let filename = name(path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing filename"))?)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // Walk every component from / with a held directory descriptor. A
        // replaced pathname cannot redirect later writes through a symlink.
        let mut directory = fd_file(unsafe { libc::open(c"/".as_ptr(), flags) })?;
        let mut current = PathBuf::from("/");
        for component in parent.components() {
            let Component::Normal(component) = component else {
                if component == Component::RootDir { continue; }
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid destination parent"));
            };
            current.push(component);
            let component = name(component)?;
            let mut next = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
            if next < 0 && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
                // Never recreate a missing managed root or one of its parents.
                // Only the selected upload's descendants may be created.
                if current == base || !current.starts_with(base) { return Err(io::Error::last_os_error()); }
                let created = unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o755) };
                if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists { return Err(io::Error::last_os_error()); }
                next = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
            }
            directory = fd_file(next).map_err(|error| {
                if matches!(error.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)) {
                    io::Error::new(io::ErrorKind::PermissionDenied, "Upload folders must not contain symbolic links")
                } else { error }
            })?;
        }
        Ok((directory, filename))
    }
    fn destination_metadata(parent: &File, filename: &CString) -> io::Result<Option<(libc::mode_t, libc::uid_t, libc::gid_t)>> {
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatat(parent.as_raw_fd(), filename.as_ptr(), &mut metadata, libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound { Ok(None) } else { Err(error) };
        }
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Uploads cannot replace a directory or symbolic link"));
        }
        Ok(Some((metadata.st_mode & 0o777, metadata.st_uid, metadata.st_gid)))
    }
    struct Staged<'a> { parent: &'a File, name: CString, active: bool }
    impl Drop for Staged<'_> {
        fn drop(&mut self) { if self.active { unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0); } } }
    }
    fn rename_exclusive(parent: &File, source: &CString, destination: &CString) -> io::Result<bool> {
        let fd = parent.as_raw_fd();
        #[cfg(target_os = "linux")]
        let rc = unsafe { libc::syscall(libc::SYS_renameat2, fd, source.as_ptr(), fd, destination.as_ptr(), libc::RENAME_NOREPLACE) };
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renameatx_np(fd, source.as_ptr(), fd, destination.as_ptr(), libc::RENAME_EXCL) } as libc::c_long;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let rc = -1;
        if rc == 0 { return Ok(true); }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)) { return Err(error); }
        // Hard-link publication is also atomic and never replaces an existing
        // name. Filesystems supporting neither primitive fail closed.
        if unsafe { libc::linkat(fd, source.as_ptr(), fd, destination.as_ptr(), 0) } == 0 { Ok(false) } else { Err(io::Error::last_os_error()) }
    }
    pub(super) fn publish_in(source: &mut File, parent: &File, filename: &CString, overwrite: bool, cancelled: &AtomicBool) -> io::Result<()> {
        check_cancel(cancelled)?;
        destination_metadata(parent, filename)?;
        let stage_name = CString::new(temp_name()).unwrap();
        let mut output = fd_file(unsafe { libc::openat(parent.as_raw_fd(), stage_name.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) })?;
        // Own cleanup only after exclusive creation succeeds.
        let mut stage = Staged { parent, name: stage_name, active: true };
        source.rewind()?;
        let expected = source.metadata()?.len();
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancel(cancelled)?;
            let bytes = source.read(&mut buffer)?;
            if bytes == 0 { break; }
            output.write_all(&buffer[..bytes])?;
            copied += bytes as u64;
        }
        if copied != expected { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Staged upload changed while copying")); }
        output.sync_data()?;
        check_cancel(cancelled)?;
        // Re-read just before publication. Preserve basic ownership/mode on
        // replacements; new files remain readable by guest Samba clients.
        let permissions = if let Some((mode, uid, gid)) = destination_metadata(parent, filename)? {
            if unsafe { libc::fchown(output.as_raw_fd(), uid, gid) } != 0 { return Err(io::Error::last_os_error()); }
            mode
        } else { 0o644 };
        if unsafe { libc::fchmod(output.as_raw_fd(), permissions) } != 0 { return Err(io::Error::last_os_error()); }
        output.sync_all()?;
        check_cancel(cancelled)?;
        if overwrite {
            // renameat replaces the directory entry, never follows a final
            // symlink and never truncates the old file in place.
            if unsafe { libc::renameat(parent.as_raw_fd(), stage.name.as_ptr(), parent.as_raw_fd(), filename.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            stage.active = false;
        } else if rename_exclusive(parent, &stage.name, filename)? {
            stage.active = false;
        }
        parent.sync_all()
    }
}
fn publish(source: &mut File, destination: &Path, base: &Path, overwrite: bool, cancelled: &AtomicBool) -> io::Result<()> {
    check_cancel(cancelled)?;
    #[cfg(unix)] {
        let (parent, filename) = confined::open_parent(destination, base)?;
        confined::publish_in(source, &parent, &filename, overwrite, cancelled)
    }
    #[cfg(not(unix))] {
        let _ = (source, destination, base, overwrite);
        Err(io::Error::new(io::ErrorKind::Unsupported, "Safe uploads require Unix filesystem operations"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::FromRequest, http::Request};

    fn directories() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap().join("destination");
        let staging = root.path().canonicalize().unwrap().join("incoming");
        std::fs::create_dir(&base).unwrap();
        std::fs::create_dir(&staging).unwrap();
        (root, base, staging)
    }
    fn part(name: &str, value: &str) -> String {
        format!("--BOUNDARY\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
    }
    fn file_part(name: &str, contents: &str) -> String {
        format!("--BOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n{contents}\r\n")
    }
    async fn multipart(body: Body) -> Multipart {
        Multipart::from_request(Request::builder().header("content-type", "multipart/form-data; boundary=BOUNDARY").body(body).unwrap(), &()).await.unwrap()
    }
    async fn post(body: String, base: &Path, staging: &Path) -> Reply {
        handle(multipart(Body::from(body)).await, vec![base.to_owned()], staging.to_owned()).await
    }
    fn no_staging(directory: &Path) {
        assert!(!std::fs::read_dir(directory).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".dashusb-upload-")));
    }

    #[test]
    fn paths_cannot_escape_selected_folder_or_use_reserved_stage_names() {
        for path in ["", "/etc/passwd", "../outside", "folder/../../escape", "a/./file", "a//file", "a\\file", "nul\0name", ".dashusb-upload-fake"] {
            assert!(relative_path(path).is_err(), "accepted {path:?}");
        }
        let bases = [PathBuf::from("/mutable"), PathBuf::from("/mnt/cam")];
        assert!(destination("/mutable-secret", "file", &bases).is_err());
        assert!(destination("/mutable/../etc", "file", &bases).is_err());
        assert!(destination("mutable", "file", &bases).is_err());
        assert_eq!(destination("/mutable", "Album A/clip.mp4", &bases).ok().unwrap(), PathBuf::from("/mutable/Album A/clip.mp4"));
    }

    #[tokio::test]
    async fn multipart_order_folders_collision_and_explicit_replacement() {
        let (_root, base, staging) = directories();
        let body = format!("{}{}{}--BOUNDARY--\r\n", file_part("clip.mp4", "first complete content"), part("relative_path", "Album A/clip.mp4"), part("path", base.to_str().unwrap()));
        let (status, Json(value)) = post(body.clone(), &base, &staging).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["name"], "clip.mp4");
        assert_eq!(value["size"], "22");
        let target = base.join("Album A/clip.mp4");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first complete content");
        let (status, Json(value)) = post(body, &base, &staging).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(value["error"].as_str().unwrap().contains("Replace"));
        let body = format!("{}{}{}{}--BOUNDARY--\r\n", part("path", base.to_str().unwrap()), part("relative_path", "Album A/clip.mp4"), part("overwrite", "true"), file_part("clip.mp4", "replacement"));
        assert_eq!(post(body, &base, &staging).await.0, StatusCode::OK);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "replacement");
        no_staging(&staging); no_staging(target.parent().unwrap());
    }

    #[tokio::test]
    async fn truncated_or_invalid_multipart_never_publishes_even_after_a_complete_file_part() {
        let (_root, base, staging) = directories();
        let prefix = part("path", base.to_str().unwrap());
        let mut cases = vec![
            format!("{prefix}{}", file_part("partial.mp4", "incomplete bytes")),
            format!("{prefix}{}--BOUNDARY\r\nContent-Disposition: form-data; name=\"relative_path\"\r\n\r\nbroken", file_part("partial.mp4", "file bytes")),
            format!("{prefix}{}{}--BOUNDARY--\r\n", file_part("partial.mp4", "bytes"), part("overwrite", "yes")),
            format!("{prefix}{}{}--BOUNDARY--\r\n", file_part("partial.mp4", "bytes"), part("path", base.to_str().unwrap())),
            format!("{prefix}{}{}--BOUNDARY--\r\n", file_part("one.mp4", "first"), file_part("two.mp4", "second")),
        ];
        for body in cases.drain(..) {
            assert_eq!(post(body, &base, &staging).await.0, StatusCode::BAD_REQUEST);
            assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
            no_staging(&staging);
        }
    }

    #[tokio::test]
    async fn dropping_a_receiving_request_removes_its_temporary_file() {
        let (_root, base, staging) = directories();
        let (sender, receiver) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, io::Error>>(2);
        let prefix = format!("{}--BOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"cancelled.mp4\"\r\n\r\n{}", part("path", base.to_str().unwrap()), "x".repeat(65_536));
        sender.send(Ok(prefix.into())).await.unwrap();
        let multipart = multipart(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))).await;
        let temp_path = staging.clone();
        let task = tokio::spawn(async move { receive(multipart, &temp_path).await });
        for _ in 0..200 {
            if std::fs::read_dir(&staging).unwrap().any(|entry| entry.unwrap().metadata().unwrap().len() > 0) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(std::fs::read_dir(&staging).unwrap().count() > 0);
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        drop(sender);
        no_staging(&staging);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_default_publish_has_one_winner_and_no_partial_files() {
        let (_root, base, staging) = directories();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [b'a', b'b'].into_iter().map(|byte| {
            let path = staging.join(format!("source-{byte}"));
            std::fs::write(&path, vec![byte; 131_072]).unwrap();
            let target = base.join("same.mp4"); let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut file = File::open(path).unwrap();
                barrier.wait();
                publish(&mut file, &target, target.parent().unwrap(), false, &AtomicBool::new(false))
            })
        }).collect();
        let results: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.into_iter().find_map(Result::err).unwrap().kind(), io::ErrorKind::AlreadyExists);
        let contents = std::fs::read(base.join("same.mp4")).unwrap();
        assert!(contents == vec![b'a'; 131_072] || contents == vec![b'b'; 131_072]);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replacement_keeps_old_readers_and_permissions_and_copy_failure_preserves_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, base, staging) = directories();
        let target = base.join("clip.mp4");
        std::fs::write(&target, b"old complete content").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut old_reader = File::open(&target).unwrap();
        let source = staging.join("source"); std::fs::write(&source, b"new complete content").unwrap();
        publish(&mut File::open(&source).unwrap(), &target, &base, true, &AtomicBool::new(false)).unwrap();
        let mut old_contents = String::new(); old_reader.read_to_string(&mut old_contents).unwrap();
        assert_eq!(old_contents, "old complete content");
        assert_eq!(std::fs::read(&target).unwrap(), b"new complete content");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640);
        assert!(publish(&mut File::open(&base).unwrap(), &target, &base, true, &AtomicBool::new(false)).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"new complete content");
        no_staging(&base);
        publish(&mut File::open(&source).unwrap(), &base.join("new.mp4"), &base, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::metadata(base.join("new.mp4")).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_cannot_redirect_uploads_or_overwrites() {
        use std::os::unix::fs::symlink;
        let (_root, base, staging) = directories();
        let outside = staging.join("outside"); std::fs::create_dir(&outside).unwrap();
        let source = staging.join("source"); std::fs::write(&source, b"upload").unwrap();
        let victim = outside.join("original"); std::fs::write(&victim, b"keep this").unwrap();
        symlink(&outside, base.join("folder-link")).unwrap();
        symlink(&victim, base.join("file-link")).unwrap();
        for target in [base.join("folder-link/new"), base.join("file-link")] {
            assert_eq!(publish(&mut File::open(&source).unwrap(), &target, &base, true, &AtomicBool::new(false)).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(std::fs::read(victim).unwrap(), b"keep this");
        assert!(!outside.join("new").exists());
        assert!(std::fs::symlink_metadata(base.join("file-link")).unwrap().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_parent_path_after_it_is_opened_cannot_redirect_publication() {
        use std::os::unix::fs::symlink;
        let (_root, base, staging) = directories();
        let source = staging.join("source"); std::fs::write(&source, b"upload").unwrap();
        let folder = base.join("folder"); std::fs::create_dir(&folder).unwrap();
        let outside = staging.join("outside"); std::fs::create_dir(&outside).unwrap();
        let (parent, filename) = confined::open_parent(&folder.join("clip.mp4"), &base).unwrap();
        std::fs::rename(&folder, base.join("original-folder")).unwrap();
        symlink(&outside, &folder).unwrap();
        confined::publish_in(&mut File::open(source).unwrap(), &parent, &filename, false, &AtomicBool::new(false)).unwrap();
        assert!(!outside.join("clip.mp4").exists());
        assert_eq!(std::fs::read(base.join("original-folder/clip.mp4")).unwrap(), b"upload");
    }

    #[test]
    fn missing_managed_root_is_not_recreated_under_the_parent_filesystem() {
        let (_root, base, staging) = directories();
        let source = staging.join("source"); std::fs::write(&source, b"upload").unwrap();
        std::fs::remove_dir(&base).unwrap();
        assert!(publish(&mut File::open(source).unwrap(), &base.join("clip.mp4"), &base, false, &AtomicBool::new(false)).is_err());
        assert!(!base.exists());
    }

    #[test]
    fn cancellation_before_publication_never_creates_the_destination() {
        let (_root, base, staging) = directories();
        let source = staging.join("source"); std::fs::write(&source, b"upload").unwrap();
        assert_eq!(publish(&mut File::open(source).unwrap(), &base.join("new/folder/file"), &base, false, &AtomicBool::new(true)).unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(std::fs::read_dir(base).unwrap().count(), 0);
    }
}
