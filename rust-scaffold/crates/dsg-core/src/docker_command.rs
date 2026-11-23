use crate::docker::{DockerOperation, DockerRead, QueryFailure};
use std::io;
#[cfg(any(unix, windows))]
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_STDOUT_BYTES: usize = 64 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

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

/// A bounded set of read-only queries against one explicitly named Docker context.
/// Create a new instance per inventory so its deadline covers the entire scan.
pub struct DockerCli {
    context: String,
    deadline: Instant,
}

impl DockerCli {
    pub fn new(context: &str) -> Result<Self, QueryFailure> {
        if context.len() > 128
            || !context
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !context
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(Self {
            context: context.to_owned(),
            deadline: Instant::now() + INVENTORY_TIMEOUT,
        })
    }

    fn command(&self, operation: DockerOperation<'_>) -> Result<Command, QueryFailure> {
        let mut command = Command::new("docker");
        command
            .arg("--context")
            .arg(&self.context)
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_API_VERSION");

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
            DockerOperation::Volumes => command.args(["volume", "ls", "--format", "{{.Name}}"]),
        };
        Ok(command)
    }
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

fn is_hex_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn capture_output(mut command: Command, deadline: Instant) -> Result<String, QueryFailure> {
    if Instant::now() >= deadline {
        return Err(QueryFailure::Timeout);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| QueryFailure::Unavailable)?;
    let result = collect_output(&mut child, deadline);
    if result.is_err() {
        // Only kill and reap the child spawned by this call.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn collect_output(child: &mut Child, deadline: Instant) -> Result<String, QueryFailure> {
    let mut stdout = child.stdout.take().ok_or(QueryFailure::Unavailable)?;
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
            if let Some(status) = child.try_wait().map_err(|_| QueryFailure::Unavailable)? {
                if !status.success() {
                    return Err(QueryFailure::Unavailable);
                }
                exited = true;
            }
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
                DockerOperation::Volumes,
                vec!["volume", "ls", "--format", "{{.Name}}"],
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
        for template in [CONTAINER_FORMAT, IMAGE_FORMAT] {
            for forbidden in [
                ".Env",
                "json .Config",
                "json .Labels",
                ".Source",
                "json .RepoDigests",
            ] {
                assert!(!template.contains(forbidden));
            }
        }
        assert_eq!(CONTAINER_FORMAT.matches("index .Labels").count(), 5);
    }

    #[test]
    fn exhausted_inventory_budget_prevents_child_spawn() {
        let docker = DockerCli {
            context: "default".to_owned(),
            deadline: Instant::now(),
        };
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
