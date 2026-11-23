use super::{local_state_dir, open_registry, policy_digest, registry_error, Cli, CliError, Output};
use crate::execution::{lease_error, project_id, scope_key};
use dsg_core::cleanup::CleanupQueue;
use dsg_core::gc::{self, GcStatus};
use dsg_core::policy::Policy;
use dsg_core::registry::{AuditOutcome, GcApplyStatus, RegistryError};
use dsg_core::runner::process_group_gone;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::{self, BufRead, Read as _};
use std::path::Path;
use std::time::Duration;

const APPROVAL_TTL: Duration = Duration::from_secs(600);

pub fn own(cli: &Cli, root: &Path, policy: &Policy, candidate: &str) -> Result<Output, CliError> {
    let mut registry = open_registry(cli)?;
    let project = project_id(&registry, root, policy)?;
    let proof = gc::ownership(root, policy, candidate).map_err(|_| {
        CliError::new(
            "RESOURCE_UNKNOWN",
            "관리 경로의 소유권과 보호 상태를 확인할 수 없습니다",
        )
    })?;
    registry
        .register_gc_output(project, candidate, &proof)
        .map_err(gc_error)?;
    Ok(Output::success(
        "gc-own",
        json!({"candidate":candidate,"registered":true}),
        "생성 산출물을 등록했습니다".to_owned(),
    ))
}

pub fn approve(
    cli: &Cli,
    root: &Path,
    policy: &Policy,
    plan_id: i64,
    candidate: &str,
) -> Result<Output, CliError> {
    let mut registry = open_registry(cli)?;
    let project = project_id(&registry, root, policy)?;
    let proof = gc::ownership(root, policy, candidate).map_err(|_| {
        CliError::new(
            "RESOURCE_UNKNOWN",
            "대상 산출물의 소유권과 보호 상태를 확인할 수 없습니다",
        )
    })?;
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|_| CliError::new("NEEDS_APPROVAL", "일회용 승인을 만들 수 없습니다"))?;
    let mut approval = String::with_capacity(64);
    for byte in nonce {
        let _ = write!(&mut approval, "{byte:02x}");
    }
    registry
        .approve_gc(
            project,
            plan_id,
            candidate,
            &policy_digest(policy)?,
            &proof,
            &hash(&approval),
            APPROVAL_TTL,
        )
        .map_err(gc_error)?;
    Ok(Output::success(
        "gc-approve",
        json!({"plan_id":plan_id,"candidate":candidate,"approval":approval}),
        approval,
    ))
}

pub fn apply(
    cli: &Cli,
    root: &Path,
    policy: &Policy,
    plan_id: i64,
    recover: bool,
) -> Result<Output, CliError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(CliError::new(
            "GC_UNAVAILABLE",
            "이 플랫폼의 정리 적용 경계는 검증 대기 상태입니다",
        ));
    }
    let approval = if recover {
        None
    } else {
        Some(read_approval()?)
    };
    let digest = policy_digest(policy)?;
    let mut registry = open_registry(cli)?;
    let project = project_id(&registry, root, policy)?;
    let lease = registry
        .acquire_lease(
            project,
            &scope_key(root),
            Duration::from_secs(30),
            process_group_gone,
        )
        .map_err(lease_error)?;
    if let Err(error) = registry.attach_lease_process(&lease, std::process::id()) {
        registry
            .finish_lease(lease, AuditOutcome::Failure, "GC_OWNER_UNCONFIRMED")
            .map_err(lease_error)?;
        return Err(gc_error(error));
    }
    let claim = if let Some(approval) = approval {
        registry.claim_gc(&lease, plan_id, &digest, &hash(&approval))
    } else {
        registry.pending_gc(&lease, plan_id)
    };
    let claim = match claim {
        Ok(claim) => claim,
        Err(error) => {
            registry
                .finish_lease(lease, AuditOutcome::Deny, "GC_CLAIM_DENIED")
                .map_err(lease_error)?;
            return Err(gc_error(error));
        }
    };
    let report = if recover {
        gc::recover(
            root,
            policy,
            claim.plan_id,
            &claim.relative_path,
            &claim.identity,
            &claim.payload,
        )
    } else {
        gc::apply(
            root,
            policy,
            claim.plan_id,
            &claim.relative_path,
            &claim.identity,
            &claim.payload,
        )
    };
    let (status, result, code, exit) = match report.status {
        GcStatus::Applied => (
            GcApplyStatus::Applied,
            AuditOutcome::Success,
            "GC_APPLIED",
            0,
        ),
        GcStatus::Preserved => (
            GcApplyStatus::Preserved,
            AuditOutcome::Incomplete,
            "GC_PRESERVED",
            if recover && report.reason == "gc_artifacts_restored" {
                0
            } else {
                5
            },
        ),
        GcStatus::RecoveryRequired => (
            GcApplyStatus::RecoveryRequired,
            AuditOutcome::Incomplete,
            "GC_RECOVERY_REQUIRED",
            5,
        ),
    };
    registry
        .finish_gc(&lease, plan_id, status, report.reclaimed_bytes)
        .map_err(gc_error)?;
    registry
        .finish_lease(lease, result, "GC_LEASE_RELEASED")
        .map_err(lease_error)?;
    Ok(Output {
        command: if recover { "gc-recover" } else { "gc-apply" },
        text: format!("{code} · 회수 {} B", report.reclaimed_bytes),
        data: json!(report),
        exit,
    })
}

