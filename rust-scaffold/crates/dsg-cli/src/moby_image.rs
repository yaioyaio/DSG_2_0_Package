use dsg_core::docker_control_local::configure_write_request_listener;
use dsg_core::docker_gc_apply::serve_moby_image_request;
use serde_json::value::RawValue;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::mem::{offset_of, size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

const MAX_FRAME_BYTES: usize = 512;
const BACKLOG: libc::c_int = 16;
const RESPONSE_TIMEOUT_MS: libc::c_int = 60_000;
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_REQUEST: &[u8] = b"{\"protocol_version\":1,\"operation\":\"probe\"}\n";
const PROBE_REPLY: &[u8] =
    b"{\"protocol_version\":1,\"operation\":\"probe\",\"boundary\":\"dedicated\",\"access\":\"read_only\"}\n";
const INVALID_REQUEST: &[u8] = br#"{"status":"error","failure":"invalid_request"}"#;
const TRANSPORT_UNAVAILABLE: &[u8] = br#"{"status":"error","failure":"transport_unavailable"}"#;

pub(crate) fn serve(
    policy: &Path,
    state_dir: &Path,
    control: &Path,
    probe: &Path,
    store_directory: &Path,
) -> ExitCode {
    match serve_inner(policy, state_dir, control, probe, store_directory) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("보호된 Moby 이미지 제어 서비스를 실행할 수 없습니다");
            ExitCode::from(2)
        }
    }
}

