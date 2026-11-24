//! Local identity checks for one operator-selected Linux ARM64 controller.
use crate::docker::QueryFailure;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

mod admission;
pub use admission::{
    configure_write_request_listener, verify_admission, with_authorized_write_request,
    AdmissionEvidence, AuthorizedWriteRequest, WriteRequest,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SOCKET_PATH_BYTES: usize = 4095;
const MAX_REPLY_BYTES: usize = 512;
const REQUEST: &[u8] = b"{\"protocol_version\":1,\"operation\":\"probe\"}\n";
type Result<T> = std::result::Result<T, QueryFailure>;

#[derive(Debug, Serialize)]
pub struct ControllerEvidence {
    pub endpoint: String,
    pub generation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    protocol_version: u8,
    operation: String,
    boundary: String,
    access: String,
}

/// Confirm the local socket, peer process and a dedicated, read-only service declaration.
/// These observations convey no authority to modify resources.
pub fn probe(path: &Path, expected_pid: u32) -> Result<ControllerEvidence> {
    let pid = i32::try_from(expected_pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(QueryFailure::InvalidSelector)?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let socket = open_socket(path)?;
    let identity = socket_identity(&socket)?;
    // Keep the selected process instance pinned across the probe, including PID reuse.
    let process = owned_file(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as RawFd })?;
    let mut connection = connect(&socket)?;
    check_peer(&connection, pid, 0)?;
    let start = process_start(pid)?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(io_failure)?;
    check_alive(&process)?;
    exchange(&mut connection, deadline)?;
    if process_start(pid)? != start || socket_identity(&open_socket(path)?)? != identity {
        return Err(QueryFailure::Unavailable);
    }
    check_alive(&process)?;
    remaining(deadline)?;

    let mut endpoint = Sha256::new();
    endpoint.update(b"dsg-docker-controller-local-endpoint-v1\0");
    endpoint.update(path.as_os_str().as_bytes());
    let endpoint = endpoint.finalize();
    let mut generation = Sha256::new();
    generation.update(b"dsg-docker-controller-local-generation-v1\0");
    generation.update(endpoint);
    generation.update(pid.to_be_bytes());
    generation.update(start.to_be_bytes());
    generation.update(identity.0.to_be_bytes());
    generation.update(identity.1.to_be_bytes());
    generation.update(boot.as_bytes());
    Ok(ControllerEvidence {
        endpoint: format!("{endpoint:x}"),
        generation: format!("{:x}", generation.finalize()),
    })
}

fn owned_file(fd: RawFd) -> Result<File> {
    if fd < 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    // Each caller passes a newly opened descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_at(parent: RawFd, name: &OsStr, flags: i32) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| QueryFailure::InvalidSelector)?;
    // O_PATH pins the inode, and O_NOFOLLOW prevents traversal through a symlink.
    owned_file(unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

fn open_socket(path: &Path) -> Result<File> {
    open_protected(path, true)
}

pub(crate) fn open_protected(path: &Path, private_parent: bool) -> Result<File> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH_BYTES
        || path.as_os_str() != path.components().collect::<PathBuf>().as_os_str()
    {
        return Err(QueryFailure::InvalidSelector);
    }
    let mut components = path.components();
    components.next();
    let mut components = components.peekable();
    let mut parent = open_at(libc::AT_FDCWD, OsStr::new("/"), libc::O_DIRECTORY)?;
    while let Some(Component::Normal(name)) = components.next() {
        let info = parent.metadata().map_err(io_failure)?;
        if info.uid() != 0 || info.mode() & 0o022 != 0 {
            return Err(QueryFailure::InvalidSelector);
        }
        if components.peek().is_none() {
            if private_parent && info.mode() & 0o7777 != 0o700 {
                return Err(QueryFailure::InvalidSelector);
            }
            return open_at(parent.as_raw_fd(), name, 0);
        }
        parent = open_at(parent.as_raw_fd(), name, libc::O_DIRECTORY)?;
    }
    Err(QueryFailure::InvalidSelector)
}

fn socket_identity(socket: &File) -> Result<(u64, u64)> {
    let info = socket.metadata().map_err(io_failure)?;
    if !info.file_type().is_socket()
        || info.uid() != 0
        || info.mode() & 0o7777 != 0o600
        || info.nlink() != 1
    {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok((info.dev(), info.ino()))
}

fn connect(socket: &File) -> Result<UnixStream> {
    // A nonblocking local connect fails closed when the peer's listen queue is full.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    // The descriptor is a new Unix stream socket and is closed by UnixStream on failure.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let descriptor = format!("/proc/self/fd/{}", socket.as_raw_fd());
    // Zero initialization supplies the terminating NUL for the short procfs pathname.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    for (slot, byte) in address.sun_path.iter_mut().zip(descriptor.bytes()) {
        *slot = byte as _;
    }
    let size = std::mem::offset_of!(libc::sockaddr_un, sun_path) + descriptor.len() + 1;
    if unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), size as _) } != 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    stream.set_nonblocking(false).map_err(io_failure)?;
    Ok(stream)
}

