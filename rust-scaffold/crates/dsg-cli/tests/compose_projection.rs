#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const ENGINE: &str = "fixture-engine";
const CONTAINER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REPLACEMENT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const IMAGE: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const DIGEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const HASH: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn install_fixture(root: &Path) {
    let client = root.join("docker");
    fs::write(
        &client,
        r#"#!/bin/sh
[ "$1:$2" = '--context:fixture' ] || exit 10
[ -z "${DOCKER_HOST+x}${DOCKER_CONTEXT+x}${DOCKER_API_VERSION+x}" ] || exit 11
shift 2
case "$1:$2" in
  info:--format) [ "$#:$3" = '3:{{.ID}}' ] || exit 12; printf '%s\n' "$DSG_FIXTURE_ENGINE";;
  container:ls)
    IFS= read -r scans < "$DSG_FIXTURE_SCANS" || exit 13
    scans=$((scans + 1))
    printf '%s\n' "$scans" > "$DSG_FIXTURE_SCANS"
    id=$DSG_FIXTURE_CONTAINER
    if [ "$DSG_FIXTURE_MODE" = replaced ] && [ "$scans" -eq 2 ]; then
      id=$DSG_FIXTURE_REPLACEMENT
    fi
    printf '%s\n' "$id";;
  container:inspect)
    [ "$#:$3:$5" = '6:--format:--' ] || exit 14
    case "$4" in *'.Env'*|*'.HostConfig'*) exit 15;; esac
    health=healthy
    if [ "$DSG_FIXTURE_MODE" = unhealthy ]; then health=unhealthy; fi
    printf '{"id":"%s","image_id":"%s","state":"running","health":"%s","compose":{"project":"shop","service":"app","number":1,"config_hash":"%s","oneoff":"False"},"mounts":[]}\n' "$6" "$DSG_FIXTURE_IMAGE" "$health" "$DSG_FIXTURE_HASH";;
  image:ls) printf '%s\n' "$DSG_FIXTURE_IMAGE";;
  image:inspect)
    [ "$#:$3:$5:$6" = "6:--format:--:$DSG_FIXTURE_IMAGE" ] || exit 16
    arch=arm64
    if [ "$DSG_FIXTURE_MODE" = wrong-platform ]; then arch=amd64; fi
    printf '{"id":"%s","digests":["%s"],"os":"linux","architecture":"%s"}\n' "$6" "$DSG_FIXTURE_DIGEST" "$arch";;
  volume:ls) :;;
  *) exit 17;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&client, fs::Permissions::from_mode(0o700)).unwrap();
}

fn dsg(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dsg"));
    command
        .arg("--root")
        .arg(root)
        .arg("--state-dir")
        .arg(root.join("state"))
        .arg("--json");
    command
}

