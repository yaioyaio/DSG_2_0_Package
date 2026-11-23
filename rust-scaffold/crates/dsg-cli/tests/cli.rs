use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn invoke(root: &Path, state: &Path, command: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsg"))
        .arg("--root")
        .arg(root)
        .arg("--state-dir")
        .arg(state)
        .arg("--json")
        .args(command)
        .output()
        .unwrap()
}

fn body(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn initialization_is_explicit_and_keeps_an_existing_config() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state = root.join("state");
    let doctor = invoke(root, &state, &["doctor"]);
    assert!(doctor.status.success());
    assert_eq!(body(&doctor)["result"]["mode"], "observe");
    assert!(!state.exists());

    let first = invoke(root, &state, &["init", "--project-id", "fixture"]);
    assert!(first.status.success(), "{:?}", first.status.code());
    assert_eq!(body(&first)["result"]["project"]["label"], "fixture");
    let config = root.join(".dsg.json");
    let before = fs::read(&config).unwrap();
    let second = invoke(root, &state, &["init", "--project-id", "another"]);
    assert_eq!(second.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&second.stderr).unwrap();
    assert_eq!(error["error"]["code"], "CONFIG_EXISTS");
    assert_eq!(fs::read(config).unwrap(), before);
}

#[test]
fn check_reports_policy_reasons_without_echoing_command_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let result = invoke(
        dir.path(),
        &state,
        &["check", "--", "docker", "volume", "rm", "synthetic-arg"],
    );
    assert_eq!(result.status.code(), Some(3));
    let verdict = body(&result);
    assert_eq!(verdict["schema_version"], 1);
    assert_eq!(verdict["result"]["decision"], "deny");
    assert_eq!(verdict["result"]["reason_code"], "PROTECTED_VOLUME");
    assert!(!verdict.to_string().contains("synthetic-arg"));
    let result = invoke(
        dir.path(),
        &state,
        &["check", "--", "docker", "system", "prune"],
    );
    assert_eq!(result.status.code(), Some(4));
    assert_eq!(body(&result)["result"]["decision"], "approval_required");
}

#[test]
#[cfg(unix)]
fn docker_scan_queries_the_requested_context_and_keeps_empty_lists_known() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = tempfile::tempdir().unwrap();
    let client = fixture.path().join("docker");
    fs::write(
        &client,
        concat!(
            "#!/bin/sh\n",
            "[ \"$1:$2\" = '--context:fixture' ] || exit 10\n",
            "[ -z \"${DOCKER_HOST+x}${DOCKER_CONTEXT+x}${DOCKER_API_VERSION+x}\" ] || exit 11\n",
            "shift 2\n",
            "case \"$1:$2\" in\n",
            "  info:--format) printf '%s\\n' 'fixture-engine';;\n",
            "  container:ls|image:ls|volume:ls) :;;\n",
            "  *) exit 12;;\n",
            "esac\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&client, fs::Permissions::from_mode(0o700)).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_dsg"))
        .arg("--root")
        .arg(fixture.path())
        .args(["--json", "docker-scan", "--context", "fixture"])
        .env("PATH", fixture.path())
        .env("DOCKER_HOST", "synthetic-invalid")
        .env("DOCKER_CONTEXT", "synthetic-invalid")
        .env("DOCKER_API_VERSION", "synthetic-invalid")
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result.status.code());
    let output = body(&result);
    assert_eq!(output["result"]["engine_id"], "fixture-engine");
    assert_eq!(output["result"]["complete"], true);
    assert_eq!(output["result"]["containers"], serde_json::json!([]));
}
