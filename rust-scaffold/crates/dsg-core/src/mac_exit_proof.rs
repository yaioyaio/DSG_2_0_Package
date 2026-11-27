//! Exit evidence for a managed child which called `setsid` before running user code.
use crate::runner::TreeExitFailure;
use std::collections::HashSet;
use std::ffi::CStr;
use std::io;
use std::mem::size_of;
use std::ptr;
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const PROOF_BUDGET: Duration = Duration::from_millis(150);
const SCAN_ATTEMPTS: u8 = 8;
const SCAN_RETRY_DELAY: Duration = Duration::from_millis(8);

#[derive(Clone, Debug)]
pub(crate) struct MacExitProof {
    pub(crate) boot: String,
    pub(crate) generation: i64,
    pub(crate) version: u32,
}

// XNU sys/proc_info_private.h: PROC_PIDUNIQIDENTIFIERINFO, with arg=1 to
// include zombies. original_parent_version is recorded at fork and survives
// both reparenting and exec. parent_generation need not survive their combination.
#[repr(C)]
#[derive(Default)]
struct Identifier {
    executable_uuid: [u8; 16],
    generation: u64,
    parent_generation: u64,
    version: u32,
    original_parent_version: u32,
    reserved: [u64; 2],
}
const _: () = assert!(size_of::<Identifier>() == 56);

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete macOS process evidence",
    )
}

fn identifier(pid: libc::pid_t) -> io::Result<Identifier> {
    let mut info = Identifier::default();
    let size = size_of::<Identifier>() as libc::c_int;
    let count = unsafe { libc::proc_pidinfo(pid, 17, 1, ptr::from_mut(&mut info).cast(), size) };
    if count != size || info.generation == 0 || info.generation > i64::MAX as u64 {
        return Err(invalid());
    }
    Ok(info)
}

fn uuid_valid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}

fn boot_id() -> io::Result<String> {
    let mut buf = [0u8; 37];
    let mut len = buf.len();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != buf.len() || buf[36] != 0 {
        return Err(invalid());
    }
    let uuid = std::str::from_utf8(&buf[..36]).map_err(|_| invalid())?;
    if !uuid_valid(uuid) {
        return Err(invalid());
    }
    Ok(uuid.to_owned())
}

// The child performs only async-signal-safe _exit. Read its identity while
// still waitable, then reap exactly this child even when the identity failed.
fn marker(deadline: Instant) -> io::Result<u64> {
    if Instant::now() >= deadline {
        return Err(invalid());
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { libc::_exit(0) }
    }
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    let info = identifier(pid);
    let mut status = 0;
    loop {
        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
        if ret == -1 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        } else if ret == pid {
            if libc::WIFSTOPPED(status) {
                // This is our stopped, waitable marker child.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            } else if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                return info.map(|i| i.generation);
            } else {
                return Err(invalid());
            }
        }
        if Instant::now() >= deadline {
            // A delayed exit must not hold the SQLite writer or settle a lease.
            let _ = thread::Builder::new()
                .name("dsg-proof-reaper".into())
                .spawn(move || {
                    while unsafe { libc::waitpid(pid, ptr::null_mut(), 0) } < 0
                        && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
                    {
                    }
                });
            return Err(invalid());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

// Under XNU's process-list lock, live processes precede PID 0 (the kernel
// process at the tail of allproc), followed by zombies. Require the sentinel
// in the returned bytes, never in unused buffer capacity. Truncation through
// the live list cannot then be mistaken for complete coverage.
fn live_pids() -> io::Result<Vec<libc::pid_t>> {
    const MAX_PIDS: usize = 8192;
    let mut pids = vec![-1; MAX_PIDS];
    let size = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    let bytes = unsafe { libc::proc_listpids(1, 0, pids.as_mut_ptr().cast(), size) };
    if bytes <= 0 || bytes > size || bytes as usize % size_of::<libc::pid_t>() != 0 {
        return Err(invalid());
    }
    pids.truncate(bytes as usize / size_of::<libc::pid_t>());
    let kernel = pids.iter().position(|pid| *pid == 0).ok_or_else(invalid)?;
    if !pids[..kernel].contains(&1) || pids[..kernel].iter().any(|pid| *pid < 0) {
        return Err(invalid());
    }
    pids.truncate(kernel);
    Ok(pids)
}

fn supports_original_parent_versions() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        if !cfg!(target_arch = "aarch64") {
            return false;
        }
        let mut system: libc::utsname = unsafe { std::mem::zeroed() };
        if unsafe { libc::uname(&mut system) } != 0 {
            return false;
        }
        let release = unsafe { CStr::from_ptr(system.release.as_ptr()) };
        let build = unsafe { CStr::from_ptr(system.version.as_ptr()) };
        release.to_bytes().starts_with(b"25.")
            && build
                .to_str()
                .is_ok_and(|v| v.contains("xnu-12377.") && v.contains("/RELEASE_ARM64_"))
    })
}