fn check_peer(stream: &UnixStream, pid: i32, uid: u32) -> Result<()> {
    let peer = peer_credentials(stream.as_raw_fd())?;
    if peer.pid != pid || peer.uid != uid {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(())
}

fn peer_credentials(fd: RawFd) -> Result<libc::ucred> {
    let mut peer = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = std::mem::size_of_val(&peer) as libc::socklen_t;
    // The credential buffer is writable and size matches the Linux SO_PEERCRED structure.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io_failure(io::Error::last_os_error()));
    }
    if size as usize != std::mem::size_of_val(&peer) {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(peer)
}

fn check_alive(process: &File) -> Result<()> {
    let mut entry = libc::pollfd {
        fd: process.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // PID descriptors become readable when the process exits.
    if unsafe { libc::poll(&mut entry, 1, 0) } != 0 {
        return Err(QueryFailure::Unavailable);
    }
    Ok(())
}

fn process_start(pid: i32) -> Result<u64> {
    let stat = fs::read(format!("/proc/{pid}/stat")).map_err(io_failure)?;
    // The process name can contain closing parentheses and non-UTF-8 bytes.
    let name_end = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or(QueryFailure::Unavailable)?;
    let fields =
        std::str::from_utf8(&stat[name_end + 1..]).map_err(|_| QueryFailure::Unavailable)?;
    fields
        .split_ascii_whitespace()
        .nth(19)
        .and_then(|time| time.parse().ok())
        .ok_or(QueryFailure::Unavailable)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(QueryFailure::Timeout)
}

fn exchange(stream: &mut UnixStream, deadline: Instant) -> Result<()> {
    let mut request = REQUEST;
    while !request.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(io_failure)?;
        let count = stream.write(request).map_err(io_failure)?;
        if count == 0 {
            return Err(QueryFailure::Unavailable);
        }
        request = &request[count..];
    }
    stream.shutdown(Shutdown::Write).map_err(io_failure)?;

    let mut reply = [0; MAX_REPLY_BYTES + 1];
    let mut length = 0;
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(io_failure)?;
        let count = stream.read(&mut reply[length..]).map_err(io_failure)?;
        if count == 0 {
            return validate_reply(&reply[..length]);
        }
        length += count;
        if length > MAX_REPLY_BYTES {
            return Err(QueryFailure::TooLarge);
        }
    }
}

fn validate_reply(reply: &[u8]) -> Result<()> {
    let json = reply
        .strip_suffix(b"\n")
        .filter(|line| {
            line.first() == Some(&b'{')
                && line.last() == Some(&b'}')
                && !line.iter().any(|byte| matches!(*byte, b'\r' | b'\n'))
        })
        .ok_or(QueryFailure::Malformed)?;
    let response: Declaration =
        serde_json::from_slice(json).map_err(|_| QueryFailure::Malformed)?;
    if response.protocol_version != 1
        || response.operation != "probe"
        || response.boundary != "dedicated"
        || response.access != "read_only"
    {
        return Err(QueryFailure::Malformed);
    }
    Ok(())
}

fn io_failure(error: io::Error) -> QueryFailure {
    match error.kind() {
        io::ErrorKind::PermissionDenied => QueryFailure::InvalidSelector,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => QueryFailure::Timeout,
        _ => QueryFailure::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_dedicated_read_only_v1_declaration_is_accepted() {
        let valid = b"{\"protocol_version\":1,\"operation\":\"probe\",\"boundary\":\"dedicated\",\"access\":\"read_only\"}\n";
        assert_eq!(validate_reply(valid), Ok(()));
        for reply in [
            &valid[..valid.len() - 1],
            b"{\"protocol_version\":2,\"operation\":\"probe\",\"boundary\":\"dedicated\",\"access\":\"read_only\"}\n",
            b"{\"protocol_version\":1,\"operation\":\"probe\",\"boundary\":\"shared\",\"access\":\"read_only\"}\n",
            b"{\"protocol_version\":1,\"operation\":\"probe\",\"boundary\":\"dedicated\",\"access\":\"write\"}\n",
            b"{\"protocol_version\":1,\"operation\":\"probe\",\"boundary\":\"dedicated\",\"access\":\"read_only\",\"access\":\"read_only\"}\n",
            b"[1,\"probe\",\"dedicated\",\"read_only\"]\n",
        ] {
            assert_eq!(validate_reply(reply), Err(QueryFailure::Malformed));
        }
    }

    #[test]
    fn probe_rejects_relative_paths_and_invalid_pids() {
        assert_eq!(
            probe(Path::new("relative.sock"), 1).unwrap_err(),
            QueryFailure::InvalidSelector
        );
        for pid in [0, u32::MAX] {
            assert_eq!(
                probe(Path::new("/probe.sock"), pid).unwrap_err(),
                QueryFailure::InvalidSelector
            );
        }
    }

    #[test]
    fn unix_stream_peer_must_have_the_expected_pid_and_root_uid() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        assert_eq!(
            check_peer(&stream, 0, 0),
            Err(QueryFailure::InvalidSelector)
        );
        // A socket pair records this process as the peer when it is created.
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(
            check_peer(&stream, pid, 0),
            if root {
                Ok(())
            } else {
                Err(QueryFailure::InvalidSelector)
            }
        );
    }
}
