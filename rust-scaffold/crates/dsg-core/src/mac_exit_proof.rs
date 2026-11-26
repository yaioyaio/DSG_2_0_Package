//! Exit evidence for a managed child which called `setsid` before running user code.
use std::collections::HashSet;
use std::io;
use std::mem::size_of;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

const PROOF_BUDGET: Duration = Duration::from_millis(150);

#[derive(Clone, Debug)]
pub(crate) struct MacExitProof {
    pub(crate) boot: String,
    pub(crate) generation: i64,
    pub(crate) version: u32,
}

// XNU sys/proc_info_private.h: PROC_PIDUNIQIDENTIFIERINFO, with arg=1 to
// include zombies. Only the fork generation and identity version are consumed.
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

    pub(crate) fn proves_gone(&self) -> bool {
        if !self.valid() {
            return false;
        }
        let Ok(boot) = boot_id() else {
            return false;
        };
        if !self.boot.eq_ignore_ascii_case(&boot) {
            return true;
        }
        let deadline = Instant::now() + PROOF_BUDGET;
        for _ in 0..3 {
            match self.scan(deadline) {
                Ok(answer) => return answer,
                Err(_) => continue,
            }
        }
        false
    }

    fn scan(&self, deadline: Instant) -> io::Result<bool> {
        let first = marker(deadline)?;
        let root = self.generation as u64;
        if first <= root {
            return Err(invalid());
        }
        let mut older_sessions = HashSet::new();
        let mut newer_sessions = HashSet::new();
        for pid in live_pids()? {
            if Instant::now() >= deadline {
                return Err(invalid());
            }
            let gen = identifier(pid)?.generation;
            if gen == root {
                return Ok(false);
            }
            if gen >= first {
                return Err(invalid());
            }
            let sid = unsafe { libc::getsid(pid) };
            if gen < root {
                if sid >= 0 {
                    older_sessions.insert(sid);
                }
            } else if sid >= 0 {
                newer_sessions.insert(sid);
            } else {
                return Err(invalid());
            }
        }
        // Fork generations advance once per new PID and survive exec. With
        // adjacent markers, neither a PID nor a session ID could be recycled
        // during the scan; no descendant could relay work to a later fork.
        if marker(deadline)? != first + 1 {
            return Err(invalid());
        }
        // A descendant inherits our dedicated session or creates a fresh one.
        // It can never join a session containing a pre-existing process.
        Ok(newer_sessions.is_subset(&older_sessions))
    }
}
