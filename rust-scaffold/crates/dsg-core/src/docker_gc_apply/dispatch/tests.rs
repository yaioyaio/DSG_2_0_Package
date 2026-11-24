use super::super::{hash, Binding, Effect, Observation, Receipt, Usage};
use super::*;
use crate::docker_cache::CacheRecordState;
use rusqlite::{params, Connection};
use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

const CACHE: &str = "exclusive-cache-record";

struct Backend {
    engine: ControllerEvidence,
    binding: Binding,
    present: bool,
    retain_receipt: bool,
    receipt: Option<Intent>,
    ledger: PathBuf,
    competing_project: i64,
    observations: usize,
    attempts: usize,
    reads: usize,
}

impl ControlledDockerGc for Backend {
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
        self.observations += 1;
        (self.present
            && self.binding.owner == owner
            && self.binding.id == id
            && self.binding.kind == kind)
            .then(|| Observation {
                binding: self.binding.clone(),
                unreferenced: Some(true),
                usage: Usage::BuildCache(CacheRecordState {
                    reclaimable: true,
                    shared: false,
                    mutable: false,
                }),
            })
    }

    fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt> {
        self.attempts += 1;
        let ledger = Connection::open(&self.ledger).unwrap();
        let claimed = ledger
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM gc_plan AS plan
                   JOIN gc_application AS approval ON approval.plan_id = plan.id
                   JOIN audit_event AS event ON event.plan_id = plan.id
                   WHERE plan.project_id = ?1 AND plan.status = 'applying'
                     AND json_extract(plan.payload_json, '$.managed_docker_target.operation') = ?2
                     AND approval.claimed_at_ms IS NOT NULL
                     AND event.reason_code = 'GC_APPLY_INTENT')",
                params![intent.binding.project, intent.operation],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(claimed);
        let mut competitor = Registry::open(self.ledger.parent().unwrap()).unwrap();
        assert!(matches!(
            competitor.acquire_lease(
                self.competing_project,
                &hash(b"competitor"),
                Duration::from_secs(1),
                |_| false
            ),
            Err(RegistryError::LeaseCapacity)
        ));
        assert_eq!(self.binding, intent.binding);
        assert!(self.present);
        self.present = false;
        self.retain_receipt.then(|| {
            self.receipt = Some(intent.clone());
            Receipt {
                intent: intent.clone(),
                effect: Effect::Applied(23),
            }
        })
    }

    fn receipt(&mut self, _intent: &Intent) -> Option<Receipt> {
        self.reads += 1;
        self.receipt.clone().map(|intent| Receipt {
            intent,
            effect: Effect::Applied(23),
        })
    }
}

impl ExclusiveDockerGc for Backend {
    fn engine(&self) -> Option<ExclusiveEngine<'_>> {
        Some(ExclusiveEngine {
            admission: &self.engine,
            namespace: &self.binding.endpoint,
        })
    }
}

struct Epoch {
    policy: String,
    controller: ControllerEvidence,
    engine: ControllerEvidence,
    approver: ControllerEvidence,
    executor: ControllerEvidence,
}

impl Epoch {
    fn new() -> Self {
        Self {
            policy: hash(b"admission policy"),
            controller: evidence(b"controller"),
            engine: evidence(b"engine"),
            approver: evidence(b"approver"),
            executor: evidence(b"executor"),
        }
    }

    fn view(&self) -> DispatchEpoch<'_> {
        DispatchEpoch {
            policy: &self.policy,
            controller: &self.controller,
            engine: &self.engine,
            approver: &self.approver,
            executor: &self.executor,
        }
    }
}

fn evidence(name: &[u8]) -> ControllerEvidence {
    ControllerEvidence {
        endpoint: hash(name),
        generation: hash(&[name, b" generation"].concat()),
    }
}

