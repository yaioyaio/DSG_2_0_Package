use crate::docker::{
    context_inventory, valid_context_name, ContextInventory, DockerOperation, DockerRead,
    QueryFailure,
};
use crate::docker_cache::BuildxRead;
use crate::docker_image_gc::{valid_full_image_id, valid_owner_fingerprint};
use std::io;
#[cfg(any(unix, windows))]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_STDOUT_BYTES: usize = 64 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
const BUILDX_QUERY_TIMEOUT: Duration = Duration::from_secs(6);
#[cfg(unix)]
const BUILDX_CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const BUILD_CACHE_FORMAT: &str = r#"{{if eq .Type "Build Cache"}}{"total":{{json .TotalCount}},"active":{{json .Active}}}{{end}}"#;
#[cfg(unix)]
const BUILDX_USAGE_FORMAT: &str = "{{.Reclaimable}}|{{.Shared}}|{{.Mutable}}";
// Docker may return a stub for an unreadable or missing context on success.
const CONTEXT_NAME_FORMAT: &str = "{{if .Error}}null{{else}}{{json .Name}}{{end}}";

// Docker's Go types spell the ID field ID and expose it in API JSON as Id.
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"image_id":{{json .Image}},"state":{{with .State}}{{json .Status}}{{else}}null{{end}},"#,
    r#""health":{{with .State}}{{with .Health}}{{json .Status}}{{else}}null{{end}}{{else}}null{{end}},"#,
    r#""compose":{{with .Config}}{"project":{{json (index .Labels "com.docker.compose.project")}},"#,
    r#""service":{{json (index .Labels "com.docker.compose.service")}},"#,
    r#""number":{{json (index .Labels "com.docker.compose.container-number")}},"#,
    r#""config_hash":{{json (index .Labels "com.docker.compose.config-hash")}},"#,
    r#""oneoff":{{json (index .Labels "com.docker.compose.oneoff")}}}{{else}}null{{end}},"#,
    r#""mounts":[{{range $index, $mount := .Mounts}}{{if $index}},{{end}}{"kind":{{json $mount.Type}},"#,
    r#""volume":{{if and (eq $mount.Type "volume") $mount.Name}}{{json $mount.Name}}{{else}}null{{end}},"#,
    r#""destination":{{json $mount.Destination}},"read_only":{{json (not $mount.RW)}}}{{end}}]}"#,
);

const IMAGE_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"digests":[{{$comma := false}}{{range .RepoDigests}}"#,
    r#"{{$parts := split . "@"}}{{if eq (len $parts) 2}}{{$digest := index $parts 1}}"#,
    r#"{{if and (eq (len $digest) 71) (eq (slice $digest 0 7) "sha256:")}}"#,
    r#"{{$valid := true}}{{range split (slice $digest 7) ""}}"#,
    r#"{{if not (or (and (ge . "0") (le . "9")) (and (ge . "a") (le . "f")))}}{{$valid = false}}{{end}}{{end}}"#,
    r#"{{if $valid}}{{if $comma}},{{end}}{{json $digest}}{{$comma = true}}{{end}}{{end}}{{end}}{{end}}],"#,
    r#""os":{{json .Os}},"architecture":{{json .Architecture}}}"#,
);

const IMAGE_GC_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"owner_matches":{{with .Config}}"#,
    r#"{{eq (index .Labels "io.dsg.gc.owner") "OWNER_FINGERPRINT"}}{{else}}null{{end}},"#,
    r#""tag_count":{{len .RepoTags}},"digest_count":{{len .RepoDigests}}}"#,
);

/// A bounded set of read-only queries against one explicitly named Docker context.
/// Create a new instance per inventory so its deadline covers the entire scan.
pub struct DockerCli {
    context: String,
    deadline: Instant,
    #[cfg(unix)]
    local: Option<LocalClient>,
}

#[cfg(unix)]
struct LocalClient {
    executable: PathBuf,
    host: String,
    config: PathBuf,
}

