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
