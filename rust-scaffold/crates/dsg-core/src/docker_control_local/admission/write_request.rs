use super::*;
use std::mem::size_of;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr;

const MAX_REQUEST_BYTES: usize = 512;
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);
// Linux's SCM_PIDFD ancillary type, introduced with SO_PASSPIDFD in Linux 6.5.
const SCM_PIDFD: libc::c_int = 4;
const CREDENTIAL_BYTES: libc::c_uint = size_of::<libc::ucred>() as _;
const FD_BYTES: libc::c_uint = size_of::<RawFd>() as _;
const CONTROL_BYTES: usize =
    unsafe { (libc::CMSG_SPACE(CREDENTIAL_BYTES) + libc::CMSG_SPACE(FD_BYTES)) as usize };

#[repr(C)]
struct ControlBuffer {
    _align: [libc::cmsghdr; 0],
    bytes: [u8; CONTROL_BYTES],
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteRequest {
    Approve { project_id: i64, plan_id: i64 },
    Apply { project_id: i64, plan_id: i64 },
    Recover { project_id: i64, intent_id: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope {
    protocol_version: u8,
    request: WriteRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    Approver,
    Executor,
}

impl Role {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Approver => b"approver",
            Self::Executor => b"executor",
        }
    }
}

impl WriteRequest {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let envelope: RequestEnvelope =
            serde_json::from_slice(bytes).map_err(|_| QueryFailure::Malformed)?;
        if envelope.protocol_version != 1 {
            return Err(QueryFailure::Malformed);
        }
        let valid_selector = match &envelope.request {
            Self::Approve {
                project_id,
                plan_id,
            }
            | Self::Apply {
                project_id,
                plan_id,
            } => *project_id > 0 && *plan_id > 0,
            Self::Recover {
                project_id,
                intent_id,
            } => *project_id > 0 && valid_hash(intent_id),
        };
        if !valid_selector {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(envelope.request)
    }

    fn role(&self) -> Role {
        match self {
            Self::Approve { .. } => Role::Approver,
            Self::Apply { .. } | Self::Recover { .. } => Role::Executor,
        }
    }
}

/// Authority for one request in the current dispatch, issued only by the local admission gate.
pub struct AuthorizedWriteRequest<'a> {
    request: &'a WriteRequest,
    policy: &'a ProtectedPolicy<'a>,
    peers: &'a PinnedPeers<'a>,
    sender: &'a Sender,
    controller: &'a ControllerEvidence,
    engine: &'a ControllerEvidence,
    approver: &'a ControllerEvidence,
    executor: &'a ControllerEvidence,
}

impl AuthorizedWriteRequest<'_> {
    pub fn request(&self) -> &WriteRequest {
        self.request
    }

    pub fn policy_digest(&self) -> &str {
        &self.policy.digest
    }

    pub fn controller(&self) -> &ControllerEvidence {
        self.controller
    }

    pub fn engine(&self) -> &ControllerEvidence {
        self.engine
    }

    pub fn role(&self) -> &ControllerEvidence {
        match self.request.role() {
            Role::Approver => self.approver,
            Role::Executor => self.executor,
        }
    }

    pub(crate) fn pin_backend_ownership(
        &self,
    ) -> Result<super::backend_ownership::PinnedBackendOwnership> {
        super::backend_ownership::PinnedBackendOwnership::open(self.policy)
    }

    pub(crate) fn approver(&self) -> &ControllerEvidence {
        self.approver
    }

    pub(crate) fn executor(&self) -> &ControllerEvidence {
        self.executor
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        let deadline = Instant::now() + ADMISSION_TIMEOUT;
        self.peers.check_current(deadline)?;
        self.policy.check_current()?;
        check_alive(&self.sender.process)?;
        remaining(deadline)?;
        Ok(())
    }
}

