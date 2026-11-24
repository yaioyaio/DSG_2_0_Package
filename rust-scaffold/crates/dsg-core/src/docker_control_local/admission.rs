use super::*;

mod backend_ownership;
mod write_request;
pub use backend_ownership::{
    verify_backend_ownership, BackendOwnershipEvidence, BackendOwnershipFailure,
};
pub use write_request::{
    configure_write_request_listener, with_authorized_write_request, AuthorizedWriteRequest,
    WriteRequest,
};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(8);
const POLICY_FILENAME: &str = "docker-controller-admission.json";
const MAX_POLICY_BYTES: u64 = 8192;
const MAX_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;

/// Point-in-time observations about protected local sockets and their peers.
#[derive(Debug, Serialize)]
pub struct AdmissionEvidence {
    pub controller: ControllerEvidence,
    pub engine: ControllerEvidence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u8,
    boot_id: String,
    controller: Peer,
    engine: Peer,
    approver: Peer,
    executor: Peer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    socket: PathBuf,
    pid: i32,
    uid: u32,
    start_ticks: u64,
    executable: PathBuf,
    executable_sha256: String,
}

impl Policy {
    fn peers(&self) -> [&Peer; 4] {
        [
            &self.controller,
            &self.engine,
            &self.approver,
            &self.executor,
        ]
    }

    fn validate(&self, boot_id: &str) -> Result<()> {
        if self.version != 1
            || !same_boot(&self.boot_id, boot_id)
            || self.controller.uid != 0
            || self.engine.uid != 0
            || self.approver.uid == 0
            || self.executor.uid == 0
            || self.approver.uid == self.executor.uid
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let peers = self.peers();
        for (index, peer) in peers.iter().enumerate() {
            if peer.pid <= 0
                || peer.start_ticks == 0
                || !valid_hash(&peer.executable_sha256)
                || peers[..index]
                    .iter()
                    .any(|other| other.pid == peer.pid || other.socket == peer.socket)
            {
                return Err(QueryFailure::InvalidSelector);
            }
        }
        Ok(())
    }
}

fn same_boot(configured: &str, current: &str) -> bool {
    configured.len() == 36 && configured == current.trim_end_matches('\n')
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Verify protected deployment metadata against live Linux process and socket identity.
/// This API only probes the controller. It does not issue an engine API request, accept
/// per-resource authorization, or create a mutation-capable adapter.
pub fn verify_admission(path: &Path) -> Result<AdmissionEvidence> {
    let deadline = Instant::now() + ADMISSION_TIMEOUT;
    let policy = ProtectedPolicy::open(path)?;
    let peers = PinnedPeers::open(&policy.value, deadline)?;
    let mut connection = peers.controller.connect()?;
    peers.engine.connect()?;
    peers.approver.connect()?;
    peers.executor.connect()?;
    exchange(&mut connection, deadline)?;
    peers.check_current(deadline)?;
    policy.check_current()?;
    remaining(deadline)?;

    Ok(AdmissionEvidence {
        controller: peers.controller.evidence(b"controller", &policy.boot_id),
        engine: peers.engine.evidence(b"engine", &policy.boot_id),
    })
}

struct ProtectedPolicy<'a> {
    path: &'a Path,
    value: Policy,
    identity: fs::Metadata,
    boot_id: String,
    digest: String,
}

impl<'a> ProtectedPolicy<'a> {
    fn open(path: &'a Path) -> Result<Self> {
        if path.file_name() != Some(OsStr::new(POLICY_FILENAME)) {
            return Err(QueryFailure::InvalidSelector);
        }
        let descriptor = open_protected(path, false)?;
        let info = descriptor.metadata().map_err(io_failure)?;
        if !info.is_file() || info.uid() != 0 || info.mode() & 0o7777 != 0o444 || info.nlink() != 1
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let bytes = bounded_read(readable(&descriptor)?, MAX_POLICY_BYTES)?;
        compare_metadata(&info, &descriptor.metadata().map_err(io_failure)?)?;
        let value: Policy = serde_json::from_slice(&bytes).map_err(|_| QueryFailure::Malformed)?;
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(io_failure)?;
        value.validate(&boot_id)?;
        Ok(Self {
            path,
            value,
            identity: info,
            boot_id,
            digest: format!("{:x}", Sha256::digest(&bytes)),
        })
    }

