#[path = "build_provider.rs"]
mod build_provider;

use super::{
    check_budget, local_state_dir, open_registry, read_scan, registry_error, Cli, CliError, Output,
};
use crate::cleanup_cli;
use dsg_core::cleanup::{CleanupEvent, PlaywrightEvidence};
use dsg_core::compose::{
    ComposeAction, ComposeAssessment, ComposeComparison, ComposeUnknown, ComposeVerification,
    MAX_PROJECTION_BYTES,
};
use dsg_core::docker::{Mount, ReuseRequest};
use dsg_core::docker_command::DockerCli;
use dsg_core::policy::Policy;
use dsg_core::registry::{AuditAction, AuditOutcome, Registry, RegistryError};
use dsg_core::reuse::verify_reuse;
use dsg_core::runner::{
    process_group_gone, run_configured, FileDescriptorLimit, RunEvent, RunOutcome, RunSettings,
};
use dsg_core::{evaluate_command, Decision, Verdict};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const LEASE_TTL: Duration = Duration::from_secs(30);

pub fn exec(
    cli: &Cli,
    root: &Path,
    policy: &Policy,
    timeout: u16,
    auto_clean_cargo: bool,
    provider: Option<&str>,
    trace_ttl_seconds: Option<u64>,
    argv: &[String],
) -> Result<Output, CliError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(CliError::new(
            "RUNNER_UNAVAILABLE",
            "이 플랫폼의 프로세스 트리 격리는 검증 대기 상태입니다",
        ));
    }
    if trace_ttl_seconds.is_some() && provider != Some("playwright") {
        return Err(CliError::new(
            "CONFIG_INVALID",
            "trace TTL은 Playwright 실행에서 지정하세요",
        ));
    }
    let mut registry = open_registry(cli)?;
    let project_id = project_id(&registry, root, policy)?;
    let mut command = provider
        .map(|name| build_provider::prepare(name, argv))
        .transpose()?;
    let planned = command.as_ref().map_or(argv, |c| c.argv.as_slice());
    let verdict = evaluate_command(planned);
    if matches!(
        verdict.decision,
        Decision::Deny | Decision::ApprovalRequired
    ) {
        return rejected_exec(&mut registry, project_id, verdict);
    }
    if let Some(verdict) = check_budget(policy, &read_scan(root, policy)?) {
        return rejected_exec(&mut registry, project_id, verdict);
    }
    if auto_clean_cargo && !registered_cargo(planned) {
        return Err(CliError::new(
            "CLEANUP_PROVIDER_UNKNOWN",
            "자동 정리는 등록된 Cargo build·check·test 대상에서만 사용할 수 있습니다",
        ));
    }
    let queue = auto_clean_cargo
        .then(|| cleanup_cli::queue(cli))
        .transpose()?;

    let cancellation = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&cancellation);
    ctrlc::set_handler(move || signal_flag.store(true, Ordering::Release)).map_err(|_| {
        CliError::new(
            "RUNNER_UNAVAILABLE",
            "실행 중단 제어를 초기화할 수 없습니다",
        )
    })?;
    let lease = registry
        .acquire_lease(project_id, &scope_key(root), LEASE_TTL, process_group_gone)
        .map_err(lease_error)?;
    let mut evidence = None;
    if let Some(playwright) = command.as_mut().filter(|c| c.provider == "playwright") {
        match local_state_dir(cli)
            .and_then(|state| prepare_playwright_output(&state, trace_ttl_seconds.is_some()))
        {
            Ok(run) => {
                playwright.argv.extend([
                    "--output".to_owned(),
                    run.output.clone(),
                    "--reporter".to_owned(),
                    run.reporter.clone(),
                ]);
                evidence = Some(run);
            }
            Err(error) => {
                registry
                    .finish_lease(lease, AuditOutcome::Failure, "EVIDENCE_PREPARE_FAILED")
                    .map_err(lease_error)?;
                return Err(error);
            }
        }
    }
    let artifact = match queue
        .as_ref()
        .map(|queue| queue.prepare_cargo())
        .transpose()
    {
        Ok(artifact) => artifact,
        Err(_) => {
            registry
                .finish_lease(lease, AuditOutcome::Failure, "CLEANUP_PREPARE_FAILED")
                .map_err(lease_error)?;
            return Err(CliError::new(
                "CLEANUP_QUEUE_UNAVAILABLE",
                "등록된 산출물 위치를 만들 수 없습니다",
            ));
        }
    };
    let result = run_configured(
        root,
        command.as_ref().map_or(argv, |c| c.argv.as_slice()),
        Duration::from_secs(u64::from(timeout)),
        cli.json || command.is_some(),
        &cancellation,
        RunSettings {
            cargo_target: artifact.as_ref().map(|output| output.target.as_path()),
            env: command.as_ref().map_or(&[], |c| c.env),
            file_limit: if provider == Some("gradle") {
                FileDescriptorLimit::Gradle
            } else {
                FileDescriptorLimit::Standard
            },
        },
        |event| {
            match event {
                #[cfg(target_os = "macos")]
                RunEvent::Started(pid) => registry.attach_watched_lease_process(&lease, pid),
                #[cfg(not(target_os = "macos"))]
                RunEvent::Started(pid) => registry.attach_lease_process(&lease, pid),
                RunEvent::Heartbeat => registry.heartbeat(&lease, LEASE_TTL),
                #[cfg(target_os = "macos")]
                RunEvent::ObservationStarted => registry.begin_lease_observation(&lease),
                #[cfg(target_os = "macos")]
                RunEvent::ObservationCompleted(observed) => {
                    registry.commit_lease_observation(&lease, &observed)
                }
            }
            .map_err(|_| io::Error::other("lease update failed"))
        },
    );
    let report = match result {
        Ok(report) => report,
        Err(_) => {
            if let (Some(queue), Some(artifact)) = (&queue, artifact) {
                let _ = queue.complete_cargo(artifact, CleanupEvent::Failure);
            }
            registry
                .finish_lease(lease, AuditOutcome::Failure, "RUNNER_START_FAILED")
                .map_err(lease_error)?;
            return Err(CliError::new(
                "RUNNER_START_FAILED",
                evidence.as_ref().map_or_else(
                    || "대상 명령을 시작할 수 없습니다".to_owned(),
                    |run| {
                        format!(
                            "대상 명령을 시작할 수 없습니다 · 증거 경로 {}",
                            run.relative
                        )
                    },
                ),
            ));
        }
    };
    let (outcome, code, exit) = match report.outcome {
        RunOutcome::Success => (AuditOutcome::Success, "RUN_COMPLETED", 0),
        RunOutcome::Failure => (
            AuditOutcome::Failure,
            "RUN_FAILED",
            report
                .exit_code
                .and_then(|code| u8::try_from(code).ok())
                .filter(|code| *code > 0)
                .unwrap_or(1),
        ),
        RunOutcome::TimedOut => (AuditOutcome::Failure, "RUN_TIMED_OUT", 124),
        RunOutcome::Interrupted => (AuditOutcome::Failure, "RUN_INTERRUPTED", 130),
        RunOutcome::ControlFailure => (AuditOutcome::Incomplete, "RUN_CONTROL_UNVERIFIED", 5),
    };
    let cleanup = match (&queue, artifact) {
        (Some(queue), Some(artifact)) if report.tree_confirmed_gone => {
            let event = match report.outcome {
                RunOutcome::Success => CleanupEvent::Success,
                RunOutcome::Interrupted => CleanupEvent::Interrupted,
                _ => CleanupEvent::Failure,
            };
            queue.complete_cargo(artifact, event).map(|observed| json!(observed))
                .unwrap_or_else(|_| json!({"removed":0,"preserved":1,"reclaimed_bytes":0,"reason":"cleanup_unverified"}))
        }
        (Some(_), Some(_)) => {
            json!({"removed":0,"preserved":1,"reclaimed_bytes":0,"reason":"process_tree_unknown"})
        }
        _ => serde_json::Value::Null,
    };
    let audit_reason = command.as_ref().map(|command| {
        let status = match report.outcome {
            RunOutcome::Success => "SUCCESS",
            RunOutcome::Failure => "FAILURE",
            RunOutcome::Interrupted => "INTERRUPTED",
            RunOutcome::TimedOut => "TIMED_OUT",
            RunOutcome::ControlFailure => "CONTROL_FAILURE",
        };
        format!(
            "PROVIDER_{}_{}_{status}",
            command.provider.to_ascii_uppercase(),
            command.task.to_ascii_uppercase()
        )
    });
    if report.tree_confirmed_gone {
        registry
            .finish_lease(lease, outcome, audit_reason.as_deref().unwrap_or(code))
            .map_err(lease_error)?;
    } else {
        registry
            .record_event(
                Some(project_id),
                AuditAction::ManagedExecution,
                AuditOutcome::Incomplete,
                "RUN_PROCESS_TREE_UNKNOWN",
            )
            .map_err(registry_error)?;
        // The database marker still protects this project until absence can be verified.
        drop(lease);
    }
    let mut data = json!({"run": report, "reason_code": code, "lease_released": report.tree_confirmed_gone, "cleanup":cleanup});
    if let Some(command) = command {
        data["provider"] = json!(command.provider);
        if report.tree_confirmed_gone {
            data["audit_reason"] = json!(audit_reason);
        }
        data["task"] = json!(command.task);
        data["workers"] = json!(1);
    }
    let mut text = format!("관리형 실행 · {code}");
    if let Some(run) = evidence {
        if let Some(seconds) = trace_ttl_seconds {
            let eligible = report.tree_confirmed_gone
                && report.outcome == RunOutcome::Success
                && run.all_passed();
            data["trace_ttl_eligible"] = json!(eligible);
            if report.tree_confirmed_gone
                && matches!(report.outcome, RunOutcome::Success | RunOutcome::Failure)
            {
                let ttl = Duration::from_secs(seconds);
                let scope = scope_key(root);
                data["trace_retention"] = match cleanup_cli::queue(cli) {
                    Ok(queue) => {
                        let registered = if eligible {
                            queue.register_success_traces(&scope, &run.handle, ttl)
                        } else {
                            Ok(0)
                        };
                        let sweep = queue.expire_success_trace(&scope, ttl);
                        json!({"registered": registered.ok(), "sweep": sweep.ok()})
                    }
                    Err(_) => {
                        json!({"registered":null, "sweep":null, "reason":"queue_unavailable"})
                    }
                };
            } else {
                data["trace_retention"] =
                    json!({"registered":0, "sweep":null, "reason":"run_unconfirmed"});
            }
        }
        let path = run.relative;
        text.push_str(&format!(" · 증거 경로 {path}"));
        data["evidence_path"] = json!(path);
    }
    Ok(Output {
        command: "exec",
        text,
        data,
        exit,
    })
}

