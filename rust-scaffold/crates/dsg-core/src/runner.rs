use serde::Serialize;
use std::{io, path::Path, sync::atomic::AtomicBool, time::Duration};

#[cfg(target_os = "macos")]
pub use crate::mac_process::ProcessIdentity;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunEvent {
    Started(u32),
    Heartbeat,
    #[cfg(target_os = "macos")]
    ObservationStarted,
    #[cfg(target_os = "macos")]
    ObservationCompleted(Vec<ProcessIdentity>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failure,
    TimedOut,
    Interrupted,
    ControlFailure,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub outcome: RunOutcome,
    pub exit_code: Option<i32>,
    /// The owned group and all observed descendants are absent.
    pub tree_confirmed_gone: bool,
}

pub fn run_command(
    root: &Path,
    argv: &[String],
    timeout: Duration,
    quiet: bool,
    cancelled: &AtomicBool,
    notify: impl FnMut(RunEvent) -> io::Result<()>,
) -> io::Result<RunResult> {
    run_with_target(root, argv, timeout, quiet, cancelled, None, notify)
}

pub fn run_with_target(
    root: &Path,
    argv: &[String],
    timeout: Duration,
    quiet: bool,
    cancelled: &AtomicBool,
    cargo_target: Option<&Path>,
    notify: impl FnMut(RunEvent) -> io::Result<()>,
) -> io::Result<RunResult> {
    run_configured(
        root,
        argv,
        timeout,
        quiet,
        cancelled,
        RunSettings {
            cargo_target,
            ..RunSettings::default()
        },
        notify,
    )
}

#[derive(Default)]
pub struct RunSettings<'a> {
    pub cargo_target: Option<&'a Path>,
    pub env: &'a [(&'a str, &'a str)],
    pub file_limit: FileDescriptorLimit,
}

#[derive(Clone, Copy, Default)]
pub enum FileDescriptorLimit {
    #[default]
    Standard,
    Gradle,
}

impl FileDescriptorLimit {
    fn maximum(self) -> u64 {
        match self {
            Self::Standard => 256,
            Self::Gradle => 1024,
        }
    }
}
pub fn run_configured(
    root: &Path,
    argv: &[String],
    timeout: Duration,
    quiet: bool,
    cancelled: &AtomicBool,
    settings: RunSettings<'_>,
    notify: impl FnMut(RunEvent) -> io::Result<()>,
) -> io::Result<RunResult> {
    if timeout.is_zero() || timeout > Duration::from_secs(120) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid timeout",
        ));
    }
    if argv.first().is_none_or(String::is_empty)
        || argv.len() > 128
        || argv
            .iter()
            .any(|arg| arg.len() > 4096 || arg.contains('\0'))
        || argv.iter().map(String::len).sum::<usize>() > 32 * 1024
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid command arguments",
        ));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        posix::run(root, argv, timeout, quiet, cancelled, settings, notify)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (root, quiet, cancelled, settings, notify);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "managed process groups unavailable",
        ))
    }
}