fn reuse(root: &Path, mode: &str, projection: Option<Value>) -> (Option<i32>, Value, String) {
    let scan_count = root.join("scan-count");
    fs::write(&scan_count, "0\n").unwrap();
    let expected = json!({
        "engine_id": ENGINE, "project": "shop", "service": "app", "number": 1,
        "config_hash": HASH, "image_id": IMAGE, "image_digest": DIGEST, "mounts": [],
    });
    let mut command = dsg(root);
    command
        .args(["docker-reuse", "--context", "fixture", "--expectation-json"])
        .arg(expected.to_string())
        .env("PATH", root)
        .env("DOCKER_HOST", "synthetic-invalid")
        .env("DOCKER_CONTEXT", "synthetic-invalid")
        .env("DOCKER_API_VERSION", "synthetic-invalid")
        .env("DSG_FIXTURE_SCANS", &scan_count)
        .env("DSG_FIXTURE_ENGINE", ENGINE)
        .env("DSG_FIXTURE_CONTAINER", CONTAINER)
        .env("DSG_FIXTURE_REPLACEMENT", REPLACEMENT)
        .env("DSG_FIXTURE_IMAGE", IMAGE)
        .env("DSG_FIXTURE_DIGEST", DIGEST)
        .env("DSG_FIXTURE_HASH", HASH)
        .env("DSG_FIXTURE_MODE", mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if projection.is_some() {
        command
            .arg("--compose-projection-stdin")
            .stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(projection) = projection {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(projection.to_string().as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "CLI exit {:?}",
        output.status.code()
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["command"], "docker-reuse");
    (
        output.status.code(),
        body["result"].clone(),
        fs::read_to_string(scan_count).unwrap(),
    )
}

fn projection() -> Value {
    let baseline = json!({
        "service": {
            "image": format!("docker.io/library/alpine@{DIGEST}"),
            "platform": "linux/arm64", "init": false, "read_only": false, "volumes": [],
        },
        "build_inputs_sha256": null,
    });
    json!({ "baseline_config_hash": HASH, "baseline": baseline, "desired": baseline })
}

#[test]
fn compose_projection_cli_checks_live_baseline_twice_and_preserves_legacy_output() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    install_fixture(root);
    let init = dsg(root)
        .args(["init", "--project-id", "shop"])
        .output()
        .unwrap();
    assert!(init.status.success());

    let (exit, legacy, scans) = reuse(root, "healthy", None);
    assert_eq!(exit, Some(0));
    assert_eq!(legacy["reason"], json!({"code": "verified"}));
    assert_eq!(legacy["assessment"]["container_id"], CONTAINER);
    assert_eq!(legacy.as_object().unwrap().len(), 3);
    assert_eq!(scans, "2\n");

    let (exit, same, scans) = reuse(root, "healthy", Some(projection()));
    assert_eq!(exit, Some(0));
    assert_eq!(
        same,
        json!({"verified": true, "baseline": legacy, "compose": {"status": "same"}})
    );
    assert_eq!(scans, "2\n");

    let mut changed = projection();
    changed["desired"]["service"]["read_only"] = json!(true);
    let (exit, recreate, scans) = reuse(root, "healthy", Some(changed.clone()));
    assert_eq!(exit, Some(5));
    assert_eq!(recreate["verified"], false);
    assert_eq!(
        recreate["compose"],
        json!({"status": "changed", "action": "recreate"})
    );
    assert_eq!(scans, "2\n");

    changed["desired"]["build_inputs_sha256"] = json!(HASH);
    let (exit, rebuild, scans) = reuse(root, "healthy", Some(changed.clone()));
    assert_eq!(exit, Some(5));
    assert_eq!(rebuild["verified"], false);
    assert_eq!(
        rebuild["compose"],
        json!({"status": "changed", "action": "rebuild"})
    );
    assert_eq!(scans, "2\n");

    for (mode, reason) in [
        ("replaced", "candidate_changed"),
        ("unhealthy", "initial_unverified"),
        ("wrong-platform", "initial_unverified"),
    ] {
        let (exit, unverified, scans) = reuse(root, mode, Some(changed.clone()));
        assert_eq!(exit, Some(5));
        assert_eq!(unverified["verified"], false);
        assert_eq!(unverified["baseline"]["reason"]["code"], reason);
        assert_eq!(
            unverified["compose"],
            json!({"status": "unknown", "reason": "baseline_unverified"})
        );
        assert_eq!(scans, "2\n");
    }

    changed["baseline_config_hash"] = json!("0".repeat(64));
    let (exit, unmatched, scans) = reuse(root, "healthy", Some(changed.clone()));
    assert_eq!(exit, Some(5));
    assert_eq!(
        unmatched["compose"],
        json!({"status": "unknown", "reason": "baseline_mismatch"})
    );
    assert_eq!(scans, "0\n");

    changed["desired"]["service"]["extra"] = json!("synthetic-unrecognized-field");
    let (exit, invalid, scans) = reuse(root, "healthy", Some(changed));
    assert_eq!(exit, Some(5));
    assert_eq!(invalid["baseline"], Value::Null);
    assert_eq!(
        invalid["compose"],
        json!({"status": "unknown", "reason": "invalid_projection"})
    );
    assert!(!invalid.to_string().contains("synthetic-unrecognized-field"));
    assert_eq!(scans, "0\n");
}
