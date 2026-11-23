use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

const ACCEPT_POLL: Duration = Duration::from_millis(25);
const MAX_OWNER_RECORD: u64 = 256;

pub(crate) fn serve(
    listen: &Path,
    upstream: &Path,
    allowed_uids: &[u32],
    mut handle: impl FnMut(UnixStream, &Path, &AtomicBool) -> io::Result<()>,
) -> io::Result<()> {
    if allowed_uids.is_empty() {
        return Err(invalid("the socket peer UID allowlist is empty"));
    }
    validate_paths(listen, upstream)?;
    let stopping = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&stopping);
    ctrlc::set_handler(move || signal_flag.store(true, Ordering::Release))
        .map_err(|_| io::Error::other("could not install daemon signal handling"))?;

    let listener = bind_owned(listen)?;
    // Check any alias that was unresolved while both socket names were absent.
    validate_paths(listen, upstream)?;
    while !stopping.load(Ordering::Acquire) {
        match listener.listener.accept() {
            Ok((stream, _)) => {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                if peer_uid(&stream).is_ok_and(|uid| allowed_uids.contains(&uid)) {
                    // Linux and macOS differ in accepted socket flag inheritance.
                    if stream.set_nonblocking(false).is_ok() {
                        let _ = handle(stream, upstream, &stopping);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    listener.close()
}

pub(crate) fn validate_paths(listen: &Path, upstream: &Path) -> io::Result<()> {
    check_paths(listen, upstream, false)
}

pub(crate) fn validate_upstream(listen: &Path, upstream: &Path) -> io::Result<()> {
    check_paths(listen, upstream, true)
}

fn check_paths(listen: &Path, upstream: &Path, serving: bool) -> io::Result<()> {
    let uid = effective_uid();
    let listen_parent = controlled_parent(listen, uid)?;
    let upstream_parent = controlled_parent(upstream, uid)?;
    if listen == upstream {
        return Err(invalid("the listen and upstream sockets must be distinct"));
    }
    if listen_parent.uid() != uid {
        return Err(denied(
            "the listen directory must be owned by the daemon UID",
        ));
    }
    if upstream_parent.mode() & 0o077 != 0 {
        return Err(denied("the upstream directory must be private"));
    }

    let listen_entry = socket_entry(listen)?;
    let upstream_entry = socket_entry(upstream)?;
    if listen_entry
        .as_ref()
        .is_some_and(|entry| entry.uid() != uid)
    {
        return Err(denied("the existing listen socket has a different owner"));
    }
    if upstream_entry
        .as_ref()
        .is_some_and(|entry| entry.uid() != uid && entry.uid() != 0)
    {
        return Err(denied("the upstream socket is not owned by a trusted UID"));
    }
    if let (Some(left), Some(right)) = (&listen_entry, &upstream_entry) {
        if (left.dev(), left.ino()) == (right.dev(), right.ino()) {
            return Err(invalid("the listen and upstream sockets must be distinct"));
        }
    }
    if serving && (listen_entry.is_none() || upstream_entry.is_none()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "a configured daemon socket is unavailable",
        ));
    }
    Ok(())
}

fn controlled_parent(path: &Path, uid: u32) -> io::Result<Metadata> {
    if !is_normalized_absolute(path) || path.file_name().is_none() {
        return Err(invalid("socket paths must be normalized absolute paths"));
    }
    SocketAddr::from_pathname(path)?;
    check_directory(path.parent().expect("validated absolute socket path"), uid)
}

pub(crate) fn controlled_directory(path: &Path) -> io::Result<Metadata> {
    check_directory(path, effective_uid())
}

fn check_directory(path: &Path, uid: u32) -> io::Result<Metadata> {
    if !is_normalized_absolute(path) {
        return Err(invalid("directory paths must be normalized absolute paths"));
    }
    let mut prefix = PathBuf::new();
    let mut metadata = None;
    for part in path.components() {
        prefix.push(part);
        let current = fs::symlink_metadata(&prefix)?;
        if !current.is_dir()
            || (current.uid() != uid && current.uid() != 0)
            || current.mode() & 0o022 != 0
        {
            return Err(denied(
                "daemon directories must be controlled by the daemon or root",
            ));
        }
        reject_extended_grants(&prefix)?;
        metadata = Some(current);
    }
    Ok(metadata.expect("validated absolute socket parent"))
}

fn is_normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str() == path.components().collect::<PathBuf>().as_os_str()
        && !path.components().any(|part| part == Component::ParentDir)
}

fn socket_entry(path: &Path) -> io::Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => Ok(Some(metadata)),
        Ok(_) => Err(denied(
            "the socket path is already occupied by another file type",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn bind_owned(path: &Path) -> io::Result<BoundSocket<'_>> {
    let uid = effective_uid();
    let _ = controlled_parent(path, uid)?;
    let existing = socket_entry(path)?;
    let (mut owner, created) = acquire_owner_lock(path, uid, existing.is_none())?;
    if let Some(entry) = socket_entry(path)? {
        let owned = OwnedSocket::new(path, entry, uid)?;
        if !owned.matches_record(&mut owner)? {
            return Err(denied(
                "the existing socket has no matching daemon ownership record",
            ));
        }
        owned.remove()?;
    } else if !created && !is_owner_record(&read_owner_record(&mut owner)?) {
        return Err(denied("the existing ownership file is unrecognized"));
    }

    let listener = UnixListener::bind(path)?;
    let mut owned = OwnedSocket::new(path, required_socket(path)?, uid)?;
    let result = (|| {
        listener.set_nonblocking(true)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
        owned.refresh()?;
        owned.write_record(&mut owner)
    })();
    if let Err(error) = result {
        let _ = owned.remove();
        return Err(error);
    }
    Ok(BoundSocket {
        listener,
        owned,
        _owner_lock: owner,
        cleanup_attempted: false,
    })
}

struct BoundSocket<'a> {
    listener: UnixListener,
    owned: OwnedSocket<'a>,
    _owner_lock: File,
    cleanup_attempted: bool,
}

impl BoundSocket<'_> {
    fn close(mut self) -> io::Result<()> {
        self.cleanup_attempted = true;
        self.owned.remove()
    }
}

