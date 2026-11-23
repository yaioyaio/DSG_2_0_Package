use super::{check_budget, open_registry, read_scan, registry_error, Cli, CliError, Output};
use crate::cleanup_cli;
use dsg_core::cleanup::CleanupEvent;
use dsg_core::docker::{Mount, ReuseRequest};
use dsg_core::docker_command::DockerCli;
use dsg_core::policy::Policy;
use dsg_core::registry::{AuditAction, AuditOutcome, Registry, RegistryError};
use dsg_core::reuse::verify_reuse;
use dsg_core::runner::{process_group_gone, run_with_target, RunEvent, RunOutcome};
use dsg_core::{evaluate_command, Decision, Verdict};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
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
    argv: &[String],
) -> Result<Output, CliError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(CliError::new(
            "RUNNER_UNAVAILABLE",
            "이 플랫폼의 프로세스 트리 격리는 검증 대기 상태입니다",
        ));
    }
    let mut registry = open_registry(cli)?;
    let project_id = project_id(&registry, root, policy)?;
    let verdict = evaluate_command(argv);
    if matches!(
        verdict.decision,
        Decision::Deny | Decision::ApprovalRequired
    ) {
        return rejected_exec(&mut registry, project_id, verdict);
    }
    if let Some(verdict) = check_budget(policy, &read_scan(root, policy)?) {
        return rejected_exec(&mut registry, project_id, verdict);
    }
    if auto_clean_cargo && !registered_cargo(argv) {
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
    let result = run_with_target(
        root,
        argv,
        Duration::from_secs(u64::from(timeout)),
        cli.json,
        &cancellation,
        artifact.as_ref().map(|output| output.target.as_path()),
        |event| {
            match event {
                RunEvent::Started(pid) => registry.attach_lease_process(&lease, pid),
                RunEvent::Heartbeat => registry.heartbeat(&lease, LEASE_TTL),
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
                "대상 명령을 시작할 수 없습니다",
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
    if report.tree_confirmed_gone {
        registry
            .finish_lease(lease, outcome, code)
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
    Ok(Output {
        command: "exec",
        text: format!("관리형 실행 · {code}"),
        data: json!({"run": report, "reason_code": code, "lease_released": report.tree_confirmed_gone, "cleanup":cleanup}),
        exit,
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
    let backend = DockerCli::new(context)
        .map_err(|_| CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요"))?;
    let mut registry = open_registry(cli)?;
    let project_id = project_id(&registry, root, policy)?;
    let lease = registry
        .acquire_lease(project_id, &scope_key(root), LEASE_TTL, process_group_gone)
        .map_err(lease_error)?;
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
    let verification = verify_reuse(&backend, 64, &expected);
    let (outcome, code, text, exit) = if verification.verified {
        (
            AuditOutcome::Success,
            "REUSE_VERIFIED",
            "실행 중인 Compose 컨테이너를 재사용할 수 있습니다",
            0,
        )
    } else {
        (
            AuditOutcome::Incomplete,
            "REUSE_UNVERIFIED",
            "재사용 대상을 확인할 수 없습니다",
            5,
        )
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
        data: json!(verification),
        exit,
    })
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