fn serve_inner(
    policy: &Path,
    state_dir: &Path,
    control: &Path,
    probe: &Path,
    store_directory: &Path,
) -> io::Result<()> {
    if current_uid()? != 0
        || policy.file_name() != Some(OsStr::new("docker-controller-admission.json"))
    {
        return Err(invalid());
    }
    // The policy binds this process and may be installed after the sockets are listening.
    socket_path(policy, None)?;
    protected_directory(state_dir, Some(0o700))?;
    let store = protected_directory(store_directory, Some(0o700))?;
    if store.as_raw_fd() < 3 {
        return Err(invalid());
    }

    let (control, _control_path) = bind_listener(control, 0o755, 0o666, libc::SOCK_SEQPACKET)?;
    let (probe, _probe_path) = bind_listener(probe, 0o700, 0o600, libc::SOCK_STREAM)?;
    start_probe(UnixListener::from(probe))?;
    {
        let mut output = io::stdout().lock();
        writeln!(output, "{{\"store_fd\":{}}}", store.as_raw_fd())?;
        output.flush()?;
    }
    loop {
        match accept(control.as_raw_fd()) {
            Ok(connection) => serve_moby_image_request(policy, state_dir, connection),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn session(control: &Path, probe_fd: RawFd) -> ExitCode {
    match session_inner(control, probe_fd) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(_) => {
            eprintln!("보호된 Moby 이미지 역할 세션을 실행할 수 없습니다");
            ExitCode::from(2)
        }
    }
}

fn session_inner(control: &Path, probe_fd: RawFd) -> io::Result<bool> {
    if current_uid()? == 0 {
        return Err(invalid());
    }
    let control = socket_path(control, Some(0o755))?;
    start_probe(inherited_probe(probe_fd)?)?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut request = Vec::with_capacity(MAX_FRAME_BYTES + 1);
    let mut reply = [0; MAX_FRAME_BYTES];
    loop {
        match read_frame(&mut input, &mut request)? {
            Frame::End => return Ok(true),
            Frame::Incomplete => {
                reply_line(&mut output, INVALID_REQUEST)?;
                return Ok(false);
            }
            Frame::Complete => {}
        }
        let response = if !is_json_object(&request) {
            INVALID_REQUEST
        } else {
            exchange(&control, &request, &mut reply).unwrap_or(TRANSPORT_UNAVAILABLE)
        };
        reply_line(&mut output, response)?;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Frame {
    End,
    Complete,
    Incomplete,
}

fn read_frame(input: &mut impl BufRead, bytes: &mut Vec<u8>) -> io::Result<Frame> {
    bytes.clear();
    let count = input
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', bytes)?;
    Ok(match (count, bytes.last()) {
        (0, _) => Frame::End,
        (_, Some(b'\n')) => {
            bytes.pop();
            Frame::Complete
        }
        _ => Frame::Incomplete,
    })
}

fn is_json_object(bytes: &[u8]) -> bool {
    serde_json::from_slice::<&RawValue>(bytes).is_ok_and(|value| value.get().starts_with('{'))
}

fn reply_line(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    output.write_all(bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn exchange<'a>(
    control: &SocketPath,
    request: &[u8],
    reply: &'a mut [u8; MAX_FRAME_BYTES],
) -> io::Result<&'a [u8]> {
    let endpoint = control.open()?;
    socket_identity(&endpoint, 0o666)?;
    let socket = unix_socket(libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK)?;
    let path = format!("/proc/self/fd/{}", endpoint.as_raw_fd());
    let (address, len) = unix_address(path.as_bytes())?;
    let fd = socket.as_raw_fd();
    check(unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), len) })?;
    require_root_peer(fd)?;
    let sent = unsafe {
        libc::send(
            fd,
            request.as_ptr().cast(),
            request.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if sent != request.len() as isize {
        return Err(invalid());
    }
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    check(unsafe { libc::poll(&mut poll, 1, RESPONSE_TIMEOUT_MS) })?;
    if poll.revents & libc::POLLIN == 0 {
        return Err(invalid());
    }
    let count = unsafe { libc::recv(fd, reply.as_mut_ptr().cast(), reply.len(), libc::MSG_TRUNC) };
    let count = usize::try_from(count).map_err(|_| invalid())?;
    let response = reply.get(..count).ok_or_else(invalid)?;
    if response.iter().any(|&byte| matches!(byte, b'\r' | b'\n')) || !is_json_object(response) {
        return Err(invalid());
    }
    Ok(response)
}

fn current_uid() -> io::Result<libc::uid_t> {
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    check(unsafe { libc::getresuid(&mut real, &mut effective, &mut saved) })?;
    if real != effective || real != saved {
        return Err(invalid());
    }
    Ok(real)
}

fn require_root_peer(fd: RawFd) -> io::Result<()> {
    // An exact SO_PEERCRED value is a C ucred with three integer fields.
    let peer: libc::ucred = unsafe { socket_option(fd, libc::SO_PEERCRED) }?;
    if peer.uid != 0 || peer.pid <= 0 {
        return Err(invalid());
    }
    Ok(())
}

/// A successful response for `option` must initialize a valid `T`.
unsafe fn socket_option<T>(fd: RawFd, option: libc::c_int) -> io::Result<T> {
    let mut data = std::mem::MaybeUninit::<T>::uninit();
    let mut len = size_of::<T>() as libc::socklen_t;
    check(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            data.as_mut_ptr().cast(),
            &mut len,
        )
    })?;
    if len as usize != size_of::<T>() {
        return Err(invalid());
    }
    Ok(unsafe { data.assume_init() })
}

fn inherited_probe(fd: RawFd) -> io::Result<UnixListener> {
    // The three requested options each return exactly one C integer.
    let get_int = |option| unsafe { socket_option::<libc::c_int>(fd, option) };
    if fd < 3
        || get_int(libc::SO_DOMAIN)? != libc::AF_UNIX
        || get_int(libc::SO_TYPE)? != libc::SOCK_STREAM
        || get_int(libc::SO_ACCEPTCONN)? != 0
    {
        return Err(invalid());
    }
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    let mut len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let address_ptr = (&mut address as *mut libc::sockaddr_un).cast();
    check(unsafe { libc::getsockname(fd, address_ptr, &mut len) })?;
    if len as usize <= offset_of!(libc::sockaddr_un, sun_path)
        || address.sun_path[0] != b'/' as libc::c_char
    {
        return Err(invalid());
    }
    // The installer bound the descriptor before handing it off. listen captures this role's
    // credentials for clients connecting later.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    check(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) })?;
    check(unsafe { libc::listen(fd, BACKLOG) })?;
    Ok(UnixListener::from(socket))
}

fn start_probe(listener: UnixListener) -> io::Result<()> {
    listener.set_nonblocking(false)?;
    thread::Builder::new()
        .name("moby-image-probe".to_owned())
        .spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = answer_probe(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        })?;
    Ok(())
}