struct Fixture {
    _temp: TempDir,
    registry: Registry,
    project: i64,
    policy: String,
    backend: Backend,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let mut registry = Registry::open(&state).unwrap();
        let project = registry.register(temp.path(), "fixture", 1).unwrap().id;
        let competing_root = temp.path().join("competitor");
        std::fs::create_dir(&competing_root).unwrap();
        let competing_project = registry
            .register(&competing_root, "competitor", 1)
            .unwrap()
            .id;
        let engine = evidence(b"engine");
        let binding = Binding {
            project,
            owner: registry.docker_gc_project_owner(project).unwrap(),
            kind: DockerGcKind::BuildCache,
            id: CACHE.into(),
            endpoint: hash(b"stable resource store"),
            generation: engine.generation.clone(),
            snapshot: hash(b"current snapshot"),
        };
        Self {
            _temp: temp,
            registry,
            project,
            policy: hash(b"project policy"),
            backend: Backend {
                engine,
                binding,
                present: true,
                retain_receipt: true,
                receipt: None,
                ledger: state.join("registry.sqlite3"),
                competing_project,
                observations: 0,
                attempts: 0,
                reads: 0,
            },
        }
    }

    fn plan(&mut self) -> i64 {
        plan_with_controller(
            &mut self.backend,
            &mut self.registry,
            self.project,
            &self.policy,
            DockerGcKind::BuildCache,
            CACHE,
            |_| true,
        )
        .unwrap()
        .plan_id
    }

    fn lease(&mut self) -> Lease {
        self.registry
            .acquire_lease(
                self.project,
                &hash(b"scope"),
                Duration::from_secs(30),
                |_| false,
            )
            .unwrap()
    }

    fn dispatch(
        &mut self,
        lease: &Lease,
        request: WriteRequest,
        epoch: &Epoch,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        DockerGcDispatch::new(&mut self.backend).dispatch_request(
            &mut self.registry,
            lease,
            &self.policy,
            &request,
            &epoch.view(),
            || true,
        )
    }

    fn approve(&mut self, lease: &Lease, plan_id: i64, epoch: &Epoch) -> String {
        match self
            .dispatch(
                lease,
                WriteRequest::Approve {
                    project_id: self.project,
                    plan_id,
                },
                epoch,
            )
            .unwrap()
        {
            DockerGcDispatchResult::Approved { intent_id } => intent_id,
            other => panic!("unexpected dispatch response: {other:?}"),
        }
    }

    fn apply(
        &mut self,
        lease: &Lease,
        plan_id: i64,
        epoch: &Epoch,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        self.dispatch(
            lease,
            WriteRequest::Apply {
                project_id: self.project,
                plan_id,
            },
            epoch,
        )
    }

    fn recover(
        &mut self,
        lease: &Lease,
        intent_id: &str,
        epoch: &Epoch,
    ) -> Result<DockerGcDispatchResult, DockerGcError> {
        self.dispatch(
            lease,
            WriteRequest::Recover {
                project_id: self.project,
                intent_id: intent_id.to_owned(),
            },
            epoch,
        )
    }
}

const APPLIED: DockerGcResult = DockerGcResult {
    status: GcApplyStatus::Applied,
    reclaimed_bytes: 23,
};
const UNKNOWN: DockerGcResult = DockerGcResult {
    status: GcApplyStatus::RecoveryRequired,
    reclaimed_bytes: 0,
};

#[test]
fn exclusive_dispatch_consumes_only_the_matching_approval_once() {
    let mut fixture = Fixture::new();
    let plan = fixture.plan();
    let other_plan = fixture.plan();
    let lease = fixture.lease();
    let epoch = Epoch::new();
    fixture.approve(&lease, plan, &epoch);
    let observations = fixture.backend.observations;
    let wrong_project = WriteRequest::Apply {
        project_id: fixture.project + 1,
        plan_id: plan,
    };
    assert!(matches!(
        fixture.dispatch(&lease, wrong_project, &epoch),
        Err(DockerGcError::Registry(RegistryError::InvalidLeaseOwner))
    ));
    assert!(fixture.apply(&lease, other_plan, &epoch).is_err());
    let mut mismatched = Epoch::new();
    mismatched.engine = evidence(b"different engine");
    assert!(matches!(
        fixture.apply(&lease, plan, &mismatched),
        Err(DockerGcError::UnverifiedControl)
    ));
    let rotations: [fn(&mut Epoch); 4] = [
        |epoch: &mut Epoch| epoch.policy = hash(b"new admission policy"),
        |epoch: &mut Epoch| epoch.controller.generation = hash(b"new controller"),
        |epoch: &mut Epoch| epoch.approver.generation = hash(b"new approver"),
        |epoch: &mut Epoch| epoch.executor.generation = hash(b"new executor"),
    ];
    for rotate in rotations {
        let mut mismatch = Epoch::new();
        rotate(&mut mismatch);
        assert!(matches!(
            fixture.apply(&lease, plan, &mismatch),
            Err(DockerGcError::Registry(RegistryError::GcApprovalInvalid))
        ));
    }
    assert_eq!(fixture.backend.observations, observations);
    assert_eq!(fixture.backend.attempts, 0);
    assert_eq!(
        fixture.apply(&lease, plan, &epoch).unwrap(),
        DockerGcDispatchResult::Applied(APPLIED)
    );
    assert!(fixture.apply(&lease, plan, &epoch).is_err());
    assert_eq!(fixture.backend.attempts, 1);
    assert!(!fixture.backend.present);
}

