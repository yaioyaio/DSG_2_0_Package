use super::{
    managed_docker_identity, matches_current, plan_with_controller, resolve_receipt,
    valid_owner_fingerprint, ControlledDockerGc, DockerGcError, DockerGcKind, DockerGcResult,
    GcApplyStatus, Intent, Lease, Registry, RegistryError, StoredPlan, APPROVAL_TTL,
};
use crate::docker_control_local::{AuthorizedWriteRequest, ControllerEvidence, WriteRequest};
use sha2::{Digest, Sha256};

/// Implemented by the in-process controller that owns the mutation and receipt boundary.
pub(crate) trait ExclusiveDockerGc: ControlledDockerGc {
    /// The admission identity belongs to the pinned engine used by this backend. The namespace
    /// belongs to the underlying resource store and survives new sockets, policies and processes.
    /// This is local controller evidence, available without an engine query during recovery.
    fn engine(&self) -> Option<ExclusiveEngine<'_>>;
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
        registry.check_protected_docker_gc_state()?;
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
            || request.check_current().is_ok(),
        )
    }

    fn dispatch_request(
        &mut self,
        registry: &mut Registry,
        lease: &Lease,
        project_policy_digest: &str,
        request: &WriteRequest,
        epoch: &DispatchEpoch<'_>,
        request_current: impl Fn() -> bool,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        let project = match request {
            WriteRequest::Approve { project_id, .. }
            | WriteRequest::Apply { project_id, .. }
            | WriteRequest::Recover { project_id, .. } => *project_id,
        };
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
            WriteRequest::Approve { plan_id, .. } => {
                let policy = project_policy_digest.to_ascii_lowercase();
                let payload = registry.read_current_gc_plan(project, *plan_id, &policy)?;
                let intent = Intent::load(&payload, project, &owner)?;
                require_current_intent(&intent, &policy, &endpoint, epoch.engine)?;
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
                let verified = matches_current(self.backend, &intent.binding);
                if !request_current()
                    || self.engine_namespace(epoch.engine) != Some(endpoint.as_str())
                {
                    return Err(DockerGcError::UnverifiedControl);
                }
                registry.claim_managed_docker_gc(
                    lease,
                    *plan_id,
                    &policy,
                    &digest,
                    &approved.identity,
                )?;
                let result = if verified
                    && request_current()
                    && self.engine_namespace(epoch.engine) == Some(endpoint.as_str())
                {
                    resolve_receipt(&intent, self.backend.apply_if_current(&intent))
                } else {
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
                if intent.operation != *intent_id || intent.binding.endpoint != endpoint {
                    return Err(DockerGcError::UnverifiedControl);
                }
                if !request_current()
                    || self.engine_namespace(epoch.engine) != Some(endpoint.as_str())
                {
                    return Err(DockerGcError::UnverifiedControl);
                }
                let result = resolve_receipt(&intent, self.backend.receipt(&intent));
                if !request_current()
                    || self.engine_namespace(epoch.engine) != Some(endpoint.as_str())
                {
                    return Err(DockerGcError::UnverifiedControl);
                }
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

    fn engine_namespace(&self, admission: &ControllerEvidence) -> Option<&str> {
        let engine = self.backend.engine()?;
        (engine.admission.endpoint == admission.endpoint
            && engine.admission.generation == admission.generation
            && valid_owner_fingerprint(engine.namespace))
        .then_some(engine.namespace)
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
