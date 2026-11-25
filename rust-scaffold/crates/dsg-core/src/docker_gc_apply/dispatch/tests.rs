use super::super::{hash, Binding, Effect, Observation, Receipt, Usage};
use super::*;
use crate::docker_cache::CacheRecordState;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

const CACHE: &str = "exclusive-cache-record";

struct Backend {
    engine: ControllerEvidence,
    binding: Binding,
    native: Connection,
    retain_receipt: bool,
    acknowledge: bool,
    ledger: PathBuf,
    competing_project: i64,
    observations: usize,
    attempts: usize,
    reads: usize,
}

fn read_observation(
    conn: &Connection,
    owner: &str,
    kind: DockerGcKind,
    id: &str,
) -> Option<Observation> {
    let json: String = conn
        .query_row("SELECT binding FROM target WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()?;
    let binding: Binding = serde_json::from_str(&json).unwrap();
    (binding.owner == owner && binding.id == id && binding.kind == kind).then_some(Observation {
        binding,
        unreferenced: Some(true),
        usage: Usage::BuildCache(CacheRecordState {
            reclaimable: true,
            shared: false,
            mutable: false,
        }),
    })
}

impl Backend {
    fn seed(&self, binding: &Binding) {
        self.native
            .execute(
                "INSERT OR REPLACE INTO target(id, binding) VALUES (?1, ?2)",
                params![binding.id, serde_json::to_string(binding).unwrap()],
            )
            .unwrap();
    }

    fn present(&self, id: &str) -> bool {
        self.native
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM target WHERE id = ?1)",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn replace_receipt(&self, value: &Intent) {
        self.native
            .execute(
                "INSERT OR REPLACE INTO receipt(operation, intent) VALUES (?1, ?2)",
                params![value.operation, serde_json::to_string(value).unwrap()],
            )
            .unwrap();
    }
}

impl ControlledDockerGc for Backend {
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
        self.observations += 1;
        read_observation(&self.native, owner, kind, id)
    }

    fn apply_if_current(&mut self, _intent: &Intent) -> Option<Receipt> {
        panic!("exclusive dispatch may only commit a prepared native transaction")
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        self.reads += 1;
        let json: String = self
            .native
            .query_row(
                "SELECT intent FROM receipt WHERE operation = ?1",
                [&intent.operation],
                |row| row.get(0),
            )
            .optional()
            .unwrap()?;
        Some(Receipt {
            intent: serde_json::from_str(&json).unwrap(),
            effect: Effect::Applied(23),
        })
    }
}

struct Prepared<'a> {
    tx: Transaction<'a>,
    observation: Observation,
    ledger: &'a std::path::Path,
    competing_project: i64,
    attempts: &'a mut usize,
    retain_receipt: bool,
    acknowledge: bool,
}

impl PreparedDockerGc for Prepared<'_> {
    fn observed(&self) -> &Observation {
        &self.observation
    }

    fn commit(self: Box<Self>, claim: ClaimedDockerGc<'_>) -> bool {
        let intent = claim.intent();
        *self.attempts += 1;
        let ledger = Connection::open(self.ledger).unwrap();
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
        assert_eq!(self.observation.binding, intent.binding);
        let changed = self
            .tx
            .execute(
                "DELETE FROM target WHERE id = ?1 AND binding = ?2",
                params![
                    intent.binding.id,
                    serde_json::to_string(&intent.binding).unwrap()
                ],
            )
            .unwrap();
        assert_eq!(changed, 1);
        if self.retain_receipt {
            self.tx
                .execute(
                    "INSERT INTO receipt(operation,intent) VALUES (?1,?2)",
                    params![intent.operation, serde_json::to_string(intent).unwrap()],
                )
                .unwrap();
        }
        self.tx.commit().unwrap();
        self.acknowledge
    }
}

impl ExclusiveDockerGc for Backend {
    fn engine(&self) -> Option<ExclusiveEngine<'_>> {
        Some(ExclusiveEngine {
            admission: &self.engine,
            namespace: &self.binding.endpoint,
        })
    }

    fn prepare(&mut self, intent: &Intent) -> Option<Box<dyn PreparedDockerGc + '_>> {
        self.observations += 1;
        let tx = self
            .native
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let binding = &intent.binding;
        let observation = read_observation(&tx, &binding.owner, binding.kind, &binding.id)?;
        Some(Box::new(Prepared {
            tx,
            observation,
            ledger: &self.ledger,
            competing_project: self.competing_project,
            attempts: &mut self.attempts,
            retain_receipt: self.retain_receipt,
            acknowledge: self.acknowledge,
        }))
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
        let native = Connection::open(state.join("native-fixture.sqlite3")).unwrap();
        native
            .execute_batch(
                "PRAGMA synchronous=FULL;
            CREATE TABLE target(id TEXT PRIMARY KEY, binding TEXT NOT NULL);
            CREATE TABLE receipt(operation TEXT PRIMARY KEY, intent TEXT NOT NULL);",
            )
            .unwrap();
        let fixture = Self {
            _temp: temp,
            registry,
            project,
            policy: hash(b"project policy"),
            backend: Backend {
                engine,
                binding,
                native,
                retain_receipt: true,
                acknowledge: true,
                ledger: state.join("registry.sqlite3"),
                competing_project,
                observations: 0,
                attempts: 0,
                reads: 0,
            },
        };
        fixture.backend.seed(&fixture.backend.binding);
        fixture
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
            |_| true,
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
    assert!(!fixture.backend.present(CACHE));
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
    fixture.backend.seed(&fixture.backend.binding);
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
    assert_eq!(fixture.backend.reads, 1);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(UNKNOWN)
    );
    let mut wrong = saved.clone();
    wrong.binding.snapshot = hash(b"unrelated snapshot");
    fixture.backend.replace_receipt(&wrong);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(UNKNOWN)
    );
    fixture.backend.replace_receipt(&saved);
    assert_eq!(
        fixture.recover(&lease, &intent_id, &rotated).unwrap(),
        DockerGcDispatchResult::Recovered(APPLIED)
    );
    assert!(fixture.recover(&lease, &intent_id, &rotated).is_err());
    assert_eq!(fixture.backend.observations, observations);
    assert_eq!(fixture.backend.attempts, 1);
    assert_eq!(fixture.backend.reads, 4);
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
            |_| true,
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
    assert!(fixture.backend.present(CACHE));
}