pub fn create_worktree(
    cli: &Cli,
    root: &Path,
    branch: &str,
    path: &Path,
) -> Result<Output, CliError> {
    let _registry = open_registry(cli)?;
    let queue = queue(cli)?;
    queue.create_worktree(root, branch, path).map_err(|_| {
        CliError::new(
            "WORKTREE_UNVERIFIED",
            "보조 워크트리 생성 조건을 확인하세요",
        )
    })?;
    Ok(Output::success(
        "worktree-create",
        json!({"created":1}),
        "등록된 보조 워크트리 1개를 만들었습니다".to_owned(),
    ))
}

pub fn complete_git(cli: &Cli, root: &Path, feature: &str) -> Result<Output, CliError> {
    let _registry = open_registry(cli)?;
    let queue = queue(cli)?;
    let report = queue.complete_git(root, feature).map_err(|_| {
        CliError::new(
            "GIT_CLOSURE_UNVERIFIED",
            "Git 기준과 워크트리 회수 조건을 확인하세요",
        )
    })?;
    let text = format!(
        "Git 완료 정리 · 회수 {}개 · 보존 {}개 · 회수량 {} B",
        report.removed, report.preserved, report.reclaimed_bytes
    );
    Ok(Output::success("cleanup-git", json!(report), text))
}

pub(super) fn queue(cli: &Cli) -> Result<CleanupQueue, CliError> {
    CleanupQueue::open(&local_state_dir(cli)?).map_err(|_| {
        CliError::new(
            "CLEANUP_QUEUE_UNAVAILABLE",
            "자동 정리 상태 저장소를 확인하세요",
        )
    })
}

fn read_approval() -> Result<String, CliError> {
    let rejected = || CliError::new("NEEDS_APPROVAL", "표준 입력으로 일회용 승인을 전달하세요");
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take(67)
        .read_until(b'\n', &mut input)
        .map_err(|_| rejected())?;
    let token = std::str::from_utf8(&input)
        .map_err(|_| rejected())?
        .trim_end_matches(['\r', '\n']);
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(rejected());
    }
    Ok(token.to_owned())
}

fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn gc_error(error: RegistryError) -> CliError {
    match error {
        RegistryError::GcExpired => {
            CliError::new("PLAN_EXPIRED", "정리 계획 또는 승인이 만료됐습니다")
        }
        RegistryError::GcApprovalInvalid => {
            CliError::new("NEEDS_APPROVAL", "승인을 다시 확인하세요")
        }
        RegistryError::GcPolicyChanged
        | RegistryError::GcCandidateMismatch
        | RegistryError::GcOwnershipConflict => CliError::new(
            "RESOURCE_CHANGED",
            "계획과 현재 정책·소유권·상태를 대조하세요",
        ),
        RegistryError::GcStateConflict | RegistryError::GcPlanNotFound => {
            CliError::new("GC_STATE_CONFLICT", "정리 계획의 상태를 확인하세요")
        }
        _ => registry_error(error),
    }
}