/// Enable per-message credentials on a dedicated AF_UNIX SOCK_SEQPACKET socket before listening.
/// Accepted sockets inherit these options. Supplying the sending process descriptor requires
/// Linux 6.5 or later.
pub fn configure_write_request_listener(listener: BorrowedFd<'_>) -> Result<()> {
    let fd = listener.as_raw_fd();
    check_seqpacket(fd)?;
    if get_int_option(fd, libc::SO_ACCEPTCONN)? != 0 || peer_credentials(fd)?.pid > 0 {
        return Err(QueryFailure::InvalidSelector);
    }
    enable_credentials(fd)
}

/// Check one request on a connection accepted from the configured listener in the protected controller.
/// The kernel authenticates the record and ties it to its original process generation.
/// The connection is consumed. Dispatch can reply on the borrowed socket before it is closed.
/// A rejected packet never calls dispatch.
///
/// The adapter must compare the bound scope to its exact project and plan or pending intent. It
/// must keep pending intents associated with the underlying engine across policy/role generation
/// changes and hold exclusion while revalidating and applying a separately recorded approval.
pub fn with_authorized_write_request<T>(
    policy_path: &Path,
    socket: OwnedFd,
    dispatch: impl FnOnce(AuthorizedWriteRequest<'_>, BorrowedFd<'_>) -> T,
) -> Result<T> {
    let deadline = Instant::now() + ADMISSION_TIMEOUT;
    let fd = socket.as_raw_fd();
    let connection_peer = prepare_socket(fd)?;
    if connection_peer.pid <= 0 || connection_peer.uid == 0 {
        return Err(QueryFailure::InvalidSelector);
    }

    let policy = ProtectedPolicy::open(policy_path)?;
    if u32::try_from(policy.value.controller.pid).ok() != Some(std::process::id()) {
        return Err(QueryFailure::InvalidSelector);
    }
    let mut payload = [0; MAX_REQUEST_BYTES];
    let (count, sender) = receive(fd, &mut payload, deadline)?;
    if !matches_peer(&connection_peer, &sender.credentials) {
        return Err(QueryFailure::InvalidSelector);
    }

    let request = WriteRequest::parse(&payload[..count])?;
    let role = request.role();
    let selected = match role {
        Role::Approver => &policy.value.approver,
        Role::Executor => &policy.value.executor,
    };
    check_role(&sender, selected)?;
    let peers = PinnedPeers::open(&policy.value, deadline)?;
    // The pinned path and the actual upstream socket must identify the same engine process.
    peers.engine.connect()?;
    let controller = peers.controller.evidence(b"controller", &policy.boot_id);
    let engine = peers.engine.evidence(b"engine", &policy.boot_id);
    let approver = peers
        .approver
        .evidence(Role::Approver.name(), &policy.boot_id);
    let executor = peers
        .executor
        .evidence(Role::Executor.name(), &policy.boot_id);

    peers.check_current(deadline)?;
    policy.check_current()?;
    check_alive(&sender.process)?;
    remaining(deadline)?;
    Ok(dispatch(
        AuthorizedWriteRequest {
            request: &request,
            policy: &policy,
            peers: &peers,
            sender: &sender,
            controller: &controller,
            engine: &engine,
            approver: &approver,
            executor: &executor,
        },
        socket.as_fd(),
    ))
}

struct Sender {
    credentials: libc::ucred,
    process: File,
}

fn matches_peer(connection: &libc::ucred, message: &libc::ucred) -> bool {
    (connection.pid, connection.uid) == (message.pid, message.uid)
}

fn check_role(sender: &Sender, expected: &Peer) -> Result<()> {
    check_sender(sender, expected.pid, expected.uid)
}

fn check_sender(sender: &Sender, pid: i32, uid: u32) -> Result<()> {
    if sender.credentials.uid == 0 || sender.credentials.pid != pid || sender.credentials.uid != uid
    {
        return Err(QueryFailure::InvalidSelector);
    }
    check_alive(&sender.process)?;
    let info = File::open(format!("/proc/self/fdinfo/{}", sender.process.as_raw_fd()))
        .map_err(io_failure)?;
    let info = bounded_read(info, 4096)?;
    if credential_field(&info, b"Pid:")?.parse::<i32>() != Ok(pid) {
        return Err(QueryFailure::InvalidSelector);
    }
    check_alive(&sender.process)
}