#[test]
fn exclusive_dispatch_rejects_stale_assignment_and_locked_snapshot() {
    let mut fixture = Fixture::new();
    let plan = fixture.plan();
    let lease = fixture.lease();
    let epoch = Epoch::new();
    fixture.approve(&lease, plan, &epoch);
    let request = WriteRequest::Apply {
        project_id: fixture.project,
        plan_id: plan,
    };
    for (store, id) in [
        (hash(b"other store"), CACHE),
        (fixture.backend.binding.endpoint.clone(), "another-record"),
    ] {
        let result = DockerGcDispatch::new(&mut fixture.backend).dispatch_request(
            &mut fixture.registry,
            &lease,
            &fixture.policy,
            &request,
            &epoch.view(),
            || true,
            |binding| binding.endpoint == store && binding.id == id,
        );
        assert!(matches!(result, Err(DockerGcError::UnverifiedControl)));
    }
    let mut stale = fixture.backend.binding.clone();
    stale.snapshot = hash(b"snapshot changed after plan");
    fixture.backend.seed(&stale);
    assert!(matches!(
        fixture.apply(&lease, plan, &epoch),
        Err(DockerGcError::UnverifiedCandidate)
    ));
    let ledger = Connection::open(&fixture.backend.ledger).unwrap();
    let claimed: bool = ledger
        .query_row(
            "SELECT claimed_at_ms IS NOT NULL FROM gc_application WHERE plan_id=?1",
            [plan],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!claimed);
    assert!(fixture.backend.present(CACHE));
    assert_eq!(fixture.backend.attempts, 0);
}

#[test]
fn exclusive_dispatch_recovers_durable_receipt_after_lost_commit_response() {
    let mut fixture = Fixture::new();
    let plan = fixture.plan();
    let lease = fixture.lease();
    let epoch = Epoch::new();
    let intent_id = fixture.approve(&lease, plan, &epoch);
    let mut other = fixture.backend.binding.clone();
    other.id = "unrelated-exact-record".into();
    fixture.backend.seed(&other);
    fixture.backend.acknowledge = false;

    let path = fixture
        .backend
        .ledger
        .with_file_name("native-fixture.sqlite3");
    let contender = Connection::open(&path).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();
    let ledger = Connection::open(&fixture.backend.ledger).unwrap();
    let validations = Cell::new(0);
    let request = WriteRequest::Apply {
        project_id: fixture.project,
        plan_id: plan,
    };
    let result = DockerGcDispatch::new(&mut fixture.backend)
        .dispatch_request(
            &mut fixture.registry,
            &lease,
            &fixture.policy,
            &request,
            &epoch.view(),
            || {
                let step = validations.get() + 1;
                validations.set(step);
                if step >= 2 {
                    let error = contender.execute_batch("BEGIN IMMEDIATE").unwrap_err();
                    assert_eq!(
                        error.sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy)
                    );
                    let claimed: bool = ledger
                        .query_row(
                            "SELECT claimed_at_ms IS NOT NULL FROM gc_application WHERE plan_id=?1",
                            [plan],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(claimed, step == 3);
                }
                true
            },
            |_| true,
        )
        .unwrap();
    assert_eq!(validations.get(), 3);
    assert_eq!(result, DockerGcDispatchResult::Applied(UNKNOWN));
    fixture.backend.native = Connection::open(&path).unwrap();
    assert!(!fixture.backend.present(CACHE));
    let untouched =
        read_observation(&fixture.backend.native, &other.owner, other.kind, &other.id).unwrap();
    assert_eq!(untouched.binding, other);
    assert!(fixture.apply(&lease, plan, &epoch).is_err());
    assert_eq!(
        fixture.recover(&lease, &intent_id, &epoch).unwrap(),
        DockerGcDispatchResult::Recovered(APPLIED)
    );
    assert!(fixture.recover(&lease, &intent_id, &epoch).is_err());
    assert_eq!(fixture.backend.attempts, 1);
    assert_eq!(fixture.backend.reads, 1);
}