impl DockerCli {
    pub fn new(context: &str) -> Result<Self, QueryFailure> {
        if !valid_context_name(context) {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(Self {
            context: context.to_owned(),
            deadline: Instant::now() + INVENTORY_TIMEOUT,
            #[cfg(unix)]
            local: None,
        })
    }

    #[cfg(unix)]
    pub fn for_local_socket(
        executable: &Path,
        socket: &Path,
        config: &Path,
    ) -> Result<Self, QueryFailure> {
        let path = socket
            .to_str()
            .filter(|path| !path.chars().any(char::is_control) && path.trim_end() == *path);
        if !executable.is_absolute()
            || !socket.is_absolute()
            || !config.is_absolute()
            || path.is_none()
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let mut client = Self::new("dsg-local")?;
        client.local = Some(LocalClient {
            executable: executable.to_owned(),
            host: format!("unix://{}", path.ok_or(QueryFailure::InvalidSelector)?),
            config: config.to_owned(),
        });
        Ok(client)
    }

    fn base_command(&self) -> Command {
        #[cfg(unix)]
        if let Some(local) = &self.local {
            let mut command = Command::new(&local.executable);
            command
                .env_clear()
                .env("HOME", &local.config)
                .env("PATH", "/usr/bin:/bin")
                .arg("--config")
                .arg(&local.config)
                .arg("--host")
                .arg(&local.host);
            return command;
        }
        let mut command = docker_command();
        command.arg("--context").arg(&self.context);
        command
    }

    fn command(&self, operation: DockerOperation<'_>) -> Result<Command, QueryFailure> {
        let mut command = self.base_command();
        match operation {
            DockerOperation::Engine => command.args(["info", "--format", "{{.ID}}"]),
            DockerOperation::Containers => command.args([
                "container",
                "ls",
                "--all",
                "--no-trunc",
                "--format",
                "{{.ID}}",
            ]),
            DockerOperation::Container(id) => {
                if !is_hex_id(id) {
                    return Err(QueryFailure::InvalidSelector);
                }
                command.args([
                    "container",
                    "inspect",
                    "--format",
                    CONTAINER_FORMAT,
                    "--",
                    id,
                ])
            }
            DockerOperation::Images => {
                command.args(["image", "ls", "--all", "--no-trunc", "--format", "{{.ID}}"])
            }
            DockerOperation::Image(id) => {
                if !is_hex_id(id.strip_prefix("sha256:").unwrap_or(id)) {
                    return Err(QueryFailure::InvalidSelector);
                }
                command.args(["image", "inspect", "--format", IMAGE_FORMAT, "--", id])
            }
            DockerOperation::ImageGcEvidence {
                id,
                owner_fingerprint,
            } => {
                if !valid_full_image_id(id) || !valid_owner_fingerprint(owner_fingerprint) {
                    return Err(QueryFailure::InvalidSelector);
                }
                let format = IMAGE_GC_FORMAT.replace("OWNER_FINGERPRINT", owner_fingerprint);
                command.args(["image", "inspect", "--format", &format, "--", id])
            }
            DockerOperation::Volumes => command.args(["volume", "ls", "--format", "{{.Name}}"]),
            DockerOperation::BuildCache => {
                command.args(["system", "df", "--format", BUILD_CACHE_FORMAT])
            }
        };
        Ok(command)
    }

    #[cfg(unix)]
    fn buildx_usage_command(&self, config: &Path) -> Result<Command, QueryFailure> {
        if self.local.is_some() {
            return Err(QueryFailure::Unavailable);
        }
        let mut command = self.base_command();
        // An empty store prevents existing builders from shadowing the context's builder.
        command
            .env("BUILDX_CONFIG", config)
            .env_remove("BUILDX_BUILDER")
            .env_remove("BUILDKIT_HOST")
            .process_group(0)
            .args([
                "buildx",
                "du",
                "--builder",
                &self.context,
                "--timeout",
                "5s",
                "--format",
                BUILDX_USAGE_FORMAT,
            ]);
        Ok(command)
    }
}

/// List locally configured names without selecting a context or querying an engine.
pub fn discover_contexts(max_contexts: u16) -> ContextInventory {
    let mut command = docker_command();
    command
        .env_remove("DOCKER_TLS")
        .env_remove("DOCKER_TLS_VERIFY")
        .env_remove("DOCKER_CERT_PATH")
        .args(["context", "ls", "--format", CONTEXT_NAME_FORMAT]);
    context_inventory(
        capture_output(command, Instant::now() + QUERY_TIMEOUT),
        max_contexts,
    )
}

fn docker_command() -> Command {
    let mut command = Command::new("docker");
    command
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_API_VERSION");
    command
}

impl DockerRead for DockerCli {
    fn context(&self) -> &str {
        &self.context
    }

    fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
        let command = self.command(operation)?;
        let deadline = self.deadline.min(Instant::now() + QUERY_TIMEOUT);
        capture_output(command, deadline)
    }
}