const REPORTER_FILE: &str = "dsg-trace-reporter.cjs";
const PASSED_MARKER: &str = "all-tests-passed.v1";
const REPORTER: &str = r#"const fs = require('node:fs');
const path = require('node:path');
module.exports = class {
  constructor() { this.complete = true; this.pending = new Set(); this.total = 0; }
  onBegin(_config, suite) {
    this.pending = new Set(suite.allTests());
    this.total = this.pending.size;
  }
  onError() { this.complete = false; }
  onTestEnd(test, result) {
    if (!this.pending.delete(test) || test.expectedStatus !== 'passed' ||
        result.status !== 'passed' || result.retry !== 0) this.complete = false;
  }
  onEnd(result) {
    if (this.complete && this.total > 0 && this.pending.size === 0 && result.status === 'passed') {
      fs.writeFileSync(path.join(__dirname, 'all-tests-passed.v1'), '', {flag:'wx', mode:0o600});
    }
  }
};
"#;

struct PlaywrightRun {
    relative: String,
    output: String,
    reporter: String,
    parent: PathBuf,
    directory: File,
    handle: PlaywrightEvidence,
}

impl PlaywrightRun {
    #[cfg(unix)]
    fn all_passed(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(original) = self.directory.metadata() else {
            return false;
        };
        let Ok(current) = fs::symlink_metadata(&self.parent) else {
            return false;
        };
        if !current.is_dir()
            || current.mode() & 0o077 != 0
            || original.dev() != current.dev()
            || original.ino() != current.ino()
            || current.uid() != unsafe { libc::geteuid() }
        {
            return false;
        }
        fs::symlink_metadata(self.parent.join(PASSED_MARKER)).is_ok_and(|marker| {
            marker.is_file()
                && marker.len() == 0
                && marker.mode() & 0o777 == 0o600
                && marker.uid() == current.uid()
                && marker.nlink() == 1
        })
    }
    #[cfg(not(unix))]
    fn all_passed(&self) -> bool {
        false
    }
}