    fn check_current(&self) -> Result<()> {
        compare_metadata(
            &self.identity,
            &open_protected(self.path, false)?
                .metadata()
                .map_err(io_failure)?,
        )
    }
}

fn readable(descriptor: &File) -> Result<File> {
    // Reopen the already-validated O_PATH inode, independent of changes to the pathname.
    File::open(format!("/proc/self/fd/{}", descriptor.as_raw_fd())).map_err(io_failure)
}

fn bounded_read(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    if bytes.len() as u64 > limit {
        return Err(QueryFailure::TooLarge);
    }
    Ok(bytes)
}

fn same_inode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    (left.dev(), left.ino()) == (right.dev(), right.ino())
}

fn compare_metadata(expected: &fs::Metadata, current: &fs::Metadata) -> Result<()> {
    // ctime covers writes, link changes, permission changes, and ownership changes.
    if !same_inode(expected, current)
        || expected.ctime() != current.ctime()
        || expected.ctime_nsec() != current.ctime_nsec()
    {
        return Err(QueryFailure::Unavailable);
    }
    Ok(())
}

struct PinnedPeer<'a> {
    policy: &'a Peer,
    process: File,
    socket: File,
    socket_identity: (u64, u64),
    executable: fs::Metadata,
}

struct PinnedPeers<'a> {
    controller: PinnedPeer<'a>,
    engine: PinnedPeer<'a>,
    approver: PinnedPeer<'a>,
    executor: PinnedPeer<'a>,
}

impl<'a> PinnedPeers<'a> {
    fn open(policy: &'a Policy, deadline: Instant) -> Result<Self> {
        let peers = Self {
            controller: PinnedPeer::open(&policy.controller, deadline)?,
            engine: PinnedPeer::open(&policy.engine, deadline)?,
            approver: PinnedPeer::open(&policy.approver, deadline)?,
            executor: PinnedPeer::open(&policy.executor, deadline)?,
        };
        let all = peers.all();
        for (index, peer) in all.iter().enumerate() {
            if all[..index]
                .iter()
                .any(|other| other.socket_identity == peer.socket_identity)
            {
                return Err(QueryFailure::InvalidSelector);
            }
        }
        Ok(peers)
    }

    fn all(&self) -> [&PinnedPeer<'a>; 4] {
        [
            &self.controller,
            &self.engine,
            &self.approver,
            &self.executor,
        ]
    }

    fn check_current(&self, deadline: Instant) -> Result<()> {
        for peer in self.all() {
            remaining(deadline)?;
            peer.check_current()?;
        }
        Ok(())
    }
}