/// Receive a single local role account's packet with a kernel-pinned, live sender.
/// Both connection and per-message credentials must identify the same process.
pub(crate) fn receive_account_packet(
    fd: RawFd,
    payload: &mut [u8],
    deadline: Instant,
) -> Result<(usize, u32, File)> {
    let peer = prepare_socket(fd)?;
    if peer.pid <= 0 {
        return Err(QueryFailure::InvalidSelector);
    }
    let (length, sender) = receive(fd, payload, deadline)?;
    // Consume the bounded packet before denying root so the peer can receive
    // the admission response without racing an unread packet at socket close.
    if peer.uid == 0 || !matches_peer(&peer, &sender.credentials) {
        return Err(QueryFailure::InvalidSelector);
    }
    check_sender(&sender, peer.pid, peer.uid)?;
    check_process_uid(peer.pid, peer.uid)?;
    check_alive(&sender.process)?;
    Ok((length, peer.uid, sender.process))
}

fn get_int_option(fd: RawFd, option: libc::c_int) -> Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut length = size_of::<libc::c_int>() as libc::socklen_t;
    // Both output buffers are valid and sized for integer SOL_SOCKET options.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            ptr::from_mut(&mut value).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io_failure(io::Error::last_os_error()));
    }
    if length as usize != size_of::<libc::c_int>() {
        return Err(QueryFailure::Malformed);
    }
    Ok(value)
}