fn answer_probe(mut stream: UnixStream) -> io::Result<()> {
    require_root_peer(stream.as_raw_fd())?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut input = [0; PROBE_REQUEST.len() + 1];
    let mut count = 0;
    while count < input.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(invalid)?;
        stream.set_read_timeout(Some(remaining))?;
        let read = stream.read(&mut input[count..])?;
        if read == 0 {
            break;
        }
        count += read;
    }
    if &input[..count] != PROBE_REQUEST {
        return Err(invalid());
    }
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    stream.write_all(PROBE_REPLY)
}

struct SocketPath {
    parent: File,
    name: CString,
}

impl SocketPath {
    fn open(&self) -> io::Result<File> {
        open_at(self.parent.as_raw_fd(), &self.name, 0)
    }
}

struct BoundSocket {
    path: SocketPath,
    identity: (u64, u64),
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        let current = self.path.open().and_then(|file| file.metadata());
        if current.is_ok_and(|info| {
            info.file_type().is_socket() && (info.dev(), info.ino()) == self.identity
        }) {
            // The directory is root-controlled. Leave a replaced inode untouched.
            unsafe { libc::unlinkat(self.path.parent.as_raw_fd(), self.path.name.as_ptr(), 0) };
        }
    }
}

fn bind_listener(
    path: &Path,
    parent_mode: u32,
    mode: u32,
    kind: libc::c_int,
) -> io::Result<(OwnedFd, BoundSocket)> {
    let path = socket_path(path, Some(parent_mode))?;
    let socket = unix_socket(kind)?;
    if kind == libc::SOCK_SEQPACKET {
        configure_write_request_listener(socket.as_fd()).map_err(|_| invalid())?;
    }
    let mut scoped_path = format!("/proc/self/fd/{}/", path.parent.as_raw_fd()).into_bytes();
    scoped_path.extend_from_slice(path.name.as_bytes());
    let (address, len) = unix_address(&scoped_path)?;
    let fd = socket.as_raw_fd();
    check(unsafe { libc::bind(fd, (&address as *const libc::sockaddr_un).cast(), len) })?;
    let endpoint = path.open()?;
    let info = endpoint.metadata()?;
    if !info.file_type().is_socket() || info.uid() != 0 || info.nlink() != 1 {
        return Err(invalid());
    }
    let bound = BoundSocket {
        path,
        identity: (info.dev(), info.ino()),
    };
    check(unsafe {
        libc::fchmodat(
            bound.path.parent.as_raw_fd(),
            bound.path.name.as_ptr(),
            mode,
            0,
        )
    })?;
    socket_identity(&endpoint, mode)?;
    check(unsafe { libc::listen(fd, BACKLOG) })?;
    Ok((socket, bound))
}

fn socket_identity(file: &File, mode: u32) -> io::Result<()> {
    let info = file.metadata()?;
    if !info.file_type().is_socket()
        || info.uid() != 0
        || info.nlink() != 1
        || info.mode() & 0o7777 != mode
    {
        return Err(invalid());
    }
    Ok(())
}

fn socket_path(path: &Path, parent_mode: Option<u32>) -> io::Result<SocketPath> {
    checked_path(path)?;
    let parent = path.parent().ok_or_else(invalid)?;
    let name = path.file_name().ok_or_else(invalid)?;
    Ok(SocketPath {
        parent: protected_directory(parent, parent_mode)?,
        name: CString::new(name.as_bytes()).map_err(|_| invalid())?,
    })
}

fn checked_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > 4095
        || path.as_os_str() != path.components().collect::<PathBuf>().as_os_str()
    {
        return Err(invalid());
    }
    Ok(())
}