impl Drop for BoundSocket<'_> {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.owned.remove();
        }
    }
}

fn required_socket(path: &Path) -> io::Result<Metadata> {
    socket_entry(path)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the daemon socket is unavailable"))
}

fn acquire_owner_lock(path: &Path, uid: u32, create: bool) -> io::Result<(File, bool)> {
    let mut name = path
        .file_name()
        .expect("validated absolute socket path")
        .to_owned();
    name.push(".dsg-owner");
    let sidecar = path.with_file_name(name);
    let created = match fs::symlink_metadata(&sidecar) {
        Ok(metadata) => {
            check_owner_file(&metadata, uid)?;
            reject_extended_grants(&sidecar)?;
            false
        }
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
    };
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(created)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&sidecar)?;
    check_owner_file(&owner.metadata()?, uid)?;
    // The descriptor outlives the bound socket and serializes cooperating daemon launches.
    if unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((owner, created))
}

fn check_owner_file(metadata: &Metadata, uid: u32) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(denied("the daemon ownership record has unsafe metadata"));
    }
    Ok(())
}

fn read_owner_record(file: &mut File) -> io::Result<Vec<u8>> {
    let mut content = Vec::new();
    file.rewind()?;
    file.take(MAX_OWNER_RECORD + 1).read_to_end(&mut content)?;
    Ok(content)
}

