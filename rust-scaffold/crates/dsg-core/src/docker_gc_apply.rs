//! Approval and recovery for a Docker resource behind a verified, exclusive controller.
use crate::docker_cache::{valid_cache_record_id, CacheRecordState};
use crate::docker_image_gc::{
    revalidate_images, valid_full_image_id, valid_owner_fingerprint, ImageGcPlan,
};
use crate::registry::{
    managed_docker_identity, GcApplyStatus, Lease, Registry, RegistryError, StoredPlan,
    MANAGED_DOCKER_TARGET,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use thiserror::Error;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) mod dispatch;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod moby;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub use dispatch::{serve_moby_image_request, DockerGcDispatch, DockerGcDispatchResult};

const PLAN_TTL: Duration = Duration::from_secs(3600);
const APPROVAL_TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerApplyGate {
    #[default]
    PreserveUnverifiedControl,
    Controlled,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerGcKind {
    Image,
    BuildCache,
}

impl DockerGcKind {
    pub(crate) fn valid_id(self, id: &str) -> bool {
        match self {
            Self::Image => valid_full_image_id(id),
            Self::BuildCache => valid_cache_record_id(id),
        }
    }
}

#[derive(Debug, Error)]
pub enum DockerGcError {
    #[error("Docker 자원 변경을 제어하는 경계를 확인할 수 없습니다")]
    UnverifiedControl,
    #[error("Docker 자원의 일치 상태와 소유권을 확인할 수 없습니다")]
    UnverifiedCandidate,
    #[error("정리 의도 또는 일회 승인 번호를 만들 수 없습니다")]
    RandomUnavailable,
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DockerGcResult {
    pub status: GcApplyStatus,
    pub reclaimed_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) project: i64,
    pub(crate) owner: String,
    pub(crate) kind: DockerGcKind,
    pub(crate) id: String,
    pub(crate) endpoint: String,
    pub(crate) generation: String,
    pub(crate) snapshot: String,
}

impl Binding {
    fn valid_for(&self, project: i64, owner: &str, kind: DockerGcKind, id: &str) -> bool {
        project > 0
            && self.project == project
            && self.owner == owner
            && self.kind == kind
            && self.id == id
            && kind.valid_id(id)
            && [
                &self.owner,
                &self.endpoint,
                &self.generation,
                &self.snapshot,
            ]
            .into_iter()
            .all(|value| valid_owner_fingerprint(value))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub(crate) operation: String,
    pub(crate) policy: String,
    pub(crate) binding: Binding,
}

impl Intent {
    fn load(payload: &Value, project: i64, owner: &str) -> Result<Self, DockerGcError> {
        managed_docker_identity(payload)?;
        let intent: Self = serde_json::from_value(payload[MANAGED_DOCKER_TARGET].clone())
            .map_err(|_| DockerGcError::UnverifiedCandidate)?;
        if !valid_owner_fingerprint(&intent.operation)
            || !valid_owner_fingerprint(&intent.policy)
            || !intent
                .binding
                .valid_for(project, owner, intent.binding.kind, &intent.binding.id)
        {
            return Err(DockerGcError::UnverifiedCandidate);
        }
        Ok(intent)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Usage {
    Image(ImageGcPlan),
    BuildCache(CacheRecordState),
}

pub(crate) struct Observation {
    pub(crate) binding: Binding,
    pub(crate) unreferenced: Option<bool>,
    pub(crate) usage: Usage,
}

impl Observation {
    fn eligible_for(&self, project: i64, owner: &str, kind: DockerGcKind, id: &str) -> bool {
        self.binding.valid_for(project, owner, kind, id)
            && self.unreferenced == Some(true)
            && match (&self.usage, kind) {
                (Usage::Image(plan), DockerGcKind::Image) => {
                    plan.image_id == id
                        && plan.engine_id.is_some()
                        && plan.fingerprint.as_deref() == Some(self.binding.snapshot.as_str())
                        && revalidate_images(plan, plan)
                }
                (Usage::BuildCache(state), DockerGcKind::BuildCache) => {
                    state.reclaim_blocker().is_none()
                }
                _ => false,
            }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) enum Effect {
    Applied(u64),
    Preserved,
}

pub(crate) struct Receipt {
    pub(crate) intent: Intent,
    pub(crate) effect: Effect,
}

pub(crate) trait ControlledDockerGc {
    // Provenance must come from the controller that created and exclusively owns this target.
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation>;

    // Compare the exact current binding and reference state under the same exclusion
    // that guards the single-target change and durable receipt.
    fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt>;

    // Read only. The presence or absence of the resource does not establish a receipt.
    fn receipt(&mut self, intent: &Intent) -> Option<Receipt>;
}

pub struct DockerGcService<'a> {
    controller: Option<&'a mut dyn ControlledDockerGc>,
}

impl DockerGcService<'static> {
    pub fn external() -> Self {
        Self { controller: None }
    }
}

impl<'a> DockerGcService<'a> {
    pub fn apply_gate(&self) -> DockerApplyGate {
        if self.controller.is_some() {
            DockerApplyGate::Controlled
        } else {
            DockerApplyGate::PreserveUnverifiedControl
        }
    }

    fn controller(&mut self) -> Result<&mut (dyn ControlledDockerGc + 'a), DockerGcError> {
        self.controller
            .as_deref_mut()
            .ok_or(DockerGcError::UnverifiedControl)
    }

    pub fn plan(
        &mut self,
        registry: &mut Registry,
        project: i64,
        policy_digest: &str,
        kind: DockerGcKind,
        id: &str,
    ) -> Result<StoredPlan, DockerGcError> {
        plan_with_controller(
            self.controller()?,
            registry,
            project,
            policy_digest,
            kind,
            id,
            |_| true,
        )
    }

    pub fn approve(
        &mut self,
        registry: &mut Registry,
        project: i64,
        plan_id: i64,
        policy_digest: &str,
    ) -> Result<String, DockerGcError> {
        let controller = self.controller()?;
        let policy = policy_digest.to_ascii_lowercase();
        let payload = registry.read_current_gc_plan(project, plan_id, &policy)?;
        let owner = registry.docker_gc_project_owner(project)?;
        let intent = Intent::load(&payload, project, &owner)?;
        if intent.policy != policy {
            return Err(RegistryError::GcPolicyChanged.into());
        }
        if registry.has_open_execution_lease()? || !matches_current(controller, &intent.binding) {
            return Err(DockerGcError::UnverifiedCandidate);
        }
        let approval = random_hex()?;
        registry.approve_managed_docker_gc(
            project,
            plan_id,
            policy_digest,
            &managed_docker_identity(&payload)?,
            &hash(approval.as_bytes()),
            APPROVAL_TTL,
        )?;
        Ok(approval)
    }

    pub fn apply(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        plan_id: i64,
        policy_digest: &str,
        approval: &str,
    ) -> Result<DockerGcResult, DockerGcError> {
        let controller = self.controller()?;
        if !valid_owner_fingerprint(approval) {
            return Err(RegistryError::GcApprovalInvalid.into());
        }
        let _exclusive = registry.lock_docker_gc_slots(lease)?;
        let digest = hash(approval.as_bytes());
        let approved =
            registry.check_managed_docker_gc_approval(lease, plan_id, policy_digest, &digest)?;
        let owner = registry.docker_gc_project_owner(lease.project_id)?;
        let intent = Intent::load(&approved.payload, lease.project_id, &owner)?;
        if !intent.policy.eq_ignore_ascii_case(policy_digest) {
            return Err(RegistryError::GcPolicyChanged.into());
        }
        let verified = matches_current(controller, &intent.binding);
        registry.claim_managed_docker_gc(
            lease,
            plan_id,
            policy_digest,
            &digest,
            &approved.identity,
        )?;
        let result = if verified {
            resolve_receipt(&intent, controller.apply_if_current(&intent))
        } else {
            DockerGcResult {
                status: GcApplyStatus::Preserved,
                reclaimed_bytes: 0,
            }
        };
        registry.finish_managed_docker_gc(lease, plan_id, result.status, result.reclaimed_bytes)?;
        Ok(result)
    }

    pub fn recover(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        plan_id: i64,
    ) -> Result<DockerGcResult, DockerGcError> {
        let controller = self.controller()?;
        let _exclusive = registry.lock_docker_gc_slots(lease)?;
        let pending = registry.pending_managed_docker_gc(lease, plan_id)?;
        let owner = registry.docker_gc_project_owner(lease.project_id)?;
        let intent = Intent::load(&pending.payload, lease.project_id, &owner)?;
        let result = resolve_receipt(&intent, controller.receipt(&intent));
        registry.finish_managed_docker_gc(lease, plan_id, result.status, result.reclaimed_bytes)?;
        Ok(result)
    }
}

fn plan_with_controller(
    controller: &mut (impl ControlledDockerGc + ?Sized),
    registry: &mut Registry,
    project: i64,
    policy_digest: &str,
    kind: DockerGcKind,
    id: &str,
    verify_binding: impl FnOnce(&Binding) -> bool,
) -> Result<StoredPlan, DockerGcError> {
    if !kind.valid_id(id) || registry.has_open_execution_lease()? {
        return Err(DockerGcError::UnverifiedCandidate);
    }
    let policy = policy_digest.to_ascii_lowercase();
    if !valid_owner_fingerprint(&policy) {
        return Err(RegistryError::InvalidPolicyDigest.into());
    }
    let owner = registry.docker_gc_project_owner(project)?;
    let observation = controller
        .observe(&owner, kind, id)
        .filter(|observed| {
            observed.eligible_for(project, &owner, kind, id) && verify_binding(&observed.binding)
        })
        .ok_or(DockerGcError::UnverifiedCandidate)?;
    let intent = Intent {
        operation: random_hex()?,
        policy,
        binding: observation.binding,
    };
    let payload = json!({ (MANAGED_DOCKER_TARGET): intent });
    Ok(registry.save_managed_docker_gc_plan(project, policy_digest, &payload, PLAN_TTL)?)
}

fn matches_current(controller: &mut (impl ControlledDockerGc + ?Sized), binding: &Binding) -> bool {
    controller
        .observe(&binding.owner, binding.kind, &binding.id)
        .is_some_and(|observed| {
            observed.binding == *binding
                && observed.eligible_for(binding.project, &binding.owner, binding.kind, &binding.id)
        })
}

fn resolve_receipt(intent: &Intent, receipt: Option<Receipt>) -> DockerGcResult {
    match receipt
        .filter(|receipt| receipt.intent == *intent)
        .map(|receipt| receipt.effect)
    {
        Some(Effect::Applied(bytes)) if i64::try_from(bytes).is_ok() => DockerGcResult {
            status: GcApplyStatus::Applied,
            reclaimed_bytes: bytes,
        },
        Some(Effect::Preserved) => DockerGcResult {
            status: GcApplyStatus::Preserved,
            reclaimed_bytes: 0,
        },
        _ => DockerGcResult {
            status: GcApplyStatus::RecoveryRequired,
            reclaimed_bytes: 0,
        },
    }
}

fn random_hex() -> Result<String, DockerGcError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| DockerGcError::RandomUnavailable)?;
    Ok(hash(&bytes))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos"))]
mod live_fixture;