fn prepare_playwright_output(state: &Path, classify: bool) -> Result<PlaywrightRun, CliError> {
    let unavailable = || {
        CliError::new(
            "EVIDENCE_PREPARE_FAILED",
            "실행 전용 증거 경로를 준비할 수 없습니다",
        )
    };
    let metadata = std::fs::symlink_metadata(state).map_err(|_| unavailable())?;
    if !metadata.is_dir() {
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(unavailable());
        }
    }
    let state = state.canonicalize().map_err(|_| unavailable())?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| unavailable())?;
    let name = format!("playwright-{:032x}", u128::from_be_bytes(random));
    let relative = format!("{name}/output");
    let absolute = state
        .join(&relative)
        .into_os_string()
        .into_string()
        .map_err(|_| unavailable())?;
    let handle = PlaywrightEvidence::create(&state, &name).map_err(|_| unavailable())?;
    let parent = state.join(name);
    let directory = File::open(&parent).map_err(|_| unavailable())?;
    let reporter = if classify {
        let path = parent.join(REPORTER_FILE);
        let path = path
            .to_str()
            .filter(|s| !s.contains(','))
            .ok_or_else(unavailable)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|mut file| file.write_all(REPORTER.as_bytes()))
            .map_err(|_| unavailable())?;
        path.to_owned()
    } else {
        "line".to_owned()
    };
    Ok(PlaywrightRun {
        relative,
        output: absolute,
        reporter,
        parent,
        directory,
        handle,
    })
}