// XNU 12377 kern_fork.c allocates one global pidversion per fork generation,
// including failed forks. In kern_exec.c both execve and spawn create a new
// main thread before image activation. At most three activator iterations
// (EAI_ITERLIMIT) can assign an exec version. Before the first 32-bit wrap,
// the skip-previous-version loop cannot repeat an already allocated value.
// thread.c assigns monotonically increasing 64-bit IDs starting above 100.
// A thread born AFTER the closing marker bounds every exec in this scan.
fn version_upper_bound(closing_generation: u64, deadline: Instant) -> Option<u64> {
    if !supports_original_parent_versions() || Instant::now() >= deadline {
        return None;
    }
    let (send, receive) = mpsc::sync_channel(1);
    if thread::Builder::new()
        .name("dsg-version-bound".into())
        .spawn(move || {
            let mut id = 0u64;
            if unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut id) } == 0 {
                let _ = send.send(id);
            }
        })
        .is_err()
    {
        return None;
    }
    let id = receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()?;
    let bound = id.checked_mul(4)?.checked_add(closing_generation)?;
    (id > 100 && bound < (1u64 << 32)).then_some(bound)
}

enum ScanFailure {
    Retry(TreeExitFailure),
    Final(TreeExitFailure),
}

fn verify_scan(
    deadline: Instant,
    mut scan: impl FnMut() -> Result<(), ScanFailure>,
) -> Result<(), TreeExitFailure> {
    let mut attempts = SCAN_ATTEMPTS;
    loop {
        attempts -= 1;
        let reason = match scan() {
            Ok(()) => return Ok(()),
            Err(ScanFailure::Final(reason)) => return Err(reason),
            Err(ScanFailure::Retry(reason)) => reason,
        };
        if attempts == 0 || deadline.saturating_duration_since(Instant::now()) <= SCAN_RETRY_DELAY {
            return Err(reason);
        }
        // Give unrelated short-lived processes a chance to finish before taking
        // a fresh snapshot. Every successful scan still needs adjacent markers.
        thread::sleep(SCAN_RETRY_DELAY);
    }
}

impl MacExitProof {
    /// The caller owns the unreaped managed child, including when it has
    /// already exited. Its PID cannot be assigned to another process yet.
    pub(crate) fn capture(pid: u32) -> io::Result<Self> {
        let pid = i32::try_from(pid).map_err(|_| invalid())?;
        if pid <= 1 {
            return Err(invalid());
        }
        let info = identifier(pid)?;
        if info.version == 0 {
            return Err(invalid());
        }
        Ok(Self {
            boot: boot_id()?,
            generation: info.generation as i64,
            version: info.version,
        })
    }

    fn valid(&self) -> bool {
        self.generation > 0 && self.version != 0 && uuid_valid(&self.boot)
    }

    pub(crate) fn boot_changed(&self) -> bool {
        self.valid() && boot_id().is_ok_and(|boot| !self.boot.eq_ignore_ascii_case(&boot))
    }

    pub(crate) fn check_exit(&self) -> Result<(), TreeExitFailure> {
        if !self.valid() {
            return Err(TreeExitFailure::StartProof);
        }
        let boot = boot_id().map_err(|_| TreeExitFailure::BootIdentity)?;
        if !self.boot.eq_ignore_ascii_case(&boot) {
            return Ok(());
        }
        let deadline = Instant::now() + PROOF_BUDGET;
        verify_scan(deadline, || self.scan(deadline))
    }