fn is_owner_record(content: &[u8]) -> bool {
    let Some(body) = std::str::from_utf8(content)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|text| text.len() < MAX_OWNER_RECORD as usize)
    else {
        return false;
    };
    let is_nanoseconds = |value: &str| value.parse::<u32>().is_ok_and(|n| n < 1_000_000_000);
    let fields: Vec<&str> = body.split(' ').collect();
    let ["dsg-socket-v1", device, inode, kind, values @ ..] = fields.as_slice() else {
        return false;
    };
    if device.parse::<u64>().is_err() || inode.parse::<u64>().is_err() {
        return false;
    }
    match (*kind, values) {
        ("birth", [seconds, nanos]) => seconds.parse::<u64>().is_ok() && is_nanoseconds(nanos),
        ("times", [change, change_nanos, modify, modify_nanos]) => {
            change.parse::<i64>().is_ok()
                && is_nanoseconds(change_nanos)
                && modify.parse::<i64>().is_ok()
                && is_nanoseconds(modify_nanos)
        }
        _ => false,
    }
}

struct OwnedSocket<'a> {
    path: &'a Path,
    metadata: Metadata,
    uid: u32,
}

impl<'a> OwnedSocket<'a> {
    fn new(path: &'a Path, metadata: Metadata, uid: u32) -> io::Result<Self> {
        if metadata.uid() != uid || metadata.nlink() != 1 {
            return Err(denied(
                "the listen socket does not have exclusive daemon ownership",
            ));
        }
        Ok(Self {
            path,
            metadata,
            uid,
        })
    }

    fn record(metadata: &Metadata) -> String {
        let dev = metadata.dev();
        let ino = metadata.ino();
        if let Some(birth) = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        {
            return format!(
                "dsg-socket-v1 {dev} {ino} birth {} {}\n",
                birth.as_secs(),
                birth.subsec_nanos()
            );
        }
        format!(
            "dsg-socket-v1 {dev} {ino} times {} {} {} {}\n",
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.mtime(),
            metadata.mtime_nsec()
        )
    }

    fn matches_record(&self, file: &mut File) -> io::Result<bool> {
        Ok(read_owner_record(file)? == Self::record(&self.metadata).as_bytes())
    }

    fn write_record(&self, file: &mut File) -> io::Result<()> {
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(Self::record(&self.metadata).as_bytes())?;
        file.sync_all()
    }

    fn refresh(&mut self) -> io::Result<()> {
        let updated = Self::new(self.path, required_socket(self.path)?, self.uid)?;
        if (updated.metadata.dev(), updated.metadata.ino())
            != (self.metadata.dev(), self.metadata.ino())
        {
            return Err(denied("the listen socket identity changed"));
        }
        self.metadata = updated.metadata;
        Ok(())
    }

    fn remove(&self) -> io::Result<()> {
        controlled_parent(self.path, self.uid)?;
        let Some(current) = socket_entry(self.path)? else {
            return Ok(());
        };
        if current.uid() != self.uid
            || current.nlink() != 1
            || Self::record(&current) != Self::record(&self.metadata)
        {
            return Err(denied("the listen socket identity changed"));
        }
        fs::remove_file(self.path)
    }
}

fn effective_uid() -> u32 {
    // geteuid queries the current process and needs no input pointers.
    unsafe { libc::geteuid() }
}

