use super::Cli;
use dsg_core::registry::StateObservation;
use std::path::Path;
use std::process::ExitCode;

pub(super) struct Reader {
    #[cfg(target_os = "macos")]
    state_dir: std::path::PathBuf,
    #[cfg(target_os = "macos")]
    global_policy: Option<std::path::PathBuf>,
    #[cfg(target_os = "macos")]
    session_policy: Option<std::path::PathBuf>,
}

impl Reader {
    pub(super) fn new(cli: &Cli) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Some(Self {
                state_dir: std::path::absolute(super::local_state_dir(cli).ok()?).ok()?,
                global_policy: cli
                    .global_policy
                    .as_ref()
                    .map(std::path::absolute)
                    .transpose()
                    .ok()?,
                session_policy: cli
                    .session_policy
                    .as_ref()
                    .map(std::path::absolute)
                    .transpose()
                    .ok()?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cli;
            None
        }
    }

    pub(super) fn observe(&self, root: &Path, plan_id: Option<i64>) -> Option<StateObservation> {
        #[cfg(target_os = "macos")]
        {
            macos::observe(self, root, plan_id)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (root, plan_id);
            None
        }
    }
}

pub(super) fn run(cli: &Cli, plan_id: Option<i64>) -> ExitCode {
    #[cfg(target_os = "macos")]
    {
        macos::run(cli, plan_id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (cli, plan_id);
        ExitCode::FAILURE
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use dsg_core::{filesystem, policy::Policy, registry};
    use std::ffi::{c_char, c_int, CString};
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const PROFILE: &str = "(version 1)(allow default)(deny file-write*)";
    const MAX_OUTPUT: usize = 1024;
    const TIMEOUT: Duration = Duration::from_secs(2);
    const SANDBOX_FILTER_PATH: c_int = 1;

    unsafe extern "C" {
        fn sandbox_check(pid: libc::pid_t, operation: *const c_char, filter: c_int, ...) -> c_int;
    }

    pub(super) fn observe(
        reader: &Reader,
        root: &Path,
        plan_id: Option<i64>,
    ) -> Option<StateObservation> {
        let executable = std::env::current_exe().ok()?;
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .env_clear()
            .args(["-p", PROFILE])
            .arg(executable)
            .arg("--root")
            .arg(root)
            .arg("--state-dir")
            .arg(&reader.state_dir);
        for (flag, path) in [
            ("--global-policy", &reader.global_policy),
            ("--session-policy", &reader.session_policy),
        ] {
            if let Some(path) = path {
                command.arg(flag).arg(path);
            }
        }
        command.arg("mcp-state-read");
        if let Some(plan_id) = plan_id {
            command.arg("--plan-id").arg(plan_id.to_string());
        }
        let deadline = Instant::now() + TIMEOUT;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let observation = bounded_output(&mut child, deadline)
            .and_then(|output| serde_json::from_slice::<StateObservation>(&output).ok());
        if observation.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        match (&observation, plan_id) {
            (Some(StateObservation::Execution { .. }), None)
            | (Some(StateObservation::Approval { .. }), Some(_)) => observation,
            _ => None,
        }
    }

    fn bounded_output(child: &mut Child, deadline: Instant) -> Option<Vec<u8>> {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // The descriptor remains open and belongs to this pipe throughout the read.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return None;
        }
        let mut output = Vec::new();
        let mut chunk = [0; MAX_OUTPUT];
        let mut eof = false;
        let mut exited = false;
        while Instant::now() < deadline {
            if !exited {
                if let Some(status) = child.try_wait().ok()? {
                    if !status.success() {
                        return None;
                    }
                    exited = true;
                }
            }
            if !eof {
                match stdout.read(&mut chunk) {
                    Ok(0) => eof = true,
                    Ok(read) if output.len() + read <= MAX_OUTPUT => {
                        output.extend_from_slice(&chunk[..read]);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    _ => return None,
                }
            }
            if exited && eof {
                return Some(output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    pub(super) fn run(cli: &Cli, plan_id: Option<i64>) -> ExitCode {
        let Some(result) = read(cli, plan_id) else {
            return ExitCode::FAILURE;
        };
        let mut stdout = io::stdout().lock();
        if serde_json::to_writer(&mut stdout, &result).is_ok()
            && stdout.write_all(b"\n").is_ok()
            && stdout.flush().is_ok()
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    fn read(cli: &Cli, plan_id: Option<i64>) -> Option<StateObservation> {
        if plan_id.is_some_and(|id| id < 1) {
            return None;
        }
        let state_dir = cli.state_dir.as_ref()?;
        let canonical_state = state_dir.canonicalize().ok()?;
        if !writes_denied(&canonical_state) {
            return None;
        }
        let root = filesystem::canonical_root(&cli.root).ok()?;
        let policy = Policy::load(
            &root,
            cli.global_policy.as_deref(),
            cli.session_policy.as_deref(),
        )
        .ok()?;
        let digest = crate::policy_digest(&policy).ok()?;
        registry::observe_state(
            state_dir,
            &root,
            &policy.project_id,
            policy.version,
            &digest,
            plan_id,
        )
        .ok()
    }

    fn writes_denied(state: &Path) -> bool {
        for name in [
            "",
            "registry.sqlite3",
            "registry.sqlite3-journal",
            "registry.sqlite3-wal",
            "registry.sqlite3-shm",
        ] {
            let Ok(path) = CString::new(state.join(name).as_os_str().as_bytes()) else {
                return false;
            };
            for operation in [
                c"file-write-data",
                c"file-write-create",
                c"file-write-unlink",
            ] {
                // A positive result is an explicit denial. Errors and an unsandboxed
                // direct invocation of the hidden subcommand both fail closed.
                if unsafe {
                    sandbox_check(
                        libc::getpid(),
                        operation.as_ptr(),
                        SANDBOX_FILTER_PATH,
                        path.as_ptr(),
                    )
                } != 1
                {
                    return false;
                }
            }
        }
        true
    }
}