    fn scan(&self, deadline: Instant) -> Result<(), ScanFailure> {
        use ScanFailure::{Final, Retry};
        use TreeExitFailure::*;
        let read_marker = || {
            marker(deadline).map_err(|_| {
                Retry(if Instant::now() >= deadline {
                    Deadline
                } else {
                    Marker
                })
            })
        };
        let first = read_marker()?;
        let root = self.generation as u64;
        if first <= root {
            return Err(Retry(Generation));
        }
        let mut older_sessions = HashSet::new();
        let mut newer_sessions = HashSet::new();
        let mut outside_versions = HashSet::new();
        let mut newer = Vec::new();
        for pid in live_pids().map_err(|_| Retry(ProcessList))? {
            if Instant::now() >= deadline {
                return Err(Retry(Deadline));
            }
            let info = identifier(pid).map_err(|_| Retry(Identity))?;
            let gen = info.generation;
            if gen == root {
                return Err(Final(LeaderPresent));
            }
            if gen >= first {
                return Err(Retry(Generation));
            }
            let sid = unsafe { libc::getsid(pid) };
            if gen < root {
                if info.version != 0 {
                    outside_versions.insert(info.version);
                }
                if sid >= 0 {
                    older_sessions.insert(sid);
                }
            } else if sid >= 0 {
                newer_sessions.insert(sid);
                newer.push((gen, sid, info.version, info.original_parent_version));
            } else {
                return Err(Retry(Session));
            }
        }
        // Fork generations advance once per new PID and survive exec. With
        // adjacent markers, neither a PID nor a session ID could be recycled
        // during the scan; no descendant could relay work to a later fork.
        let closing = read_marker()?;
        if closing != first + 1 {
            return Err(Retry(Generation));
        }
        // A descendant inherits our dedicated session or creates a fresh one.
        // It can never join a session containing a pre-existing process.
        if newer_sessions.is_subset(&older_sessions) {
            return Ok(());
        }
        let Some(bound) = version_upper_bound(closing, deadline) else {
            return Err(Final(VersionBound));
        };
        if outside_versions
            .iter()
            .any(|&version| u64::from(version) > bound)
        {
            return Err(Final(Version));
        }
        let mut outside_sessions = older_sessions;
        newer.sort_unstable();
        for (_, sid, version, parent_version) in newer {
            if Instant::now() >= deadline {
                return Err(Retry(Deadline));
            }
            if version == 0 || u64::from(version) > bound || parent_version >= version {
                return Err(Final(Version));
            }
            // Fork versions start at zero and increase at least once per
            // generation. The managed root could already have exec'd again
            // when captured, so its sampled version is not a lower bound.
            let outside_parent = parent_version != 0
                && (u64::from(parent_version) < root - 1
                    || outside_versions.contains(&parent_version));
            if outside_parent || outside_sessions.contains(&sid) {
                outside_sessions.insert(sid);
                outside_versions.insert(version);
            }
        }
        if newer_sessions.is_subset(&outside_sessions) {
            Ok(())
        } else {
            Err(Final(SessionOrigin))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_start_identity_returns_only_the_fixed_reason() {
        let proof = MacExitProof {
            boot: String::new(),
            generation: 0,
            version: 0,
        };
        let reason = proof.check_exit().unwrap_err();
        assert_eq!(reason, TreeExitFailure::StartProof);
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            "TREE_START_PROOF_UNAVAILABLE"
        );
    }

    #[test]
    fn transient_generation_changes_get_another_bounded_scan() {
        let mut calls = 0;
        let result = verify_scan(Instant::now() + Duration::from_secs(1), || {
            calls += 1;
            if calls <= 3 {
                Err(ScanFailure::Retry(TreeExitFailure::Generation))
            } else {
                Ok(())
            }
        });
        assert_eq!((result, calls), (Ok(()), 4));
    }

    #[test]
    fn an_unverified_session_origin_is_not_retried() {
        let mut calls = 0;
        let result = verify_scan(Instant::now() + Duration::from_secs(1), || {
            calls += 1;
            Err(ScanFailure::Final(TreeExitFailure::SessionOrigin))
        });
        assert_eq!((result, calls), (Err(TreeExitFailure::SessionOrigin), 1));
    }

    #[test]
    fn exhausted_time_preserves_the_latest_failure() {
        let mut calls = 0;
        let result = verify_scan(Instant::now(), || {
            calls += 1;
            Err(ScanFailure::Retry(TreeExitFailure::Generation))
        });
        assert_eq!((result, calls), (Err(TreeExitFailure::Generation), 1));
    }
}