fn protected_directory(path: &Path, mode: Option<u32>) -> io::Result<File> {
    checked_path(path)?;
    let mut directory = open_at(libc::AT_FDCWD, c"/", libc::O_DIRECTORY)?;
    check_directory(&directory)?;
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        let name = CString::new(name.as_bytes()).map_err(|_| invalid())?;
        directory = open_at(directory.as_raw_fd(), &name, libc::O_DIRECTORY)?;
        check_directory(&directory)?;
    }
    if let Some(mode) = mode {
        if directory.metadata()?.mode() & 0o7777 != mode {
            return Err(invalid());
        }
    }
    Ok(directory)
}

fn check_directory(directory: &File) -> io::Result<()> {
    let info = directory.metadata()?;
    if info.uid() != 0 || info.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    Ok(())
}

fn open_at(fd: RawFd, name: &std::ffi::CStr, flags: libc::c_int) -> io::Result<File> {
    let flags = flags | libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(fd, name.as_ptr(), flags) };
    Ok(File::from(owned(fd)?))
}

fn unix_socket(kind: libc::c_int) -> io::Result<OwnedFd> {
    owned(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC, 0) })
}

fn accept(listener: RawFd) -> io::Result<OwnedFd> {
    owned(unsafe {
        libc::accept4(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    })
}

fn unix_address(path: &[u8]) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    if path.len() >= address.sun_path.len() || path.contains(&0) {
        return Err(invalid());
    }
    address.sun_family = libc::AF_UNIX as _;
    for (slot, byte) in address.sun_path.iter_mut().zip(path) {
        *slot = *byte as libc::c_char;
    }
    let len = offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    Ok((address, len as libc::socklen_t))
}

fn owned(fd: RawFd) -> io::Result<OwnedFd> {
    check(fd)?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_accepts_the_explicit_global_registry_before_the_subcommand() {
        use clap::Parser;
        let parsed = crate::Cli::try_parse_from([
            "dsg",
            "--state-dir",
            "/run/test/state",
            "docker-image-serve",
            "--policy",
            "/run/test/docker-controller-admission.json",
            "--control",
            "/run/test-public/control",
            "--probe",
            "/run/test/probe",
            "--store-directory",
            "/run/test/store",
        ])
        .unwrap();
        assert_eq!(
            parsed.state_dir.as_deref(),
            Some(Path::new("/run/test/state"))
        );
        assert!(matches!(
            parsed.command,
            crate::Command::DockerImageServe { .. }
        ));
    }

    #[test]
    fn reads_one_object_per_line_up_to_the_frame_limit() {
        let line = format!(r#"{{"a":"{}"}}"#, "x".repeat(MAX_FRAME_BYTES - 8));
        assert_eq!(line.len(), MAX_FRAME_BYTES);
        let stream = format!("{line}\n{{}}\n");
        let mut input = stream.as_bytes();
        let mut bytes = Vec::new();
        assert_eq!(read_frame(&mut input, &mut bytes).unwrap(), Frame::Complete);
        assert_eq!(bytes, line.as_bytes());
        assert!(is_json_object(&bytes));
        assert_eq!(read_frame(&mut input, &mut bytes).unwrap(), Frame::Complete);
        assert_eq!(bytes.as_slice(), b"{}");
        assert_eq!(read_frame(&mut input, &mut bytes).unwrap(), Frame::End);
    }

    #[test]
    fn rejects_oversized_unterminated_and_non_object_frames() {
        let oversized = format!("{}\n", " ".repeat(MAX_FRAME_BYTES + 1));
        for mut input in [oversized.as_bytes(), b"{}"] {
            let mut bytes = Vec::new();
            assert_eq!(
                read_frame(&mut input, &mut bytes).unwrap(),
                Frame::Incomplete
            );
            assert!(bytes.len() <= MAX_FRAME_BYTES + 1);
        }
        for input in [b"[]".as_slice(), b"{} {}", b"{"] {
            assert!(!is_json_object(input));
        }
    }
}