/// A missing group and a missing leader are required. Never signal a restored lease's PID.
pub fn process_group_gone(pid: u32) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        pid > 1 && posix::absent(pid) && posix::absent(-pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        false
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn process_group_absent(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(|pid| pid > 1 && posix::absent(-pid))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod posix {
    use super::*;
    #[cfg(target_os = "macos")]
    use crate::mac_process::Descendants;
    use std::os::unix::process::CommandExt;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Instant;

    const POLL: Duration = Duration::from_millis(20);
    const HEARTBEAT: Duration = Duration::from_secs(1);
    #[cfg(target_os = "macos")]
    const OBSERVATION_INTERVAL: Duration = Duration::from_millis(125);
    const TERM_GRACE: Duration = Duration::from_millis(100);
    const CLEANUP_WAIT: Duration = Duration::from_secs(1);

    pub(super) fn absent(pid_or_group: i32) -> bool {
        // Signal zero checks existence without changing a process.
        (unsafe { libc::kill(pid_or_group, 0) == -1 })
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    fn require_waitable_children() -> io::Result<()> {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // Automatic or competing SIGCHLD reapers would release the PID between
        // waitid and group signalling. The managed CLI owns its children's waits.
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "managed runner requires ownership of child waits",
            ));
        }
        Ok(())
    }

    fn isolate(cpu_seconds: u64, file_limit: u64) -> io::Result<()> {
        // This runs between fork and exec. Keep it limited to direct OS calls.
        if unsafe { libc::setsid() } == -1 {
            return Err(io::Error::last_os_error());
        }
        // The resource argument has different libc types on Darwin, glibc and musl.
        macro_rules! cap {
            ($resource:expr, $maximum:expr) => {{
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if unsafe { libc::getrlimit($resource, &mut limit) } == -1 {
                    return Err(io::Error::last_os_error());
                }
                limit.rlim_cur = limit.rlim_cur.min($maximum);
                limit.rlim_max = limit.rlim_cur;
                if unsafe { libc::setrlimit($resource, &limit) } == -1 {
                    return Err(io::Error::last_os_error());
                }
            }};
        }
        cap!(libc::RLIMIT_CPU, cpu_seconds as libc::rlim_t);
        cap!(libc::RLIMIT_FSIZE, 256 * 1024 * 1024);
        cap!(libc::RLIMIT_NOFILE, file_limit as libc::rlim_t);
        cap!(libc::RLIMIT_CORE, 0);
        Ok(())
    }

    /// WNOWAIT keeps our leader's PID reserved even when it has already exited.
    /// A reaped/reused numeric PID must never be used to terminate a process group.
    fn exited(pid: u32) -> io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } == pid as i32)
    }

    fn supervise(
        pid: u32,
        deadline: Instant,
        cancelled: &AtomicBool,
        #[cfg(target_os = "macos")] descendants: &mut Descendants,
        mut notify: impl FnMut(RunEvent) -> io::Result<()>,
    ) -> io::Result<Option<RunOutcome>> {
        notify(RunEvent::Started(pid))?;
        let mut next_heartbeat = Instant::now() + HEARTBEAT;
        #[cfg(target_os = "macos")]
        let mut next_observation = Instant::now();
        loop {
            let now = Instant::now();
            let finished = if cancelled.load(Ordering::Acquire) {
                Some(Some(RunOutcome::Interrupted))
            } else {
                match exited(pid) {
                    Ok(true) => Some(None),
                    Ok(false) => None,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => None,
                    Err(err) => return Err(err),
                }
                .or_else(|| (now >= deadline).then_some(Some(RunOutcome::TimedOut)))
            };
            #[cfg(target_os = "macos")]
            if finished.is_some() || now >= next_observation {
                if descendants.complete {
                    descendants.complete = false;
                    notify(RunEvent::ObservationStarted)?;
                }
                let observed = descendants.observe(pid)?;
                notify(RunEvent::ObservationCompleted(observed))?;
                descendants.complete = true;
                next_observation = Instant::now() + OBSERVATION_INTERVAL;
            }
            if let Some(outcome) = finished {
                return Ok(outcome);
            }
            if now >= next_heartbeat {
                notify(RunEvent::Heartbeat)?;
                next_heartbeat = Instant::now() + HEARTBEAT;
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn cleanup(mut child: Child, stopped: bool) -> (Option<ExitStatus>, bool) {
        let pid = child.id();
        let group = -(pid as i32);
        // waitid also verifies we still own this waitable child before signalling.
        let mut live_check = exited(pid);
        if live_check
            .as_ref()
            .is_err_and(|err| err.kind() == io::ErrorKind::Interrupted)
        {
            live_check = exited(pid);
        }
        if let Ok(finished) = live_check {
            if stopped && !finished {
                unsafe { libc::kill(group, libc::SIGTERM) };
                thread::sleep(TERM_GRACE);
            }
            // The unreaped leader pins the group ID across both signals. Also stop
            // any remaining group members after a normal leader exit.
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
        let deadline = Instant::now() + CLEANUP_WAIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                Err(err)
                    if err.kind() == io::ErrorKind::Interrupted && Instant::now() < deadline =>
                {
                    continue
                }
                _ => {
                    // A child stuck in kernel I/O can delay SIGKILL. Bound the caller's
                    // wait, reap on eventual exit and leave the lease in place.
                    let _ = thread::Builder::new()
                        .name("dsg-reaper".into())
                        .spawn(move || child.wait());
                    return (None, false);
                }
            }
        };
        loop {
            if process_group_gone(pid) {
                return (Some(status), true);
            }
            if Instant::now() >= deadline {
                return (Some(status), false);
            }
            thread::sleep(POLL);
        }
    }

    pub(super) fn run(
        root: &Path,
        argv: &[String],
        timeout: Duration,
        quiet: bool,
        cancelled: &AtomicBool,
        settings: RunSettings<'_>,
        notify: impl FnMut(RunEvent) -> io::Result<()>,
    ) -> io::Result<RunResult> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(RunResult {
                outcome: RunOutcome::Interrupted,
                exit_code: None,
                tree_confirmed_gone: true,
            });
        }
        require_waitable_children()?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(root)
            .stdin(Stdio::null());
        command.envs(settings.env.iter().copied());
        if let Some(target) = settings.cargo_target {
            command.env("CARGO_TARGET_DIR", target);
        }
        if quiet {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let cpu_seconds = timeout.as_secs() + u64::from(timeout.subsec_nanos() != 0);
        let file_limit = settings.file_limit.maximum();
        unsafe { command.pre_exec(move || isolate(cpu_seconds, file_limit)) };
        let deadline = Instant::now() + timeout;
        let child = command.spawn()?;
        #[cfg(target_os = "macos")]
        let mut descendants = Descendants::default();
        let monitored = catch_unwind(AssertUnwindSafe(|| {
            supervise(
                child.id(),
                deadline,
                cancelled,
                #[cfg(target_os = "macos")]
                &mut descendants,
                notify,
            )
        }));
        let (status, tree_confirmed_gone) = cleanup(child, !matches!(monitored, Ok(Ok(None))));
        #[cfg(target_os = "macos")]
        let tree_confirmed_gone = tree_confirmed_gone && descendants.gone();
        let outcome = match monitored {
            Ok(Ok(Some(reason))) => reason,
            Ok(Ok(None)) => match status {
                Some(status) if tree_confirmed_gone && status.success() => RunOutcome::Success,
                Some(_) if tree_confirmed_gone => RunOutcome::Failure,
                _ => RunOutcome::ControlFailure,
            },
            Ok(Err(_)) => RunOutcome::ControlFailure,
            Err(payload) => resume_unwind(payload),
        };
        Ok(RunResult {
            outcome,
            exit_code: status.and_then(|s| s.code()),
            tree_confirmed_gone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::sync::atomic::Ordering;

    #[test]
    fn rejects_invalid_bounds_before_spawn() {
        let root = Path::new(".");
        let flag = AtomicBool::new(false);
        for (args, timeout) in [
            (vec![], Duration::from_secs(1)),
            (vec!["unused".into()], Duration::ZERO),
            (vec!["unused".into()], Duration::from_secs(121)),
            (
                vec!["unused".into(), "a".repeat(4097)],
                Duration::from_secs(1),
            ),
            (vec!["a".repeat(4096); 9], Duration::from_secs(1)),
        ] {
            let err = run_command(root, &args, timeout, true, &flag, |_| panic!()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        for pid in [0, 1, u32::MAX, std::process::id()] {
            assert!(!process_group_gone(pid));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn exit_status_and_group_identity() {
        let flag = AtomicBool::new(false);
        for (program, expected, code) in [
            ("/usr/bin/true", RunOutcome::Success, 0),
            ("/usr/bin/false", RunOutcome::Failure, 1),
        ] {
            let mut leader = None;
            let result = run_command(
                Path::new("."),
                &[program.into()],
                Duration::from_secs(2),
                true,
                &flag,
                |event| {
                    if let RunEvent::Started(pid) = event {
                        leader = Some(pid);
                        assert!(!process_group_gone(pid));
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(result.outcome, expected);
            assert_eq!(result.exit_code, Some(code));
            assert!(result.tree_confirmed_gone && process_group_gone(leader.unwrap()));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn verify_limits() {
        macro_rules! check {
            ($resource:expr, $cap:expr) => {{
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                assert_eq!(unsafe { libc::getrlimit($resource, &mut limit) }, 0);
                assert!(limit.rlim_cur <= $cap && limit.rlim_max <= $cap);
            }};
        }
        check!(libc::RLIMIT_CPU, 4);
        check!(libc::RLIMIT_NOFILE, 256);
        check!(libc::RLIMIT_FSIZE, 256 * 1024 * 1024);
        check!(libc::RLIMIT_CORE, 0);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn enforces_child_resource_limits() {
        // The re-executed test starts in our fresh fixture directory. Its receipt
        // also verifies that the test harness matched the requested test name.
        let marker = ".dsg-test-child-limits";
        let receipt = ".dsg-test-child-verified";
        if Path::new(marker).is_file() {
            verify_limits();
            std::fs::write(receipt, b"ok").unwrap();
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join(marker), b"probe").unwrap();
        let result = run_command(
            fixture.path(),
            &[
                std::env::current_exe().unwrap().to_str().unwrap().into(),
                "--exact".into(),
                "runner::tests::enforces_child_resource_limits".into(),
            ],
            Duration::from_secs(4),
            true,
            &AtomicBool::new(false),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(result.outcome, RunOutcome::Success);
        assert_eq!(std::fs::read(fixture.path().join(receipt)).unwrap(), b"ok");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn timeout_interrupt_and_lost_heartbeat_reap_the_group() {
        for expected in [
            RunOutcome::TimedOut,
            RunOutcome::Interrupted,
            RunOutcome::ControlFailure,
        ] {
            let flag = AtomicBool::new(false);
            let timeout = if expected == RunOutcome::TimedOut {
                Duration::from_millis(80)
            } else {
                Duration::from_secs(4)
            };
            let mut leader = None;
            let result = run_command(
                Path::new("."),
                &["/bin/sleep".into(), "6".into()],
                timeout,
                true,
                &flag,
                |event| {
                    match event {
                        RunEvent::Started(pid) => {
                            leader = Some(pid);
                            if expected == RunOutcome::Interrupted {
                                flag.store(true, Ordering::Release);
                            }
                        }
                        RunEvent::Heartbeat if expected == RunOutcome::ControlFailure => {
                            return Err(io::Error::other("test heartbeat"))
                        }
                        _ => {}
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(result.outcome, expected);
            assert!(result.tree_confirmed_gone && process_group_gone(leader.unwrap()));
        }
    }
}