pub(crate) fn check_seqpacket(fd: RawFd) -> Result<()> {
    if get_int_option(fd, libc::SO_DOMAIN)? != libc::AF_UNIX
        || get_int_option(fd, libc::SO_TYPE)? != libc::SOCK_SEQPACKET
    {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(())
}

fn enable_credentials(fd: RawFd) -> Result<()> {
    let enabled: libc::c_int = 1;
    for option in [libc::SO_PASSCRED, libc::SO_PASSPIDFD] {
        // enabled is a valid integer SOL_SOCKET option input.
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                ptr::from_ref(&enabled).cast(),
                size_of::<libc::c_int>() as _,
            )
        } != 0
        {
            return Err(io_failure(io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn prepare_socket(fd: RawFd) -> Result<libc::ucred> {
    check_seqpacket(fd)?;
    if get_int_option(fd, libc::SO_ACCEPTCONN)? != 0
        || get_int_option(fd, libc::SO_PASSCRED)? != 1
        || get_int_option(fd, libc::SO_PASSPIDFD)? != 1
    {
        return Err(QueryFailure::InvalidSelector);
    }
    peer_credentials(fd)
}

fn receive(fd: RawFd, payload: &mut [u8], deadline: Instant) -> Result<(usize, Sender)> {
    let timeout = remaining(deadline)?.min(RECEIVE_TIMEOUT);
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // The caller supplies one open descriptor; the poll buffer stays alive during the call.
    match unsafe { libc::poll(&mut poll, 1, timeout.as_millis().max(1) as _) } {
        0 => return Err(QueryFailure::Timeout),
        1 if poll.revents & libc::POLLIN != 0 => (),
        _ => return Err(QueryFailure::Unavailable),
    }

    let mut control = ControlBuffer {
        _align: [],
        bytes: [0; CONTROL_BYTES],
    };
    let mut segment = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut segment;
    message.msg_iovlen = 1;
    message.msg_control = control.bytes.as_mut_ptr().cast();
    message.msg_controllen = control.bytes.len() as _;
    // A nonblocking recv after poll preserves the time bound if the descriptor is shared.
    let count = unsafe {
        libc::recvmsg(
            fd,
            &mut message,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if count < 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    let sender = read_sender(&message);
    // Drain delivered descriptors via read_sender even if payload or metadata was truncated.
    if message.msg_flags & libc::MSG_TRUNC != 0 {
        return Err(QueryFailure::TooLarge);
    }
    if count == 0 || message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(QueryFailure::Malformed);
    }
    Ok((count as usize, sender?))
}

fn read_sender(message: &libc::msghdr) -> Result<Sender> {
    let mut credentials = None;
    let mut process = None;
    let mut unsupported = false;
    // The kernel populated the aligned ancillary buffer and returns only complete headers.
    let mut item = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !item.is_null() {
        let header = unsafe { &*item };
        let data = unsafe { libc::CMSG_DATA(item) };
        let len = (header.cmsg_len as usize).saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
        match (header.cmsg_level, header.cmsg_type) {
            (libc::SOL_SOCKET, libc::SCM_CREDENTIALS)
                if len == CREDENTIAL_BYTES as usize && credentials.is_none() =>
            {
                credentials = Some(unsafe { data.cast::<libc::ucred>().read_unaligned() });
            }
            (libc::SOL_SOCKET, SCM_PIDFD) if len == FD_BYTES as usize => {
                let fd = unsafe { data.cast::<RawFd>().read_unaligned() };
                if fd < 0 || process.is_some() {
                    unsupported = true;
                }
                if fd >= 0 {
                    // A received pidfd is owned by this process, even when rejecting the packet.
                    process = Some(unsafe { File::from_raw_fd(fd) });
                }
            }
            (libc::SOL_SOCKET, libc::SCM_RIGHTS) => {
                for index in 0..len / FD_BYTES as usize {
                    let fd = unsafe { data.cast::<RawFd>().add(index).read_unaligned() };
                    // Discard installed descriptors. The kernel closes any excess on truncation.
                    drop(unsafe { File::from_raw_fd(fd) });
                }
                unsupported = true;
            }
            _ => unsupported = true,
        }
        item = unsafe { libc::CMSG_NXTHDR(message, item) };
    }
    match (credentials, process) {
        (Some(credentials), Some(process)) if !unsupported => Ok(Sender {
            credentials,
            process,
        }),
        _ => Err(QueryFailure::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of_val;

    const APPROVE: &[u8] =
        br#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":3}}"#;
    const APPLY: &[u8] =
        br#"{"protocol_version":1,"request":{"operation":"apply","project_id":9,"plan_id":3}}"#;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut pair = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    pair.as_mut_ptr(),
                )
            },
            0
        );
        unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) }
    }

    fn send(fd: RawFd, bytes: &[u8]) -> isize {
        unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) }
    }

    fn test_receive(fd: &OwnedFd, bytes: &mut [u8]) -> Result<(usize, Sender)> {
        receive(fd.as_raw_fd(), bytes, Instant::now() + RECEIVE_TIMEOUT)
    }

    #[test]
    fn listener_options_are_inherited_for_records_queued_before_accept() {
        fn socket() -> OwnedFd {
            let fd = unsafe {
                libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0)
            };
            assert!(fd >= 0);
            unsafe { OwnedFd::from_raw_fd(fd) }
        }

        let listener = socket();
        let fd = listener.as_raw_fd();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        let name = format!("dsg-write-request-{}", std::process::id());
        for (slot, byte) in address.sun_path[1..].iter_mut().zip(name.bytes()) {
            *slot = byte as _;
        }
        let size = std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1;
        let address = ptr::from_ref(&address).cast();
        assert_eq!(unsafe { libc::bind(fd, address, size as _) }, 0);
        configure_write_request_listener(listener.as_fd()).unwrap();
        assert_eq!(unsafe { libc::listen(fd, 1) }, 0);

        let sender = socket();
        assert_eq!(
            unsafe { libc::connect(sender.as_raw_fd(), address, size as _) },
            0
        );
        assert_eq!(send(sender.as_raw_fd(), APPROVE), APPROVE.len() as isize);
        let accepted =
            unsafe { libc::accept4(fd, ptr::null_mut(), ptr::null_mut(), libc::SOCK_CLOEXEC) };
        assert!(accepted >= 0);
        let accepted = unsafe { OwnedFd::from_raw_fd(accepted) };
        let peer = prepare_socket(accepted.as_raw_fd()).unwrap();
        let (count, record) = test_receive(&accepted, &mut [0; MAX_REQUEST_BYTES]).unwrap();
        assert_eq!(count, APPROVE.len());
        assert!(matches_peer(&peer, &record.credentials));
    }

    #[test]
    fn write_request_selectors_assign_roles_and_reject_actor_assertions() {
        assert_eq!(WriteRequest::parse(APPROVE).unwrap().role(), Role::Approver);
        assert_eq!(WriteRequest::parse(APPLY).unwrap().role(), Role::Executor);
        let recover = format!(
            r#"{{"protocol_version":1,"request":{{"operation":"recover","project_id":9,"intent_id":"{}"}}}}"#,
            "1f".repeat(32)
        );
        assert_eq!(
            WriteRequest::parse(recover.as_bytes()).unwrap().role(),
            Role::Executor
        );
        for request in [
            r#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":3,"actor":"approver"}}"#,
            r#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":3},"uid":1}"#,
            r#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":3,"plan_id":4}}"#,
            r#"{"protocol_version":2,"request":{"operation":"approve","project_id":9,"plan_id":3}}"#,
            r#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":3}} {}"#,
        ] {
            assert_eq!(
                WriteRequest::parse(request.as_bytes()),
                Err(QueryFailure::Malformed)
            );
        }
        for request in [
            r#"{"protocol_version":1,"request":{"operation":"apply","project_id":0,"plan_id":3}}"#,
            r#"{"protocol_version":1,"request":{"operation":"approve","project_id":9,"plan_id":-1}}"#,
            r#"{"protocol_version":1,"request":{"operation":"recover","project_id":9,"intent_id":"prefix"}}"#,
        ] {
            assert_eq!(
                WriteRequest::parse(request.as_bytes()),
                Err(QueryFailure::InvalidSelector)
            );
        }
    }

    #[test]
    fn record_has_kernel_credentials_and_a_live_sending_process() {
        let (receiver, sender) = pair();
        let fd = receiver.as_raw_fd();
        assert_eq!(
            prepare_socket(fd).err(),
            Some(QueryFailure::InvalidSelector)
        );
        enable_credentials(fd).unwrap();
        let connection = prepare_socket(fd).unwrap();
        assert_eq!(send(sender.as_raw_fd(), APPROVE), APPROVE.len() as isize);
        assert_eq!(send(sender.as_raw_fd(), APPLY), APPLY.len() as isize);
        let mut payload = [0; MAX_REQUEST_BYTES];
        let (count, mut record) = test_receive(&receiver, &mut payload).unwrap();
        assert_eq!(&payload[..count], APPROVE);
        assert!(matches_peer(&connection, &record.credentials));
        assert_eq!(record.credentials.pid, std::process::id() as i32);
        assert_eq!(record.credentials.uid, unsafe { libc::getuid() });

        // These synthetic policy roles test the credential predicate independently of test UID.
        let mut policy = super::super::tests::policy();
        policy.approver.pid = record.credentials.pid;
        record.credentials.uid = policy.approver.uid;
        assert_eq!(check_role(&record, &policy.approver), Ok(()));
        assert_eq!(
            check_role(&record, &policy.executor),
            Err(QueryFailure::InvalidSelector)
        );
        policy.approver.pid ^= 1;
        assert_eq!(
            check_role(&record, &policy.approver),
            Err(QueryFailure::InvalidSelector)
        );
        policy.approver.pid = record.credentials.pid;
        record.credentials.uid = 0;
        assert_eq!(
            check_role(&record, &policy.approver),
            Err(QueryFailure::InvalidSelector)
        );

        let (count, _) = test_receive(&receiver, &mut payload).unwrap();
        assert_eq!(&payload[..count], APPLY);
    }

    #[test]
    fn fd_handoff_cannot_reuse_the_original_connection_identity() {
        let (receiver, sender) = pair();
        enable_credentials(receiver.as_raw_fd()).unwrap();
        let connection = prepare_socket(receiver.as_raw_fd()).unwrap();
        let mut release = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(release.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let fd = sender.as_raw_fd();
        let child = unsafe { libc::fork() };
        if child == 0 {
            // After fork the child uses only async-signal-safe libc calls and exits immediately.
            unsafe { libc::close(release[1]) };
            let sent = send(fd, APPROVE) == APPROVE.len() as isize
                && send(fd, APPROVE) == APPROVE.len() as isize;
            let mut byte = 0u8;
            unsafe {
                libc::read(release[0], ptr::from_mut(&mut byte).cast(), 1);
                libc::_exit(i32::from(!sent));
            }
        }
        let release_writer = unsafe { OwnedFd::from_raw_fd(release[1]) };
        drop(unsafe { OwnedFd::from_raw_fd(release[0]) });
        assert!(child > 0);
        let live = test_receive(&receiver, &mut [0; MAX_REQUEST_BYTES]).map(|(_, record)| {
            (
                record.credentials.pid,
                matches_peer(&connection, &record.credentials),
            )
        });
        drop(release_writer);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert_eq!(live, Ok((child, false)));

        let result = test_receive(&receiver, &mut [0; MAX_REQUEST_BYTES]);
        match result {
            // Some kernels can still supply a pidfd for an exited sender's queued message.
            Ok((_, record)) => {
                assert!(!matches_peer(&connection, &record.credentials));
                assert_eq!(check_alive(&record.process), Err(QueryFailure::Unavailable));
            }
            Err(failure) => assert_eq!(failure, QueryFailure::Malformed),
        }
    }

    #[test]
    fn rejects_late_configuration_record_and_payload_truncation() {
        let (receiver, sender) = pair();
        assert_eq!(send(sender.as_raw_fd(), APPROVE), APPROVE.len() as isize);
        enable_credentials(receiver.as_raw_fd()).unwrap();
        assert_eq!(
            test_receive(&receiver, &mut [0; MAX_REQUEST_BYTES]).err(),
            Some(QueryFailure::Malformed)
        );

        let oversized = [b'x'; MAX_REQUEST_BYTES + 1];
        assert_eq!(
            send(sender.as_raw_fd(), &oversized),
            oversized.len() as isize
        );
        assert_eq!(
            test_receive(&receiver, &mut [0; MAX_REQUEST_BYTES]).err(),
            Some(QueryFailure::TooLarge)
        );
    }

    #[test]
    fn rejects_ancillary_truncation_and_closes_all_received_fds() {
        let (receiver, sender) = pair();
        enable_credentials(receiver.as_raw_fd()).unwrap();
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let (mut reader, writer) =
            unsafe { (File::from_raw_fd(pipe[0]), File::from_raw_fd(pipe[1])) };
        let rights = [writer.as_raw_fd(); 8];
        let mut control = [0usize; 8];
        let mut bytes = *b"x";
        let mut segment = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut segment;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = unsafe { libc::CMSG_SPACE(size_of_val(&rights) as _) } as _;
        unsafe {
            let item = libc::CMSG_FIRSTHDR(&message);
            (*item).cmsg_level = libc::SOL_SOCKET;
            (*item).cmsg_type = libc::SCM_RIGHTS;
            (*item).cmsg_len = libc::CMSG_LEN(size_of_val(&rights) as _) as _;
            libc::CMSG_DATA(item)
                .cast::<RawFd>()
                .copy_from_nonoverlapping(rights.as_ptr(), rights.len());
            assert_eq!(
                libc::sendmsg(sender.as_raw_fd(), &message, libc::MSG_NOSIGNAL),
                1
            );
        }
        drop(writer);
        assert_eq!(
            test_receive(&receiver, &mut [0; MAX_REQUEST_BYTES]).err(),
            Some(QueryFailure::Malformed)
        );
        // A leaked copy of the write FD would keep the pipe readable only with EAGAIN.
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    }
}