impl<'a> PinnedPeer<'a> {
    fn open(policy: &'a Peer, deadline: Instant) -> Result<Self> {
        // A PID descriptor pins the process identity across the checks and socket exchange.
        let process =
            owned_file(unsafe { libc::syscall(libc::SYS_pidfd_open, policy.pid, 0) as RawFd })?;
        let socket = open_socket(&policy.socket)?;
        let socket_identity = socket_identity(&socket)?;
        let executable_path = open_protected(&policy.executable, false)?;
        let executable = executable_path.metadata().map_err(io_failure)?;
        if !executable.is_file()
            || executable.uid() != 0
            || executable.mode() & 0o022 != 0
            || executable.mode() & 0o111 == 0
            || executable.nlink() != 1
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let peer = Self {
            policy,
            process,
            socket,
            socket_identity,
            executable,
        };
        peer.check_current()?;
        let digest = executable_hash(&executable_path, &peer.executable, deadline)?;
        if digest != policy.executable_sha256 {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(peer)
    }

    fn check_current(&self) -> Result<()> {
        let pid = self.policy.pid;
        check_alive(&self.process)?;
        if process_start(pid)? != self.policy.start_ticks
            || socket_identity(&open_socket(&self.policy.socket)?)? != self.socket_identity
        {
            return Err(QueryFailure::InvalidSelector);
        }
        compare_metadata(
            &self.executable,
            &open_protected(&self.policy.executable, false)?
                .metadata()
                .map_err(io_failure)?,
        )?;
        let running = fs::metadata(format!("/proc/{pid}/exe")).map_err(io_failure)?;
        if !same_inode(&self.executable, &running) {
            return Err(QueryFailure::InvalidSelector);
        }
        check_process_uid(pid, self.policy.uid)?;
        check_alive(&self.process)
    }

    fn connect(&self) -> Result<UnixStream> {
        let stream = connect(&self.socket)?;
        check_peer(&stream, self.policy.pid, self.policy.uid)?;
        Ok(stream)
    }

    fn evidence(&self, role: &[u8], boot_id: &str) -> ControllerEvidence {
        let mut endpoint = Sha256::new();
        endpoint.update(b"dsg-docker-admission-endpoint-v1\0");
        endpoint.update(role);
        endpoint.update(b"\0");
        endpoint.update(self.policy.socket.as_os_str().as_bytes());
        let endpoint = endpoint.finalize();
        let mut generation = Sha256::new();
        generation.update(b"dsg-docker-admission-generation-v1\0");
        generation.update(endpoint);
        generation.update(self.policy.pid.to_be_bytes());
        generation.update(self.policy.start_ticks.to_be_bytes());
        generation.update(self.socket_identity.0.to_be_bytes());
        generation.update(self.socket_identity.1.to_be_bytes());
        generation.update(boot_id.as_bytes());
        generation.update(self.policy.executable_sha256.as_bytes());
        ControllerEvidence {
            endpoint: format!("{endpoint:x}"),
            generation: format!("{:x}", generation.finalize()),
        }
    }
}

fn check_process_uid(pid: i32, expected_uid: u32) -> Result<()> {
    let status = File::open(format!("/proc/{pid}/status")).map_err(io_failure)?;
    let bytes = bounded_read(status, 16 * 1024)?;
    validate_credentials(&bytes, expected_uid)
}

fn credential_field<'a>(status: &'a [u8], name: &[u8]) -> Result<&'a str> {
    let value = status
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(name))
        .ok_or(QueryFailure::Unavailable)?;
    std::str::from_utf8(value)
        .map(str::trim_ascii)
        .map_err(|_| QueryFailure::Unavailable)
}

fn validate_credentials(status: &[u8], expected_uid: u32) -> Result<()> {
    let mut uids = credential_field(status, b"Uid:")?.split_ascii_whitespace();
    for _ in 0..4 {
        if uids.next().and_then(|uid| uid.parse().ok()) != Some(expected_uid) {
            return Err(QueryFailure::InvalidSelector);
        }
    }
    if uids.next().is_some() {
        return Err(QueryFailure::InvalidSelector);
    }
    if expected_uid != 0 {
        for name in [b"CapInh:", b"CapPrm:", b"CapEff:", b"CapAmb:"] {
            if u64::from_str_radix(credential_field(status, name)?, 16) != Ok(0) {
                return Err(QueryFailure::InvalidSelector);
            }
        }
    }
    Ok(())
}