impl BuildxRead for DockerCli {
    #[cfg(unix)]
    fn query_buildx_usage(&self) -> Result<String, QueryFailure> {
        let deadline = self.deadline.min(Instant::now() + BUILDX_QUERY_TIMEOUT);
        if Instant::now() >= deadline {
            return Err(QueryFailure::Timeout);
        }
        let config = tempfile::Builder::new()
            .prefix("dsg-buildx-")
            .tempdir()
            .map_err(|_| QueryFailure::Unavailable)?;
        capture_buildx_output(self.buildx_usage_command(config.path())?, deadline)
    }

    #[cfg(not(unix))]
    fn query_buildx_usage(&self) -> Result<String, QueryFailure> {
        Err(QueryFailure::Unavailable)
    }
}

fn is_hex_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn capture_output(mut command: Command, deadline: Instant) -> Result<String, QueryFailure> {
    let mut child = spawn_query(&mut command, deadline)?;
    let result = collect_output(&mut child, deadline);
    if result.is_err() {
        // Only kill and reap the child spawned by this call.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn spawn_query(command: &mut Command, deadline: Instant) -> Result<Child, QueryFailure> {
    if Instant::now() >= deadline {
        return Err(QueryFailure::Timeout);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| QueryFailure::Unavailable)
}

#[cfg(unix)]
pub(crate) fn capture_buildx_output(
    mut command: Command,
    deadline: Instant,
) -> Result<String, QueryFailure> {
    let mut child = spawn_query(&mut command, deadline)?;
    let pid = child.id() as libc::pid_t;
    let output = child
        .stdout
        .take()
        .ok_or(QueryFailure::Unavailable)
        .and_then(|stdout| {
            collect_stdout(stdout, deadline, || match buildx_child_exited(pid) {
                Ok(exited) => Ok(exited),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
                Err(_) => Err(QueryFailure::Unavailable),
            })
        });
    let cleanup = finish_buildx_group(child, pid);
    let output = output?;
    cleanup?;
    Ok(output)
}

#[cfg(unix)]
fn buildx_child_exited(pid: libc::pid_t) -> io::Result<bool> {
    // Leave the leader waitable so the numeric PGID cannot be recycled before cleanup.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } == pid)
}

#[cfg(unix)]
fn finish_buildx_group(mut child: Child, pid: libc::pid_t) -> Result<(), QueryFailure> {
    let deadline = Instant::now() + BUILDX_CLEANUP_TIMEOUT;
    loop {
        match buildx_child_exited(pid) {
            Ok(_) => {
                // Buildx is a CLI plugin. Stop any remaining member of this invocation's group.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                break;
            }
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted && Instant::now() < deadline => {}
            Err(_) => return Err(QueryFailure::Unavailable),
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted && Instant::now() < deadline => {}
            _ => {
                let _ = thread::Builder::new()
                    .name("dsg-buildx-reaper".into())
                    .spawn(move || child.wait());
                return Err(QueryFailure::Unavailable);
            }
        }
    };
    loop {
        // Signal zero observes the group after reaping the leader without affecting any PID.
        let present = unsafe { libc::kill(-pid, 0) } == 0;
        if !present && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return status
                .success()
                .then_some(())
                .ok_or(QueryFailure::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(QueryFailure::Unavailable);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn collect_output(child: &mut Child, deadline: Instant) -> Result<String, QueryFailure> {
    let stdout = child.stdout.take().ok_or(QueryFailure::Unavailable)?;
    collect_stdout(stdout, deadline, || {
        match child.try_wait().map_err(|_| QueryFailure::Unavailable)? {
            Some(status) if !status.success() => Err(QueryFailure::Unavailable),
            status => Ok(status.is_some()),
        }
    })
}

fn collect_stdout(
    mut stdout: ChildStdout,
    deadline: Instant,
    mut exited_query: impl FnMut() -> Result<bool, QueryFailure>,
) -> Result<String, QueryFailure> {
    prepare_pipe(&stdout).map_err(|_| QueryFailure::Unavailable)?;
    let mut buffer = Vec::new();
    let mut chunk = [0; 8192];
    let mut reached_eof = false;
    let mut exited = false;
    loop {
        if Instant::now() >= deadline {
            return Err(QueryFailure::Timeout);
        }
        if !exited {
            exited = exited_query()?;
        }
        if !reached_eof {
            let remaining = MAX_STDOUT_BYTES - buffer.len();
            let to_read = chunk.len().min(remaining + 1);
            match read_pipe(&mut stdout, &mut chunk[..to_read]) {
                Ok(0) => reached_eof = true,
                Ok(read) if read > remaining => return Err(QueryFailure::TooLarge),
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(_) => return Err(QueryFailure::Unavailable),
            }
        }
        if exited && reached_eof {
            return String::from_utf8(buffer).map_err(|_| QueryFailure::Malformed);
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(unix)]
fn prepare_pipe(stdout: &ChildStdout) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = stdout.as_raw_fd();
    // The pipe descriptor stays open for both calls. Preserve its existing flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn read_pipe(stdout: &mut ChildStdout, buffer: &mut [u8]) -> io::Result<usize> {
    stdout.read(buffer)
}

#[cfg(windows)]
fn prepare_pipe(_stdout: &ChildStdout) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn read_pipe(stdout: &mut ChildStdout, buffer: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut available = 0;
    // This function owns the only reader and never reads more than the available bytes.
    let result = unsafe {
        PeekNamedPipe(
            stdout.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    };
    if result == 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
            _ => Err(error),
        };
    }
    if available == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let to_read = buffer.len().min(available as usize);
    stdout.read(&mut buffer[..to_read])
}

#[cfg(not(any(unix, windows)))]
fn prepare_pipe(_stdout: &ChildStdout) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(unix, windows)))]
fn read_pipe(_stdout: &mut ChildStdout, _buffer: &mut [u8]) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confines_operations_to_explicit_context_and_fixed_arguments() {
        let docker = DockerCli::new("desktop-linux").unwrap();
        let id = "01abcdef".repeat(8);
        let image_id = format!("sha256:{id}");
        let image_gc_format = IMAGE_GC_FORMAT.replace("OWNER_FINGERPRINT", &id);
        let cases = [
            (DockerOperation::Engine, vec!["info", "--format", "{{.ID}}"]),
            (
                DockerOperation::Containers,
                vec![
                    "container",
                    "ls",
                    "--all",
                    "--no-trunc",
                    "--format",
                    "{{.ID}}",
                ],
            ),
            (
                DockerOperation::Container(&id),
                vec![
                    "container",
                    "inspect",
                    "--format",
                    CONTAINER_FORMAT,
                    "--",
                    &id,
                ],
            ),
            (
                DockerOperation::Images,
                vec!["image", "ls", "--all", "--no-trunc", "--format", "{{.ID}}"],
            ),
            (
                DockerOperation::Image(&image_id),
                vec![
                    "image",
                    "inspect",
                    "--format",
                    IMAGE_FORMAT,
                    "--",
                    &image_id,
                ],
            ),
            (
                DockerOperation::ImageGcEvidence {
                    id: &image_id,
                    owner_fingerprint: &id,
                },
                vec![
                    "image",
                    "inspect",
                    "--format",
                    &image_gc_format,
                    "--",
                    &image_id,
                ],
            ),
            (
                DockerOperation::Volumes,
                vec!["volume", "ls", "--format", "{{.Name}}"],
            ),
            (
                DockerOperation::BuildCache,
                vec!["system", "df", "--format", BUILD_CACHE_FORMAT],
            ),
        ];
        for (operation, args) in cases {
            let command = docker.command(operation).unwrap();
            assert_eq!(command.get_program(), "docker");
            let mut expected = vec!["--context", "desktop-linux"];
            expected.extend(args);
            let actual: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect();
            assert_eq!(actual, expected);
            for key in ["DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_API_VERSION"] {
                assert!(command
                    .get_envs()
                    .any(|(name, value)| name == key && value.is_none()));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn buildx_usage_is_pinned_to_the_engine_context_and_empty_store() {
        let docker = DockerCli::new("explicit-context").unwrap();
        let config = Path::new("/isolated-buildx-config");
        let command = docker.buildx_usage_command(config).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--context",
                "explicit-context",
                "buildx",
                "du",
                "--builder",
                "explicit-context",
                "--timeout",
                "5s",
                "--format",
                BUILDX_USAGE_FORMAT,
            ]
        );
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "BUILDX_CONFIG" && value == Some(config.as_os_str())));
        for key in ["BUILDX_BUILDER", "BUILDKIT_HOST"] {
            assert!(command
                .get_envs()
                .any(|(name, value)| name == key && value.is_none()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn buildx_child_group_is_bounded_when_a_plugin_keeps_stdout_open() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 15 & exit 0"]).process_group(0);
        assert_eq!(
            capture_buildx_output(command, Instant::now() + Duration::from_millis(100)),
            Err(QueryFailure::Timeout)
        );
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_selectors_before_startup() {
        for context in ["", "--host", "my context", "ssh://host", "a/b", "a\nb", "é"] {
            assert!(matches!(
                DockerCli::new(context),
                Err(QueryFailure::InvalidSelector)
            ));
        }
        assert!(matches!(
            DockerCli::new(&"a".repeat(129)),
            Err(QueryFailure::InvalidSelector)
        ));

        let docker = DockerCli::new("default").unwrap();
        for id in [
            "--help",
            "latest",
            "abcd",
            "prefix/image@sha256:abcd",
            "foo\nbar",
        ] {
            assert!(matches!(
                docker.command(DockerOperation::Container(id)),
                Err(QueryFailure::InvalidSelector)
            ));
            assert!(matches!(
                docker.command(DockerOperation::Image(id)),
                Err(QueryFailure::InvalidSelector)
            ));
        }
    }

    #[test]
    fn templates_keep_sensitive_inspect_fields_out_of_stdout() {
        for template in [CONTAINER_FORMAT, IMAGE_FORMAT, IMAGE_GC_FORMAT] {
            for forbidden in [
                ".Env",
                "json .Config",
                "json .Labels",
                ".Source",
                "json .RepoDigests",
                "json .RepoTags",
            ] {
                assert!(!template.contains(forbidden));
            }
        }
        assert_eq!(CONTAINER_FORMAT.matches("index .Labels").count(), 5);
        assert_eq!(IMAGE_GC_FORMAT.matches("index .Labels").count(), 1);
        let docker = DockerCli::new("default").unwrap();
        let id = "a".repeat(64);
        let image_id = format!("sha256:{id}");
        assert!(matches!(
            docker.command(DockerOperation::ImageGcEvidence {
                id: &image_id,
                owner_fingerprint: "\"}}\n{{json .Config}}",
            }),
            Err(QueryFailure::InvalidSelector)
        ));
    }

    #[test]
    fn exhausted_inventory_budget_prevents_child_spawn() {
        let mut docker = DockerCli::new("default").unwrap();
        docker.deadline = Instant::now();
        assert!(matches!(
            docker.query(DockerOperation::Engine),
            Err(QueryFailure::Timeout)
        ));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn reads_process_output_and_rejects_oversize_malformed_or_failed_output() {
        let deadline = || Instant::now() + Duration::from_secs(3);
        let output = capture_output(fixture_command("success"), deadline()).unwrap();
        assert!(output.ends_with("allowed-stdout\n"));
        assert!(!output.contains("discarded-stderr"));
        assert!(matches!(
            capture_output(fixture_command("large"), deadline()),
            Err(QueryFailure::TooLarge)
        ));
        assert!(matches!(
            capture_output(fixture_command("malformed"), deadline()),
            Err(QueryFailure::Malformed)
        ));
        assert!(matches!(
            capture_output(fixture_command("failure"), deadline()),
            Err(QueryFailure::Unavailable)
        ));
        assert!(matches!(
            capture_output(
                fixture_command("timeout"),
                Instant::now() + Duration::from_millis(100)
            ),
            Err(QueryFailure::Timeout)
        ));
    }

    #[cfg(any(unix, windows))]
    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "docker_command::tests::process_fixture",
                "--nocapture",
            ])
            .env("DSG_DOCKER_TEST_FIXTURE", mode);
        command
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn process_fixture() {
        use std::io::Write;

        let Ok(mode) = std::env::var("DSG_DOCKER_TEST_FIXTURE") else {
            return;
        };
        let mut output = io::stdout().lock();
        match mode.as_str() {
            "success" => {
                output.write_all(b"allowed-stdout\n").unwrap();
                writeln!(io::stderr(), "discarded-stderr").unwrap();
            }
            "large" => output.write_all(&vec![b'x'; MAX_STDOUT_BYTES + 1]).unwrap(),
            "malformed" => output.write_all(&[0xff]).unwrap(),
            "failure" => std::process::exit(1),
            "timeout" => thread::sleep(Duration::from_secs(2)),
            _ => std::process::exit(2),
        }
        output.flush().unwrap();
        drop(output);
        std::process::exit(0);
    }
}
