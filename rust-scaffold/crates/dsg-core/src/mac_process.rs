use std::collections::BTreeSet;
use std::io;
use std::mem::{size_of, size_of_val, MaybeUninit};

pub(crate) const MAX_IDENTITIES: usize = 256;
const MAX_LIVE_PROCESSES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) started_seconds: i64,
    pub(crate) started_microseconds: u32,
}

struct Process {
    identity: ProcessIdentity,
    parent: u32,
}

fn unknown() -> io::Error {
    io::Error::other("macOS process observation is incomplete")
}

fn process(pid: u32) -> io::Result<Option<Process>> {
    let raw_pid = i32::try_from(pid).map_err(|_| unknown())?;
    if raw_pid <= 1 {
        return Err(unknown());
    }
    let mut data = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    unsafe { *libc::__error() = 0 };
    // A nonzero argument includes the unreaped group leader after a fast exit.
    let count = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDTBSDINFO,
            1,
            data.as_mut_ptr().cast(),
            size,
        )
    };
    if count == 0
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        && unsafe { libc::kill(raw_pid, 0) == -1 }
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Ok(None);
    }
    if count != size {
        return Err(unknown());
    }
    let data = unsafe { data.assume_init() };
    if data.pbi_pid != pid
        || data.pbi_ruid != unsafe { libc::getuid() }
        || data.pbi_start_tvsec == 0
        || data.pbi_start_tvusec >= 1_000_000
    {
        return Err(unknown());
    }
    let started_seconds = i64::try_from(data.pbi_start_tvsec).map_err(|_| unknown())?;
    Ok(Some(Process {
        identity: ProcessIdentity {
            pid,
            started_seconds,
            started_microseconds: data.pbi_start_tvusec as u32,
        },
        parent: data.pbi_ppid,
    }))
}

pub(crate) fn current_process() -> io::Result<ProcessIdentity> {
    process(std::process::id())?
        .map(|entry| entry.identity)
        .ok_or_else(unknown)
}

pub(crate) fn gone(identity: ProcessIdentity) -> bool {
    process(identity.pid).is_ok_and(|entry| entry.is_none_or(|entry| entry.identity != identity))
}

fn children(parent: u32, buffer: &mut [libc::pid_t]) -> io::Result<&[libc::pid_t]> {
    let bytes = size_of_val(buffer);
    // libproc reports both an empty list and a syscall error as zero entries.
    unsafe { *libc::__error() = 0 };
    let count = unsafe {
        libc::proc_listchildpids(
            parent as i32,
            buffer.as_mut_ptr().cast(),
            bytes as libc::c_int,
        )
    };
    let error = io::Error::last_os_error();
    let count = usize::try_from(count).map_err(|_| unknown())?;
    if (count == 0 && error.raw_os_error() != Some(0)) || count >= buffer.len() {
        return Err(unknown());
    }
    Ok(&buffer[..count])
}

#[derive(Default)]
pub(crate) struct Descendants {
    live: BTreeSet<ProcessIdentity>,
    observed: usize,
    pub(crate) complete: bool,
}

impl Descendants {
    pub(crate) fn observe(&mut self, leader: u32) -> io::Result<Vec<ProcessIdentity>> {
        let mut found = Vec::new();
        if self.observed == 0 {
            let entry = process(leader)?
                .filter(|entry| entry.parent == std::process::id())
                .ok_or_else(unknown)?;
            found.push(entry.identity);
            self.live.insert(entry.identity);
        }
        let mut pending: Vec<_> = self.live.iter().copied().collect();
        let mut seen = BTreeSet::new();
        let mut buffer = [0; MAX_LIVE_PROCESSES];
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent) {
                continue;
            }
            if seen.len() > MAX_LIVE_PROCESSES {
                return Err(unknown());
            }
            if process(parent.pid)?.is_none_or(|entry| entry.identity != parent) {
                self.live.remove(&parent);
                continue;
            }
            let child_pids = children(parent.pid, &mut buffer)?;
            let mut direct = Vec::new();
            for &pid in child_pids {
                if let Some(entry) = process(u32::try_from(pid).map_err(|_| unknown())?)? {
                    if entry.parent == parent.pid {
                        direct.push(entry.identity);
                    } else if !self.live.contains(&entry.identity) {
                        return Err(unknown());
                    }
                }
            }
            if !direct.is_empty()
                && process(parent.pid)?.is_none_or(|entry| entry.identity != parent)
            {
                return Err(unknown());
            }
            for child in direct {
                if self.live.insert(child) {
                    if self.observed + found.len() == MAX_IDENTITIES
                        || self.live.len() > MAX_LIVE_PROCESSES
                    {
                        return Err(unknown());
                    }
                    found.push(child);
                    pending.push(child);
                }
            }
        }
        self.observed += found.len();
        Ok(found)
    }

    pub(crate) fn gone(&self) -> bool {
        self.complete && self.live.iter().copied().all(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_a_current_pid_from_its_earlier_generation() {
        let current = current_process().unwrap();
        assert!(!gone(current));
        assert!(gone(ProcessIdentity {
            started_seconds: current.started_seconds - 1,
            ..current
        }));
    }
}
