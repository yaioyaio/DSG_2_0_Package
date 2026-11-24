//! Read usage from one operator-identified Linux/ARM64 BuildKit daemon.
use crate::docker::QueryFailure;
use crate::docker_cache::{buildx_usage, BuildxUsage};
use crate::docker_command::capture_buildx_output;
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::Command;
use std::time::{Duration, Instant};

const CLIENT_SHA256: &str = "cb57dfdc462d2d2ea1ca2f6b81b801520582ba536513228eb0e5213244626814";
const MAX_CLIENT_BYTES: u64 = 128 * 1024 * 1024;
const FORMAT: &str =
    r#"{{range .}}{{not .InUse}}|{{.Shared}}|{{.Mutable}}{{printf "%c" 10}}{{end}}"#;
type Result<T> = std::result::Result<T, QueryFailure>;

fn file(fd: RawFd) -> Result<File> {
    if fd < 0 {
        return Err(QueryFailure::Unavailable);
    }
    // The caller passes a descriptor freshly allocated by a syscall.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_at(parent: RawFd, name: &OsStr, flags: i32) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| QueryFailure::InvalidSelector)?;
    file(unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

// Walk through pinned directory descriptors. Symlinks in ancestors cannot redirect the walk.
fn open_path(path: &Path, flags: i32, private_parent: bool) -> Result<File> {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(QueryFailure::InvalidSelector);
    }
    let mut parts = parts.peekable();
    let directory = libc::O_PATH | libc::O_DIRECTORY;
    let mut parent = open_at(libc::AT_FDCWD, OsStr::new("/"), directory)?;
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Err(QueryFailure::InvalidSelector);
        };
        if parts.peek().is_none() {
            let meta = parent.metadata().map_err(|_| QueryFailure::Unavailable)?;
            if private_parent
                && (meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != 0o700)
            {
                return Err(QueryFailure::InvalidSelector);
            }
            return open_at(parent.as_raw_fd(), name, flags);
        }
        parent = open_at(parent.as_raw_fd(), name, directory)?;
    }
    Err(QueryFailure::InvalidSelector)
}

fn socket_file(path: &Path) -> Result<File> {
    let socket = open_path(path, libc::O_PATH, true)?;
    let meta = socket.metadata().map_err(|_| QueryFailure::Unavailable)?;
    if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(socket)
}

fn descriptor_path(fd: RawFd) -> String {
    format!("/proc/self/fd/{fd}")
}

fn check_peer(socket: &File, pid: i32) -> Result<()> {
    let probe = file(unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    })?;
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    let path = descriptor_path(socket.as_raw_fd());
    for (dest, src) in addr.sun_path.iter_mut().zip(path.bytes()) {
        *dest = src as _;
    }
    let len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    if unsafe {
        libc::connect(
            probe.as_raw_fd(),
            (&addr as *const libc::sockaddr_un).cast(),
            len as _,
        )
    } != 0
    {
        return Err(QueryFailure::Unavailable);
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&peer) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            probe.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
        || size as usize != std::mem::size_of_val(&peer)
        || peer.pid != pid
        || peer.uid != unsafe { libc::geteuid() }
    {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(())
}

fn check_alive(process: &File) -> Result<()> {
    let mut entry = libc::pollfd {
        fd: process.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut entry, 1, 0) } != 0 {
        return Err(QueryFailure::Unavailable);
    }
    Ok(())
}

fn verified_client(path: &Path, deadline: Instant) -> Result<File> {
    if path.file_name() != Some(OsStr::new("buildctl")) {
        return Err(QueryFailure::InvalidSelector);
    }
    // Inspect the pinned inode before opening its contents, including when the path names a device.
    let source = open_path(path, libc::O_PATH, false)?;
    let meta = source.metadata().map_err(|_| QueryFailure::Unavailable)?;
    if !meta.is_file() || meta.mode() & 0o111 == 0 || meta.len() > MAX_CLIENT_BYTES {
        return Err(QueryFailure::InvalidSelector);
    }
    // Preserve the source inode's execute permission, ACL and noexec mount policy.
    if unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            source.as_raw_fd(),
            c"".as_ptr(),
            libc::X_OK,
            libc::AT_EACCESS | libc::AT_EMPTY_PATH,
        )
    } != 0
    {
        return Err(QueryFailure::InvalidSelector);
    }
    let mut reader =
        File::open(descriptor_path(source.as_raw_fd())).map_err(|_| QueryFailure::Unavailable)?;
    let mut image = file(unsafe {
        libc::memfd_create(
            c"dsg-buildctl".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        if Instant::now() >= deadline {
            return Err(QueryFailure::Timeout);
        }
        let len = reader
            .read(&mut buffer)
            .map_err(|_| QueryFailure::Unavailable)?;
        if len == 0 {
            break;
        }
        total += len as u64;
        if total > MAX_CLIENT_BYTES {
            return Err(QueryFailure::TooLarge);
        }
        digest.update(&buffer[..len]);
        image
            .write_all(&buffer[..len])
            .map_err(|_| QueryFailure::Unavailable)?;
    }
    if format!("{:x}", digest.finalize()) != CLIENT_SHA256 {
        return Err(QueryFailure::InvalidSelector);
    }
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(image.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
        return Err(QueryFailure::Unavailable);
    }
    // Drop the writable file description before exec to avoid ETXTBSY.
    File::open(descriptor_path(image.as_raw_fd())).map_err(|_| QueryFailure::Unavailable)
}

/// Operator authorizes exactly the supplied local daemon PID and socket. This does not attest a Docker engine or cache ownership.
pub fn observe(socket_path: &Path, expected_pid: i32, buildctl: &Path) -> Result<BuildxUsage> {
    if expected_pid <= 0 {
        return Err(QueryFailure::InvalidSelector);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let process = file(unsafe { libc::syscall(libc::SYS_pidfd_open, expected_pid, 0) as RawFd })?;
    let socket = socket_file(socket_path)?;
    check_peer(&socket, expected_pid)?;
    let client = verified_client(buildctl, deadline)?;
    check_alive(&process)?;
    let fd = socket.as_raw_fd();
    let config = tempfile::tempdir_in("/tmp").map_err(|_| QueryFailure::Unavailable)?;
    let mut command = Command::new(descriptor_path(client.as_raw_fd()));
    command
        .env_clear()
        .env("HOME", config.path())
        .current_dir(config.path())
        .process_group(0)
        .args([
            "--addr",
            &format!("unix://{}", descriptor_path(fd)),
            "--timeout",
            "3",
            "du",
            "--format",
            FORMAT,
        ]);
    // The Go client connects through this inherited O_PATH descriptor, never by reopening the operator path.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let wire = capture_buildx_output(
        command,
        deadline.min(Instant::now() + Duration::from_secs(6)),
    )?;
    let current = socket_file(socket_path)?;
    let before = socket.metadata().map_err(|_| QueryFailure::Unavailable)?;
    let after = current.metadata().map_err(|_| QueryFailure::Unavailable)?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Err(QueryFailure::Unavailable);
    }
    check_peer(&current, expected_pid)?;
    check_alive(&process)?;
    if Instant::now() >= deadline {
        return Err(QueryFailure::Timeout);
    }
    buildx_usage(wire.strip_suffix('\n').ok_or(QueryFailure::Malformed)?)
}