fn executable_hash(file: &File, info: &fs::Metadata, deadline: Instant) -> Result<String> {
    if info.len() > MAX_EXECUTABLE_BYTES {
        return Err(QueryFailure::TooLarge);
    }
    let mut reader = readable(file)?.take(MAX_EXECUTABLE_BYTES + 1);
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut length = 0;
    loop {
        remaining(deadline)?;
        let count = reader.read(&mut buffer).map_err(io_failure)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > MAX_EXECUTABLE_BYTES {
            return Err(QueryFailure::TooLarge);
        }
        digest.update(&buffer[..count]);
    }
    compare_metadata(info, &file.metadata().map_err(io_failure)?)?;
    if length != info.len() {
        return Err(QueryFailure::Unavailable);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT_ID: &str = "a7436228-7523-4f95-8695-3be87badd21a";

    fn peer(uid: u32, pid: i32, socket: &str) -> Peer {
        Peer {
            socket: PathBuf::from(socket),
            pid,
            uid,
            start_ticks: 42,
            executable: PathBuf::from("/usr/bin/fixture"),
            executable_sha256: "f".repeat(64),
        }
    }

    pub(super) fn policy() -> Policy {
        Policy {
            version: 1,
            boot_id: BOOT_ID.to_owned(),
            controller: peer(0, 1, "/run/fixture/control.sock"),
            engine: peer(0, 2, "/run/fixture/engine.sock"),
            approver: peer(101, 3, "/run/fixture/approver.sock"),
            executor: peer(102, 4, "/run/fixture/executor.sock"),
        }
    }

    #[test]
    fn policy_rejects_reused_peer_roles_and_wrong_engine() {
        assert_eq!(policy().validate(BOOT_ID), Ok(()));
        assert_eq!(
            policy().validate("wrong-boot"),
            Err(QueryFailure::InvalidSelector)
        );
        let mut same_uid = policy();
        same_uid.executor.uid = same_uid.approver.uid;
        let mut wrong_engine_uid = policy();
        wrong_engine_uid.engine.uid = wrong_engine_uid.executor.uid;
        let mut same_pid = policy();
        same_pid.executor.pid = same_pid.approver.pid;
        let mut same_socket = policy();
        same_socket.engine.socket = same_socket.controller.socket.clone();
        let mut missing_digest = policy();
        missing_digest.engine.executable_sha256.clear();
        for invalid in [
            same_uid,
            wrong_engine_uid,
            same_pid,
            same_socket,
            missing_digest,
        ] {
            assert_eq!(
                invalid.validate(BOOT_ID),
                Err(QueryFailure::InvalidSelector)
            );
        }
    }

    #[test]
    fn protected_policy_rejects_untrusted_and_unexpected_paths() {
        for path in ["docker-controller-admission.json", "/etc/another.json"] {
            assert_eq!(
                verify_admission(Path::new(path)).unwrap_err(),
                QueryFailure::InvalidSelector
            );
        }
    }

    #[test]
    fn kernel_credentials_must_match_the_distinct_role() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        let uid = unsafe { libc::geteuid() };
        assert_eq!(check_peer(&stream, pid, uid), Ok(()));
        assert_eq!(
            check_peer(&stream, pid, uid ^ 1),
            Err(QueryFailure::InvalidSelector)
        );
        assert_eq!(
            check_peer(&stream, pid ^ 1, uid),
            Err(QueryFailure::InvalidSelector)
        );
    }

    #[test]
    fn operating_role_rejects_mismatched_saved_uid_and_capabilities() {
        let status = "Uid:\t101\t101\t101\t101\nCapInh:\t0\nCapPrm:\t0\nCapEff:\t0\nCapAmb:\t0\n";
        assert_eq!(validate_credentials(status.as_bytes(), 101), Ok(()));
        assert_eq!(
            validate_credentials(
                status.replace("101\t101\t101", "101\t0\t101").as_bytes(),
                101
            ),
            Err(QueryFailure::InvalidSelector)
        );
        assert_eq!(
            validate_credentials(status.replace("CapPrm:\t0", "CapPrm:\t2").as_bytes(), 101),
            Err(QueryFailure::InvalidSelector)
        );
    }
}