#[cfg(target_os = "linux")]
pub(crate) fn reject_extended_grants(_path: &Path) -> io::Result<()> {
    // The group mode bits bound permissions granted by Linux POSIX ACL entries.
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn reject_extended_grants(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    type Acl = *mut libc::c_void;
    type Entry = *mut libc::c_void;
    const EXTENDED: libc::c_int = 0x100;
    const FIRST: libc::c_int = 0;
    const NEXT: libc::c_int = -1;
    const DENY: libc::c_int = 2;
    extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, acl_type: libc::c_int) -> Acl;
        fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut Entry) -> libc::c_int;
        fn acl_get_tag_type(entry: Entry, tag: *mut libc::c_int) -> libc::c_int;
        fn acl_free(acl: Acl) -> libc::c_int;
    }

    let path = CString::new(path.as_os_str().as_bytes())?;
    // The absolute path is NUL-terminated and the returned ACL is owned here.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        };
    }

    let mut index = FIRST;
    let result = loop {
        let mut entry = ptr::null_mut();
        // A live ACL and writable output slot satisfy the API contract.
        if unsafe { acl_get_entry(acl, index, &mut entry) } != 0 {
            let error = io::Error::last_os_error();
            break if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            };
        }
        let mut tag = 0;
        // This entry remains valid until the ACL is freed.
        if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
            break Err(io::Error::last_os_error());
        }
        if tag != DENY {
            break Err(denied(
                "extended access grants are not supported on daemon paths",
            ));
        }
        index = NEXT;
    };
    // The ACL and its entries have no further users.
    unsafe { acl_free(acl) };
    result
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid = 0;
    let mut gid = 0;
    // Both output pointers refer to writable uid_t/gid_t values.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let expected = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let mut length = expected;
    // The output buffer has the size advertised to getsockopt.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if length != expected {
        return Err(denied("the socket peer UID is unavailable"));
    }
    // A successful call reporting the full size initialized ucred.
    Ok(unsafe { credentials.assume_init() }.uid)
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let configured = std::env::var_os("DSG_TEST_RUNTIME_DIR");
        #[cfg(target_os = "linux")]
        let configured = configured.or_else(|| std::env::var_os("HOME"));
        let base = configured.map_or_else(std::env::temp_dir, PathBuf::from);
        let temp = fs::canonicalize(base).unwrap();
        tempfile::Builder::new()
            .prefix("dsg-")
            .tempdir_in(temp)
            .unwrap()
    }

    #[test]
    fn owner_lock_serializes_startup_and_record_allows_crash_recovery() {
        let temp = fixture();
        let path = temp.path().join("p.sock");
        let mut initial = bind_owned(&path).unwrap();
        assert!(bind_owned(&path).is_err());
        let _client = UnixStream::connect(&path).unwrap();
        let (accepted, _) = initial.listener.accept().unwrap();
        assert!(peer_uid(&accepted).is_ok_and(|uid| uid == effective_uid()));

        // Close the descriptors without invoking orderly endpoint cleanup.
        initial.cleanup_attempted = true;
        drop(initial);
        assert!(path.exists());
        let restarted = bind_owned(&path).unwrap();
        restarted.close().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn sockets_with_no_matching_ownership_record_are_preserved() {
        let temp = fixture();
        let path = temp.path().join("p.sock");
        let managed = bind_owned(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        assert!(managed.close().is_err());
        assert!(bind_owned(&path).is_err());
        let _client = UnixStream::connect(&path).unwrap();
        replacement.accept().unwrap();
        drop(replacement);

        assert!(bind_owned(&path).is_err());
        assert!(path.exists());
    }

    #[test]
    fn unrelated_owner_files_are_preserved_before_first_bind() {
        let temp = fixture();
        let path = temp.path().join("p.sock");
        let sidecar = temp.path().join("p.sock.dsg-owner");
        for content in [
            b"".as_slice(),
            b"original note\n",
            b"dsg-socket-v1 invalid\n",
        ] {
            fs::write(&sidecar, content).unwrap();
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(bind_owned(&path).is_err());
            assert_eq!(fs::read(&sidecar).unwrap(), content);
            assert!(!path.exists());
        }
    }

    #[test]
    fn upstream_open_requires_private_parent_and_real_socket() {
        let temp = fixture();
        let listen = temp.path().join("p.sock");
        let parent = temp.path().join("u");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let upstream = parent.join("u.sock");
        assert!(validate_paths(&listen, &upstream).is_err());

        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        validate_paths(&listen, &upstream).unwrap();
        assert!(validate_paths(&listen, &listen).is_err());
        let _managed = bind_owned(&listen).unwrap();
        assert_eq!(
            validate_upstream(&listen, &upstream).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        symlink(&listen, &upstream).unwrap();
        assert!(validate_upstream(&listen, &upstream).is_err());
        fs::remove_file(&upstream).unwrap();
        let _upstream = UnixListener::bind(&upstream).unwrap();
        validate_upstream(&listen, &upstream).unwrap();
    }
}