#[test]
fn exclusive_dispatch_recovers_only_an_exact_receipt_across_rotation() {
    let mut fixture = Fixture::new();
    fixture.backend.retain_receipt = false;
    let plan = fixture.plan();
    let competitor = fixture.plan();
    let lease = fixture.lease();
    let initial = Epoch::new();
    let intent_id = fixture.approve(&lease, plan, &initial);
    let payload = fixture
        .registry
        .read_gc_plan(fixture.project, plan)
        .unwrap();
    let saved = Intent::load(&payload, fixture.project, &fixture.backend.binding.owner).unwrap();
    assert_eq!(
        fixture.apply(&lease, plan, &initial).unwrap(),
        DockerGcDispatchResult::Applied(UNKNOWN)
    );
    assert!(fixture.apply(&lease, plan, &initial).is_err());

    let mut rotated = Epoch::new();
    rotated.policy = hash(b"rotated policy");
    rotated.executor = evidence(b"rotated executor");
    rotated.approver = evidence(b"rotated approver");
    // A new observation can exist on an engine after an indeterminate result. It cannot
    // grant a competing approval while SQLite still has an unresolved intent for this target.
    fixture.backend.present = true;
    assert!(matches!(
        fixture.dispatch(
            &lease,
            WriteRequest::Approve {
                project_id: fixture.project,
                plan_id: competitor
            },
            &rotated
        ),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));

    // A new current socket and generation may read the previous intent's receipt through
    // the same backend namespace. An unrelated backend with valid current admission cannot.
    rotated.engine = evidence(b"restarted engine alias");
    fixture.backend.engine = evidence(b"restarted engine alias");
    fixture.backend.binding.generation = rotated.engine.generation.clone();
    let observations = fixture.backend.observations;
    let namespace = std::mem::replace(
        &mut fixture.backend.binding.endpoint,
        hash(b"unrelated resource store"),
    );
    assert!(matches!(
        fixture.recover(&lease, &intent_id, &rotated),
        Err(DockerGcError::UnverifiedControl)
    ));
    fixture.backend.binding.endpoint = namespace;
    assert!(fixture
        .recover(&lease, &hash(b"unrelated operation"), &rotated)
        .is_err());
    assert_eq!(fixture.backend.reads, 0);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(UNKNOWN)
    );
    let mut wrong = saved.clone();
    wrong.binding.snapshot = hash(b"unrelated snapshot");
    fixture.backend.receipt = Some(wrong);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(UNKNOWN)
    );
    fixture.backend.receipt = Some(saved);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(APPLIED)
    );
    assert!(fixture.recover(&lease, &intent_id, &rotated).is_err());
    assert_eq!(fixture.backend.observations, observations);
    assert_eq!(fixture.backend.attempts, 1);
    assert_eq!(fixture.backend.reads, 3);
}

#[test]
fn exclusive_dispatch_revalidates_before_unchanged_authority_can_mutate() {
    let mut fixture = Fixture::new();
    let plan = fixture.plan();
    let lease = fixture.lease();
    let epoch = Epoch::new();
    fixture.approve(&lease, plan, &epoch);
    let validations = Cell::new(0);
    let result = DockerGcDispatch::new(&mut fixture.backend)
        .dispatch_request(
            &mut fixture.registry,
            &lease,
            &fixture.policy,
            &WriteRequest::Apply {
                project_id: fixture.project,
                plan_id: plan,
            },
            &epoch.view(),
            || {
                let count = validations.get() + 1;
                validations.set(count);
                count < 3
            },
        )
        .unwrap();
    assert_eq!(validations.get(), 3);
    assert_eq!(
        result,
        DockerGcDispatchResult::Applied(DockerGcResult {
            status: GcApplyStatus::Preserved,
            reclaimed_bytes: 0
        })
    );
    assert!(fixture.apply(&lease, plan, &epoch).is_err());
    assert_eq!(fixture.backend.attempts, 0);
    assert!(fixture.backend.present);
}
