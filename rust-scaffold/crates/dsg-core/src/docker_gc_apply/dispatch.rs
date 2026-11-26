use super::{
    managed_docker_identity, matches_current, plan_with_controller, resolve_receipt,
    valid_owner_fingerprint, Binding, ControlledDockerGc, DockerGcError, DockerGcKind,
    DockerGcResult, GcApplyStatus, Intent, Lease, Observation, Registry, RegistryError, StoredPlan,
    APPROVAL_TTL,
};
use crate::docker_control_local::{AuthorizedWriteRequest, ControllerEvidence, WriteRequest};
use crate::{
    docker_control_local::with_authorized_write_request, policy::Policy, registry::AuditOutcome,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    path::Path,
    time::Duration,
};

/// Implemented by the in-process controller that owns the mutation and receipt boundary.
pub(crate) trait ExclusiveDockerGc: ControlledDockerGc {
    /// The admission identity belongs to the pinned engine used by this backend. The namespace
    /// belongs to the underlying resource store and survives new sockets, policies and processes.
    /// This is local controller evidence, available without an engine query during recovery.
    fn engine(&self) -> Option<ExclusiveEngine<'_>>;

    /// Acquire the native store's mutation exclusion before the registry consumes approval.
    /// Dropping the guard without commit must leave the native resource and ledger unchanged.
    fn prepare(&mut self, intent: &Intent) -> Option<Box<dyn PreparedDockerGc + '_>>;
}

/// Only the native implementation that owns *all* writers for this target and its references
/// may supply this guard. An outer registry slot or an ordinary Docker API socket is insufficient.
pub(crate) trait PreparedDockerGc {
    fn observed(&self) -> &Observation;

    /// Commit one native change and its immutable receipt under the same held exclusion.
    /// Acknowledgement alone never settles the registry. Loss of the response is indeterminate.
    fn commit(self: Box<Self>, claim: ClaimedDockerGc<'_>) -> bool;
}

/// Constructed only after the exact registry claim has committed.
pub(crate) struct ClaimedDockerGc<'a>(&'a Intent);

impl ClaimedDockerGc<'_> {
    #[allow(dead_code)] // Used by in-process native implementations.
    pub(crate) fn intent(&self) -> &Intent {
        self.0
    }
}

pub(crate) struct ExclusiveEngine<'a> {
    pub(crate) admission: &'a ControllerEvidence,
    pub(crate) namespace: &'a str,
}

/// Dispatch one admitted write inside a protected controller's exclusive Docker boundary.
/// The backend is installed by trusted controller code. Admission evidence alone cannot install
/// it or upgrade a public observer to a resource controller.
pub struct DockerGcDispatch<'a> {
    backend: &'a mut dyn ExclusiveDockerGc,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DockerGcDispatchResult {
    Approved { intent_id: String },
    Applied(DockerGcResult),
    Recovered(DockerGcResult),
}

impl<'a> DockerGcDispatch<'a> {
    // Reserved for assembly by a verified, same-process exclusive controller.
    #[allow(dead_code)]
    pub(crate) fn new(backend: &'a mut dyn ExclusiveDockerGc) -> Self {
        Self { backend }
    }

    /// Record the controller's own observation for future approval. Resource ownership and the
    /// engine binding come from the backend, while the write request selects a stored plan.
    pub fn plan(
        &mut self,
        registry: &mut Registry,
        project: i64,
        project_policy_digest: &str,
        kind: DockerGcKind,
        id: &str,
    ) -> Result<StoredPlan, DockerGcError> {
        registry.check_protected_docker_gc_state()?;
        let engine = self
            .backend
            .engine()
            .ok_or(DockerGcError::UnverifiedControl)?;
        let namespace = engine.namespace.to_owned();
        let generation = engine.admission.generation.clone();
        plan_with_controller(
            self.backend,
            registry,
            project,
            project_policy_digest,
            kind,
            id,
            |binding| binding.endpoint == namespace && binding.generation == generation,
        )
    }