fn registered_cargo(argv: &[String]) -> bool {
    argv.first()
        .and_then(|program| program.rsplit(['/', '\\']).next())
        .is_some_and(|name| name == "cargo" || name == "cargo.exe")
        && argv
            .get(1)
            .is_some_and(|command| matches!(command.as_str(), "build" | "check" | "test"))
        && !argv.iter().skip(2).any(|argument| {
            argument.starts_with("--target-dir")
                || argument.starts_with("--build-dir")
                || argument.starts_with("--config")
        })
}

fn rejected_exec(
    registry: &mut Registry,
    project_id: i64,
    verdict: Verdict,
) -> Result<Output, CliError> {
    let (audit, exit) = if verdict.decision == Decision::Deny {
        (AuditOutcome::Deny, 3)
    } else {
        (AuditOutcome::ApprovalRequired, 4)
    };
    registry
        .record_event(
            Some(project_id),
            AuditAction::ManagedExecution,
            audit,
            verdict.reason_code,
        )
        .map_err(registry_error)?;
    Ok(Output {
        command: "exec",
        text: format!("{} · {}", verdict.decision.as_str(), verdict.message),
        data: json!(verdict),
        exit,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    engine_id: String,
    project: String,
    service: String,
    number: u32,
    config_hash: String,
    image_id: String,
    image_digest: String,
    mounts: Vec<Mount>,
}

pub fn reuse(
    cli: &Cli,
    root: &Path,
    policy: &Policy,
    context: &str,
    input: &str,
    compose_projection_stdin: bool,
) -> Result<Output, CliError> {
    if input.len() > 32 * 1024 {
        return Err(CliError::new(
            "CONFIG_INVALID",
            "Compose 기대 구성의 크기 제한을 확인하세요",
        ));
    }
    let expectation: Expectation = serde_json::from_str(input).map_err(|_| {
        CliError::new(
            "CONFIG_INVALID",
            "Compose 기대 구성의 JSON 형식을 확인하세요",
        )
    })?;
    let expected = ReuseRequest {
        engine_id: &expectation.engine_id,
        project: &expectation.project,
        service: &expectation.service,
        number: expectation.number,
        config_hash: &expectation.config_hash,
        image_id: &expectation.image_id,
        image_digest: &expectation.image_digest,
        mounts: &expectation.mounts,
    };
    let comparison = if compose_projection_stdin {
        match read_compose_comparison(&expected) {
            Ok(comparison) => Some(comparison),
            Err(reason) => {
                return Ok(Output {
                    command: "docker-reuse",
                    text: "Compose 설정 투영을 확인할 수 없습니다".to_owned(),
                    data: json!(ComposeVerification::unknown(reason)),
                    exit: 5,
                });
            }
        }
    } else {
        None
    };
    let backend = DockerCli::new(context)
        .map_err(|_| CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요"))?;
    let mut registry = open_registry(cli)?;
    let project_id = project_id(&registry, root, policy)?;
    let lease = registry
        .acquire_lease(project_id, &scope_key(root), LEASE_TTL, process_group_gone)
        .map_err(lease_error)?;
    let (data, verified, action) = if let Some(comparison) = comparison {
        let verification = comparison.verify(&backend, 64);
        let action = match verification.compose {
            ComposeAssessment::Changed { action } => Some(action),
            _ => None,
        };
        (json!(verification), verification.verified, action)
    } else {
        let verification = verify_reuse(&backend, 64, &expected);
        (json!(verification), verification.verified, None)
    };
    let (outcome, code, text, exit) = match (verified, action) {
        (true, _) => (
            AuditOutcome::Success,
            "REUSE_VERIFIED",
            "실행 중인 Compose 컨테이너를 재사용할 수 있습니다",
            0,
        ),
        (false, Some(ComposeAction::Rebuild)) => (
            AuditOutcome::Warn,
            "COMPOSE_REBUILD_REQUIRED",
            "Compose 이미지 재빌드가 필요합니다",
            5,
        ),
        (false, Some(ComposeAction::Recreate)) => (
            AuditOutcome::Warn,
            "COMPOSE_RECREATE_REQUIRED",
            "Compose 컨테이너 재생성이 필요합니다",
            5,
        ),
        (false, None) => (
            AuditOutcome::Incomplete,
            "REUSE_UNVERIFIED",
            "재사용 대상을 확인할 수 없습니다",
            5,
        ),
    };
    registry
        .record_event(Some(project_id), AuditAction::ComposeReuse, outcome, code)
        .map_err(registry_error)?;
    registry
        .finish_lease(lease, outcome, code)
        .map_err(lease_error)?;
    Ok(Output {
        command: "docker-reuse",
        text: text.to_owned(),
        data,
        exit,
    })
}

fn read_compose_comparison<'a>(
    expected: &'a ReuseRequest<'a>,
) -> Result<ComposeComparison<'a>, ComposeUnknown> {
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_PROJECTION_BYTES as u64 + 1)
        .read_to_end(&mut input)
        .map_err(|_| ComposeUnknown::InputUnavailable)?;
    ComposeComparison::prepare(&input, expected)
}

pub(super) fn project_id(
    registry: &Registry,
    root: &Path,
    policy: &Policy,
) -> Result<i64, CliError> {
    let project = registry
        .find_project(root)
        .map_err(registry_error)?
        .ok_or_else(|| {
            CliError::new(
                "PROJECT_UNKNOWN",
                "관리형 작업 전에 dsg init으로 프로젝트를 등록하세요",
            )
        })?;
    if project.label != policy.project_id || project.policy_version != policy.version {
        return Err(CliError::new(
            "CONFIG_INVALID",
            "등록된 프로젝트 식별자와 현재 정책을 대조하세요",
        ));
    }
    Ok(project.id)
}

pub(super) fn scope_key(root: &Path) -> String {
    format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))
}

pub(super) fn lease_error(error: RegistryError) -> CliError {
    match error {
        RegistryError::LeaseConflict => CliError::new(
            "RESOURCE_ACTIVE",
            "프로젝트에 활성 또는 미확인 실행이 남아 있습니다",
        ),
        RegistryError::LeaseCapacity => {
            CliError::new("EXECUTION_LIMIT", "등록부의 동시 실행 한도에 도달했습니다")
        }
        _ => registry_error(error),
    }
}