    /// Consume the kernel-issued authority for this dispatch. The controller supplies its own
    /// protected registry, project lease and current project-policy digest. Recovery uses the
    /// saved intent and therefore does not consult the current project-policy digest.
    pub fn dispatch(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        project_policy_digest: &str,
        request: AuthorizedWriteRequest<'_>,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        self.dispatch_with_policy(
            registry,
            lease,
            project_policy_digest,
            &request,
            || true,
            false,
        )
    }

    fn dispatch_with_policy(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        project_policy_digest: &str,
        request: &AuthorizedWriteRequest<'_>,
        policy_current: impl Fn() -> bool,
        image_only: bool,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        registry.check_protected_docker_gc_state()?;
        let ownership = request
            .pin_backend_ownership()
            .map_err(|_| DockerGcError::UnverifiedControl)?;
        let epoch = DispatchEpoch {
            policy: request.policy_digest(),
            controller: request.controller(),
            engine: request.engine(),
            approver: request.approver(),
            executor: request.executor(),
        };
        self.dispatch_request(
            registry,
            lease,
            project_policy_digest,
            request.request(),
            &epoch,
            || {
                request.check_current().is_ok()
                    && ownership.check_current().is_ok()
                    && policy_current()
            },
            |binding| {
                (!image_only || binding.kind == DockerGcKind::Image)
                    && ownership.permits(request.request(), binding)
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_request(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        project_policy_digest: &str,
        request: &WriteRequest,
        epoch: &DispatchEpoch<'_>,
        request_current: impl Fn() -> bool,
        assignment_permits: impl Fn(&Binding) -> bool,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        let project = request.project_id();
        if project != lease.project_id {
            return Err(RegistryError::InvalidLeaseOwner.into());
        }
        let _exclusive = registry.lock_docker_gc_slots(lease)?;
        let endpoint = self
            .engine_namespace(epoch.engine)
            .filter(|_| request_current())
            .ok_or(DockerGcError::UnverifiedControl)?
            .to_owned();
        let owner = registry.docker_gc_project_owner(project)?;

        match request {
            WriteRequest::Plan { .. } => Err(DockerGcError::UnverifiedControl),
            WriteRequest::Approve { plan_id, .. } => {
                let policy = project_policy_digest.to_ascii_lowercase();
                let payload = registry.read_current_gc_plan(project, *plan_id, &policy)?;
                let intent = Intent::load(&payload, project, &owner)?;
                require_current_intent(&intent, &policy, &endpoint, epoch.engine)?;
                if !assignment_permits(&intent.binding) {
                    return Err(DockerGcError::UnverifiedControl);
                }
                if !matches_current(self.backend, &intent.binding) {
                    return Err(DockerGcError::UnverifiedCandidate);
                }
                if !request_current()
                    || self.engine_namespace(epoch.engine) != Some(endpoint.as_str())
                {
                    return Err(DockerGcError::UnverifiedControl);
                }
                registry.approve_managed_docker_gc(
                    project,
                    *plan_id,
                    &policy,
                    &managed_docker_identity(&payload)?,
                    &epoch.approval_digest(project, *plan_id),
                    APPROVAL_TTL,
                )?;
                Ok(DockerGcDispatchResult::Approved {
                    intent_id: intent.operation,
                })
            }
            WriteRequest::Apply { plan_id, .. } => {
                let policy = project_policy_digest.to_ascii_lowercase();
                let digest = epoch.approval_digest(project, *plan_id);
                let approved =
                    registry.check_managed_docker_gc_approval(lease, *plan_id, &policy, &digest)?;
                let intent = Intent::load(&approved.payload, project, &owner)?;
                require_current_intent(&intent, &policy, &endpoint, epoch.engine)?;
                if !assignment_permits(&intent.binding) {
                    return Err(DockerGcError::UnverifiedControl);
                }
                let prepared = self
                    .backend
                    .prepare(&intent)
                    .ok_or(DockerGcError::UnverifiedCandidate)?;
                let binding = &intent.binding;
                let observation = prepared.observed();
                if observation.binding != *binding
                    || !observation.eligible_for(project, &owner, binding.kind, &binding.id)
                {
                    return Err(DockerGcError::UnverifiedCandidate);
                }
                if !request_current() {
                    return Err(DockerGcError::UnverifiedControl);
                }
                registry.claim_managed_docker_gc(
                    lease,
                    *plan_id,
                    &policy,
                    &digest,
                    &approved.identity,
                )?;
                let result = if request_current() {
                    if prepared.commit(ClaimedDockerGc(&intent)) {
                        // Always read the durable backend ledger. A commit response or an
                        // absent target alone cannot prove the operation's outcome.
                        self.read_exact_receipt(&intent, epoch.engine, &request_current)?
                    } else {
                        resolve_receipt(&intent, None)
                    }
                } else {
                    drop(prepared);
                    DockerGcResult {
                        status: GcApplyStatus::Preserved,
                        reclaimed_bytes: 0,
                    }
                };
                registry.finish_managed_docker_gc(
                    lease,
                    *plan_id,
                    result.status,
                    result.reclaimed_bytes,
                )?;
                Ok(DockerGcDispatchResult::Applied(result))
            }
            WriteRequest::Recover { intent_id, .. } => {
                let pending = registry.pending_managed_docker_gc_by_intent(lease, intent_id)?;
                let intent = Intent::load(&pending.payload, project, &owner)?;
                if intent.operation != *intent_id
                    || intent.binding.endpoint != endpoint
                    || !assignment_permits(&intent.binding)
                {
                    return Err(DockerGcError::UnverifiedControl);
                }
                let result = self.read_exact_receipt(&intent, epoch.engine, &request_current)?;
                registry.finish_managed_docker_gc(
                    lease,
                    pending.plan_id,
                    result.status,
                    result.reclaimed_bytes,
                )?;
                Ok(DockerGcDispatchResult::Recovered(result))
            }
        }
    }

    fn read_exact_receipt(
        &mut self,
        intent: &Intent,
        engine: &ControllerEvidence,
        request_current: &impl Fn() -> bool,
    ) -> Result<DockerGcResult, DockerGcError> {
        let namespace = Some(intent.binding.endpoint.as_str());
        if !request_current() || self.engine_namespace(engine) != namespace {
            return Err(DockerGcError::UnverifiedControl);
        }
        let receipt = self.backend.receipt(intent);
        if !request_current() || self.engine_namespace(engine) != namespace {
            return Err(DockerGcError::UnverifiedControl);
        }
        Ok(resolve_receipt(intent, receipt))
    }

    fn engine_namespace(&self, admission: &ControllerEvidence) -> Option<&str> {
        let engine = self.backend.engine()?;
        (engine.admission.endpoint == admission.endpoint
            && engine.admission.generation == admission.generation
            && valid_owner_fingerprint(engine.namespace))
        .then_some(engine.namespace)
    }
}

/// Dispatch one Linux ARM64 image operation from the controller's admitted listener.
/// The peer and protected manifest select the engine and project. The packet cannot
/// choose a registry, arbitrary path, engine socket or policy.
pub fn serve_moby_image_request(policy: &Path, state_dir: &Path, connection: OwnedFd) {
    let _ = with_authorized_write_request(policy, connection, |request, socket| {
        let reply = process_moby_image(state_dir, request)
            .unwrap_or_else(|_| json!({"status":"preserved","reason":"unverified_control"}));
        if let Ok(data) = serde_json::to_vec(&reply) {
            unsafe {
                libc::send(
                    socket.as_raw_fd(),
                    data.as_ptr().cast(),
                    data.len(),
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                );
            }
        }
    });
}

fn image_project_policy(root: &Path, deployment: &str) -> Result<String, DockerGcError> {
    let policy = Policy::load(root, None, None).map_err(|_| DockerGcError::UnverifiedControl)?;
    let encoded = serde_json::to_vec(&policy).map_err(|_| DockerGcError::UnverifiedControl)?;
    let mut hash = Sha256::new();
    hash.update(b"dsg-moby-image-policy-v1\0");
    hash.update(deployment.as_bytes());
    hash.update(encoded);
    Ok(format!("{:x}", hash.finalize()))
}

fn process_moby_image(
    state_dir: &Path,
    request: AuthorizedWriteRequest<'_>,
) -> Result<Value, DockerGcError> {
    let ownership = request
        .pin_backend_ownership()
        .map_err(|_| DockerGcError::UnverifiedControl)?;
    let mut registry = Registry::open_protected_docker_gc(state_dir)?;
    let project = request.request().project_id();
    let recovery = matches!(request.request(), WriteRequest::Recover { .. });
    let root = registry.docker_gc_project_root(project)?;
    let deployment = request.policy_digest().to_owned();
    let project_policy = if recovery {
        String::new()
    } else {
        image_project_policy(&root, &deployment)?
    };
    let policy_current = || {
        recovery || image_project_policy(&root, &deployment).as_ref().ok() == Some(&project_policy)
    };
    let namespace = ownership.namespace();
    let mut engine =
        super::moby::MobyImageBackend::open(request.engine(), namespace, project, || {
            request.connect_engine().ok()
        })
        .ok_or(DockerGcError::UnverifiedControl)?;
    if let WriteRequest::Plan { image_id, .. } = request.request() {
        let owner = registry.docker_gc_project_owner(project)?;
        if !ownership.owns_image(project, &owner, image_id) {
            return Err(DockerGcError::UnverifiedControl);
        }
        let generation = &request.engine().generation;
        let plan = plan_with_controller(
            &mut engine,
            &mut registry,
            project,
            &project_policy,
            DockerGcKind::Image,
            image_id,
            |binding| {
                binding.endpoint == namespace
                    && &binding.generation == generation
                    && request.check_current().is_ok()
                    && ownership.check_current().is_ok()
                    && policy_current()
            },
        )?;
        return Ok(json!({"status":"planned","plan_id":plan.plan_id,"expires_at":plan.expires_at}));
    }
    let lease = registry.acquire_docker_gc_lease(project, namespace, Duration::from_secs(20))?;
    // Connect from the same already pinned kernel peer while dispatch owns the lease.
    let result = DockerGcDispatch::new(&mut engine).dispatch_with_policy(
        &mut registry,
        &lease,
        &project_policy,
        &request,
        policy_current,
        true,
    );
    let outcome = if result.is_ok() {
        AuditOutcome::Success
    } else {
        AuditOutcome::Deny
    };
    registry.finish_lease(lease, outcome, "DOCKER_IMAGE_CONTROLLER")?;
    match result? {
        DockerGcDispatchResult::Approved { intent_id } => {
            Ok(json!({"status":"approved","intent_id":intent_id}))
        }
        DockerGcDispatchResult::Applied(result) | DockerGcDispatchResult::Recovered(result) => {
            let status = match result.status {
                GcApplyStatus::Applied => "applied",
                GcApplyStatus::Preserved => "preserved",
                GcApplyStatus::RecoveryRequired => "recovery_required",
            };
            Ok(json!({"status":status,"reclaimed_bytes":result.reclaimed_bytes}))
        }
    }
}

#[cfg(test)]
mod tests;

fn require_current_intent(
    intent: &Intent,
    policy: &str,
    endpoint: &str,
    engine: &ControllerEvidence,
) -> Result<(), DockerGcError> {
    if intent.policy != policy {
        return Err(RegistryError::GcPolicyChanged.into());
    }
    if intent.binding.endpoint != endpoint || intent.binding.generation != engine.generation {
        return Err(DockerGcError::UnverifiedControl);
    }
    Ok(())
}

struct DispatchEpoch<'a> {
    policy: &'a str,
    controller: &'a ControllerEvidence,
    engine: &'a ControllerEvidence,
    approver: &'a ControllerEvidence,
    executor: &'a ControllerEvidence,
}

impl DispatchEpoch<'_> {
    fn approval_digest(&self, project: i64, plan_id: i64) -> String {
        // The protected approval row is the one-time authorization. This internal digest
        // binds it to the kernel observation without minting a transferable bearer token.
        let mut digest = Sha256::new();
        digest.update(b"dsg-docker-dispatch-approval-v1\0");
        digest.update(project.to_be_bytes());
        digest.update(plan_id.to_be_bytes());
        digest.update(self.policy.as_bytes());
        for evidence in [self.controller, self.engine, self.approver, self.executor] {
            digest.update(evidence.endpoint.as_bytes());
            digest.update(evidence.generation.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}
