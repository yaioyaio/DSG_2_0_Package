use super::*;
use crate::docker_image_gc::{ImageGcDisposition, ImageGcReason};
use crate::registry::AuditOutcome;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const POLICY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_POLICY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CACHE: &str = "fixture-cache-record";

fn target_id(kind: DockerGcKind) -> &'static str {
    match kind {
        DockerGcKind::Image => IMAGE,
        DockerGcKind::BuildCache => CACHE,
    }
}

struct TestBackend {
    state: Connection,
    registry_path: PathBuf,
    observation: Observation,
    change_after_preflight: bool,
    interrupt_after_commit: bool,
    lose_receipt: bool,
    attempts: usize,
    competing_project: Option<i64>,
}

impl TestBackend {
    fn new(
        state: &Path,
        registry_path: PathBuf,
        project: i64,
        owner: String,
        kind: DockerGcKind,
    ) -> Self {
        let state = Connection::open(state).unwrap();
        state
            .execute_batch(
                "CREATE TABLE resources (id TEXT PRIMARY KEY);
             CREATE TABLE receipts (operation TEXT PRIMARY KEY, intent TEXT, bytes INTEGER);",
            )
            .unwrap();
        for id in [IMAGE, CACHE, "untouched"] {
            state
                .execute("INSERT INTO resources VALUES (?1)", [id])
                .unwrap();
        }
        Self::with_connection(state, registry_path, project, owner, kind)
    }

    fn with_connection(
        state: Connection,
        registry_path: PathBuf,
        project: i64,
        owner: String,
        kind: DockerGcKind,
    ) -> Self {
        let binding = Binding {
            project,
            owner,
            kind,
            id: target_id(kind).into(),
            endpoint: hash(b"fixed endpoint"),
            generation: hash(b"fixed generation"),
            snapshot: hash(b"fixed observation"),
        };
        let usage = match kind {
            DockerGcKind::Image => Usage::Image(ImageGcPlan {
                context: "fixture".into(),
                engine_id: Some("fixture-engine".into()),
                image_id: IMAGE.into(),
                disposition: ImageGcDisposition::Candidate,
                reason: ImageGcReason::Eligible,
                fingerprint: Some(binding.snapshot.clone()),
                apply_gate: DockerApplyGate::PreserveUnverifiedControl,
            }),
            DockerGcKind::BuildCache => Usage::BuildCache(CacheRecordState {
                reclaimable: true,
                shared: false,
                mutable: false,
            }),
        };
        Self {
            state,
            registry_path,
            observation: Observation {
                binding,
                unreferenced: Some(true),
                usage,
            },
            change_after_preflight: false,
            interrupt_after_commit: false,
            lose_receipt: false,
            attempts: 0,
            competing_project: None,
        }
    }

    fn service(&mut self) -> DockerGcService<'_> {
        DockerGcService {
            controller: Some(self),
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.state
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }
}

impl ControlledDockerGc for TestBackend {
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
        if !self.contains(id)
            || self.observation.binding.kind != kind
            || self.observation.binding.owner != owner
        {
            return None;
        }
        let usage = match &self.observation.usage {
            Usage::Image(plan) => Usage::Image(plan.clone()),
            Usage::BuildCache(state) => Usage::BuildCache(*state),
        };
        Some(Observation {
            binding: self.observation.binding.clone(),
            unreferenced: self.observation.unreferenced,
            usage,
        })
    }

    fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt> {
        self.attempts += 1;
        let controller = Connection::open(&self.registry_path).unwrap();
        let recorded: bool = controller.query_row(
            "SELECT EXISTS(SELECT 1 FROM audit_event e JOIN gc_plan p ON p.id = e.plan_id
             WHERE reason_code = 'GC_APPLY_INTENT' AND p.status = 'applying' AND p.payload_json = ?1)",
            [json!({ (MANAGED_DOCKER_TARGET): intent }).to_string()],
            |row| row.get(0),
        ).unwrap();
        assert!(
            recorded,
            "a separate connection must see durable intent before mutation"
        );
        if let Some(project) = self.competing_project {
            let mut registry = Registry::open(self.registry_path.parent().unwrap()).unwrap();
            assert!(matches!(
                registry.acquire_lease(project, OTHER_POLICY, Duration::from_secs(30), |_| false),
                Err(RegistryError::LeaseCapacity)
            ));
        }
        let tx = self
            .state
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        if self.change_after_preflight {
            self.observation.unreferenced = Some(false);
        }
        let binding = &intent.binding;
        let bytes = if self.observation.binding == *binding
            && self.observation.eligible_for(
                binding.project,
                &binding.owner,
                binding.kind,
                &binding.id,
            )
            && tx
                .execute("DELETE FROM resources WHERE id = ?1", [&binding.id])
                .unwrap()
                == 1
        {
            Some(17i64)
        } else {
            None
        };
        if !self.lose_receipt {
            tx.execute(
                "INSERT INTO receipts VALUES (?1, ?2, ?3)",
                params![
                    intent.operation,
                    serde_json::to_string(intent).unwrap(),
                    bytes
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        assert!(
            !self.interrupt_after_commit,
            "fixture stopped after the change and before completion"
        );
        self.receipt(intent)
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        self.state
            .query_row(
                "SELECT intent, bytes FROM receipts WHERE operation = ?1",
                [&intent.operation],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .unwrap()
            .and_then(|(saved_intent, bytes)| {
                Some(Receipt {
                    intent: serde_json::from_str(&saved_intent).ok()?,
                    effect: match bytes {
                        Some(bytes) => Effect::Applied(u64::try_from(bytes).ok()?),
                        None => Effect::Preserved,
                    },
                })
            })
    }
}

struct Scenario {
    temp: TempDir,
    registry: Registry,
    project: i64,
    backend: TestBackend,
}

impl Scenario {
    fn new(kind: DockerGcKind) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut registry = Registry::open(&temp.path().join("state")).unwrap();
        let project = registry.register(temp.path(), "fixture", 1).unwrap().id;
        let backend = TestBackend::new(
            &temp.path().join("resources.sqlite3"),
            temp.path().join("state/registry.sqlite3"),
            project,
            registry.docker_gc_project_owner(project).unwrap(),
            kind,
        );
        Self {
            temp,
            registry,
            project,
            backend,
        }
    }

    fn plan(&mut self) -> i64 {
        self.try_plan().unwrap().plan_id
    }

    fn try_plan(&mut self) -> Result<StoredPlan, DockerGcError> {
        let binding = self.backend.observation.binding.clone();
        self.backend.service().plan(
            &mut self.registry,
            self.project,
            POLICY,
            binding.kind,
            &binding.id,
        )
    }

    fn approve(&mut self, plan: i64) -> String {
        self.backend
            .service()
            .approve(&mut self.registry, self.project, plan, POLICY)
            .unwrap()
    }

    fn lease(&mut self) -> Lease {
        self.registry
            .acquire_lease(self.project, POLICY, Duration::from_secs(30), |_| false)
            .unwrap()
    }

    fn apply(
        &mut self,
        lease: &Lease,
        plan: i64,
        approval: &str,
    ) -> Result<DockerGcResult, DockerGcError> {
        self.backend
            .service()
            .apply(&mut self.registry, lease, plan, POLICY, approval)
    }

    fn recover(&mut self, lease: &Lease, plan: i64) -> Result<DockerGcResult, DockerGcError> {
        self.backend
            .service()
            .recover(&mut self.registry, lease, plan)
    }

    fn ledger(&self) -> Connection {
        Connection::open(&self.backend.registry_path).unwrap()
    }

    fn audit(&self, plan: i64) -> Vec<String> {
        self.ledger()
            .prepare("SELECT reason_code FROM audit_event WHERE plan_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([plan], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
}

#[test]
fn both_kinds_apply_one_target_and_refuse_replay_or_file_recovery() {
    for kind in [DockerGcKind::Image, DockerGcKind::BuildCache] {
        let mut scenario = Scenario::new(kind);
        let plan = scenario.plan();
        let approval = scenario.approve(plan);
        let lease = scenario.lease();
        assert!(matches!(
            scenario
                .registry
                .claim_gc(&lease, plan, POLICY, &hash(approval.as_bytes())),
            Err(RegistryError::GcCandidateMismatch)
        ));
        assert_eq!(
            scenario.apply(&lease, plan, &approval).unwrap(),
            DockerGcResult {
                status: GcApplyStatus::Applied,
                reclaimed_bytes: 17,
            }
        );
        assert!(!scenario.backend.contains(target_id(kind)));
        let sibling = if kind == DockerGcKind::Image {
            CACHE
        } else {
            IMAGE
        };
        assert!(scenario.backend.contains(sibling));
        assert!(scenario.backend.contains("untouched"));
        assert!(scenario.apply(&lease, plan, &approval).is_err());
        assert!(scenario.recover(&lease, plan).is_err());
        assert_eq!(scenario.backend.attempts, 1);
        assert_eq!(
            scenario.audit(plan),
            ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT", "GC_APPLIED"]
        );
    }
}

#[test]
fn external_inputs_and_crossed_plan_scopes_cannot_authorize_mutation() {
    let mut scenario = Scenario::new(DockerGcKind::Image);
    let mut external = DockerGcService::external();
    assert_eq!(
        external.apply_gate(),
        DockerApplyGate::PreserveUnverifiedControl
    );
    assert!(matches!(
        external.plan(
            &mut scenario.registry,
            scenario.project,
            POLICY,
            DockerGcKind::Image,
            IMAGE
        ),
        Err(DockerGcError::UnverifiedControl)
    ));
    let external_plan = scenario.registry.save_plan(scenario.project, POLICY, &json!({
        "dry_run": true, "candidates": [], "docker_images": {"image_id": IMAGE, "disposition": "candidate"}
    }), PLAN_TTL).unwrap().plan_id;
    assert!(matches!(
        scenario.backend.service().approve(
            &mut scenario.registry,
            scenario.project,
            external_plan,
            POLICY
        ),
        Err(DockerGcError::Registry(RegistryError::GcCandidateMismatch))
    ));
    let plan = scenario.plan();
    let file_id = hash(b"fixture file identity");
    scenario
        .registry
        .register_gc_output(scenario.project, "build", &file_id)
        .unwrap();
    assert!(matches!(
        scenario.registry.approve_gc(
            scenario.project,
            plan,
            "build",
            POLICY,
            &file_id,
            POLICY,
            APPROVAL_TTL
        ),
        Err(RegistryError::GcCandidateMismatch)
    ));
    assert!(matches!(
        external.approve(&mut scenario.registry, scenario.project, plan, POLICY),
        Err(DockerGcError::UnverifiedControl)
    ));
    assert_eq!(scenario.backend.attempts, 0);
}

#[test]
fn project_id_collisions_across_registries_cannot_reuse_a_managed_plan() {
    let mut owner = Scenario::new(DockerGcKind::Image);
    let plan = owner.plan();
    let payload = owner.registry.read_gc_plan(owner.project, plan).unwrap();
    let mut other = Scenario::new(DockerGcKind::Image);
    assert_eq!(owner.project, other.project);
    assert_ne!(
        owner.backend.observation.binding.owner,
        other.backend.observation.binding.owner
    );
    let copied_plan = other
        .registry
        .save_plan(other.project, POLICY, &payload, PLAN_TTL)
        .unwrap()
        .plan_id;
    assert!(matches!(
        owner.backend.service().plan(
            &mut other.registry,
            other.project,
            POLICY,
            DockerGcKind::Image,
            IMAGE
        ),
        Err(DockerGcError::UnverifiedCandidate)
    ));
    assert!(matches!(
        owner
            .backend
            .service()
            .approve(&mut other.registry, other.project, copied_plan, POLICY),
        Err(DockerGcError::UnverifiedCandidate)
    ));
    assert_eq!(owner.backend.attempts, 0);
}

#[test]
fn a_concurrent_registry_cannot_take_the_other_slot_during_change() {
    let mut scenario = Scenario::new(DockerGcKind::Image);
    let plan = scenario.plan();
    let approval = scenario.approve(plan);
    let other_root = scenario.temp.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let other = scenario
        .registry
        .register(&other_root, "other", 1)
        .unwrap()
        .id;
    scenario.backend.competing_project = Some(other);
    let lease = scenario.lease();
    let other_lease = scenario
        .registry
        .acquire_lease(other, OTHER_POLICY, Duration::from_secs(30), |_| false)
        .unwrap();
    assert!(matches!(
        scenario.apply(&lease, plan, &approval),
        Err(DockerGcError::Registry(RegistryError::LeaseConflict))
    ));
    assert_eq!(scenario.backend.attempts, 0);
    scenario
        .registry
        .finish_lease(other_lease, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .unwrap();
    assert_eq!(
        scenario.apply(&lease, plan, &approval).unwrap().status,
        GcApplyStatus::Applied
    );
    assert_eq!(scenario.backend.attempts, 1);
    let next = scenario
        .registry
        .acquire_lease(other, OTHER_POLICY, Duration::from_secs(30), |_| false)
        .unwrap();
    scenario
        .registry
        .finish_lease(next, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .unwrap();
}

#[test]
fn expiry_wrong_policy_project_and_approval_are_rejected_before_mutation() {
    let mut scenario = Scenario::new(DockerGcKind::Image);
    let plan = scenario.plan();
    let approval = scenario.approve(plan);
    let other_root = scenario.temp.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let other = scenario
        .registry
        .register(&other_root, "other", 1)
        .unwrap()
        .id;
    let other_lease = scenario
        .registry
        .acquire_lease(other, OTHER_POLICY, Duration::from_secs(30), |_| false)
        .unwrap();
    assert!(matches!(
        scenario.apply(&other_lease, plan, &approval),
        Err(DockerGcError::Registry(RegistryError::GcPlanNotFound))
    ));
    scenario
        .registry
        .finish_lease(other_lease, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .unwrap();
    let lease = scenario.lease();
    assert!(matches!(
        scenario.backend.service().apply(
            &mut scenario.registry,
            &lease,
            plan,
            OTHER_POLICY,
            &approval
        ),
        Err(DockerGcError::Registry(RegistryError::GcPolicyChanged))
    ));
    assert!(matches!(
        scenario.apply(&lease, plan, POLICY),
        Err(DockerGcError::Registry(RegistryError::GcApprovalInvalid))
    ));
    scenario
        .ledger()
        .execute(
            "UPDATE gc_application SET approved_at_ms = 1, expires_at_ms = 2 WHERE plan_id = ?1",
            [plan],
        )
        .unwrap();
    assert!(matches!(
        scenario.apply(&lease, plan, &approval),
        Err(DockerGcError::Registry(RegistryError::GcExpired))
    ));
    assert_eq!(scenario.backend.attempts, 0);
    assert_eq!(scenario.audit(plan), ["PLAN_SAVED", "GC_APPROVED"]);
}

#[test]
fn preserves_changed_endpoint_generation_snapshot_references_and_usage() {
    for change in [
        "endpoint",
        "generation",
        "snapshot",
        "reference",
        "unknown",
        "active",
        "shared",
        "mutable",
        "raced_reference",
    ] {
        let mut scenario = Scenario::new(DockerGcKind::BuildCache);
        let plan = scenario.plan();
        let approval = scenario.approve(plan);
        let observation = &mut scenario.backend.observation;
        match change {
            "endpoint" => observation.binding.endpoint = OTHER_POLICY.into(),
            "generation" => observation.binding.generation = OTHER_POLICY.into(),
            "snapshot" => observation.binding.snapshot = OTHER_POLICY.into(),
            "reference" => observation.unreferenced = Some(false),
            "unknown" => observation.unreferenced = None,
            "raced_reference" => scenario.backend.change_after_preflight = true,
            _ => {
                observation.usage = Usage::BuildCache(CacheRecordState {
                    reclaimable: change != "active",
                    shared: change == "shared",
                    mutable: change == "mutable",
                })
            }
        }
        let lease = scenario.lease();
        assert_eq!(
            scenario.apply(&lease, plan, &approval).unwrap(),
            DockerGcResult {
                status: GcApplyStatus::Preserved,
                reclaimed_bytes: 0,
            }
        );
        assert!(scenario.backend.contains(CACHE));
        assert!(scenario.apply(&lease, plan, &approval).is_err());
        assert_eq!(
            scenario.backend.attempts,
            usize::from(change == "raced_reference")
        );
        assert_eq!(
            scenario.audit(plan),
            [
                "PLAN_SAVED",
                "GC_APPROVED",
                "GC_APPLY_INTENT",
                "GC_PRESERVED"
            ]
        );
    }
}

#[test]
fn interruption_recovers_from_bound_receipt_and_missing_receipt_stays_unknown() {
    for (kind, lose_receipt) in [
        (DockerGcKind::Image, false),
        (DockerGcKind::BuildCache, true),
    ] {
        let mut scenario = Scenario::new(kind);
        let plan = scenario.plan();
        let approval = scenario.approve(plan);
        let lease = scenario.lease();
        scenario.backend.interrupt_after_commit = true;
        scenario.backend.lose_receipt = lose_receipt;
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scenario.apply(&lease, plan, &approval)
        }));
        assert!(stopped.is_err());
        assert!(!scenario.backend.contains(target_id(kind)));
        assert_eq!(
            scenario.audit(plan),
            ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT"]
        );
        scenario.registry = Registry::open(&scenario.temp.path().join("state")).unwrap();
        scenario.backend.state =
            Connection::open(scenario.temp.path().join("resources.sqlite3")).unwrap();
        assert!(matches!(
            scenario.registry.pending_gc(&lease, plan),
            Err(RegistryError::GcCandidateMismatch)
        ));
        assert!(matches!(
            scenario
                .registry
                .finish_gc(&lease, plan, GcApplyStatus::Preserved, 0),
            Err(RegistryError::GcCandidateMismatch)
        ));
        let recovered = scenario.recover(&lease, plan).unwrap();
        let status = if lose_receipt {
            GcApplyStatus::RecoveryRequired
        } else {
            GcApplyStatus::Applied
        };
        assert_eq!(recovered.status, status);
        assert_eq!(recovered.reclaimed_bytes, if lose_receipt { 0 } else { 17 });
        assert!(scenario.apply(&lease, plan, &approval).is_err());
        assert_eq!(scenario.backend.attempts, 1);
        let last = if lose_receipt {
            "GC_RECOVERY_REQUIRED"
        } else {
            "GC_APPLIED"
        };
        assert_eq!(
            scenario.audit(plan),
            ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT", last]
        );
    }
}

#[cfg(unix)]
#[test]
fn abrupt_worker_exit_reopens_durable_intent_and_reclaims_lease() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::Instant;

    const WORKER: &str = "__dsg_mock_docker_crash_worker";
    const NO_RECEIPT: &str = "__dsg_mock_docker_no_receipt";
    const LEASE_TTL: Duration = Duration::from_secs(1);

    struct StalledTestController(TestBackend);

    impl ControlledDockerGc for StalledTestController {
        fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
            self.0.observe(owner, kind, id)
        }

        fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt> {
            self.0.apply_if_current(intent);
            thread::sleep(Duration::from_secs(12));
            panic!("parent did not stop the worker after its mock transaction");
        }

        fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
            self.0.receipt(intent)
        }
    }

    if std::env::args_os().any(|arg| arg == WORKER) {
        let project = serde_json::from_reader(std::io::stdin().lock()).unwrap();
        let mut registry = Registry::open(Path::new("state")).unwrap();
        let backend = TestBackend::with_connection(
            Connection::open("resources.sqlite3").unwrap(),
            "state/registry.sqlite3".into(),
            project,
            registry.docker_gc_project_owner(project).unwrap(),
            DockerGcKind::BuildCache,
        );
        let mut controller = StalledTestController(backend);
        controller.0.lose_receipt = std::env::args_os().any(|arg| arg == NO_RECEIPT);
        let mut service = DockerGcService {
            controller: Some(&mut controller),
        };
        let plan = service
            .plan(
                &mut registry,
                project,
                POLICY,
                DockerGcKind::BuildCache,
                CACHE,
            )
            .unwrap()
            .plan_id;
        let approval = service
            .approve(&mut registry, project, plan, POLICY)
            .unwrap();
        let lease = registry
            .acquire_lease(project, POLICY, LEASE_TTL, |_| false)
            .unwrap();
        registry.attach_lease_owner(&lease).unwrap();
        service
            .apply(&mut registry, &lease, plan, POLICY, &approval)
            .unwrap();
        panic!("worker did not stop between dispatch and settlement");
    }

    struct OwnedWorker(Child);

    impl Drop for OwnedWorker {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    let test = concat!(
        module_path!(),
        "::abrupt_worker_exit_reopens_durable_intent_and_reclaims_lease"
    );
    let test = test.split_once("::").unwrap().1;
    for lose_receipt in [false, true] {
        let mut scenario = Scenario::new(DockerGcKind::BuildCache);
        let competing = scenario.plan();
        let competing_approval = scenario.approve(competing);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", test, "--skip", WORKER]);
        if lose_receipt {
            command.args(["--skip", NO_RECEIPT]);
        }
        let mut worker = OwnedWorker(
            command
                .current_dir(scenario.temp.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let worker_pid = worker.0.id();
        serde_json::to_writer(worker.0.stdin.take().unwrap(), &scenario.project).unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        while scenario.backend.contains(CACHE) {
            assert!(
                worker.0.try_wait().unwrap().is_none(),
                "worker exited before mock dispatch"
            );
            assert!(Instant::now() < deadline, "mock dispatch deadline expired");
            thread::sleep(Duration::from_millis(10));
        }
        worker.0.kill().unwrap();
        assert_eq!(worker.0.wait().unwrap().signal(), Some(libc::SIGKILL));

        scenario.registry = Registry::open(&scenario.temp.path().join("state")).unwrap();
        scenario.backend.state =
            Connection::open(scenario.temp.path().join("resources.sqlite3")).unwrap();
        let plan = scenario
            .ledger()
            .query_row(
                "SELECT id FROM gc_plan WHERE status = 'applying'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            scenario.audit(plan),
            ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT"]
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let lease = loop {
            match scenario
                .registry
                .acquire_lease(scenario.project, POLICY, LEASE_TTL, |pid| pid == worker_pid)
            {
                Ok(lease) => break lease,
                Err(RegistryError::LeaseConflict) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("bounded worker lease recovery failed: {error}"),
            }
        };
        assert!(matches!(
            scenario.apply(&lease, competing, &competing_approval),
            Err(DockerGcError::Registry(
                RegistryError::DockerGcRecoveryPending
            ))
        ));
        let result = scenario.recover(&lease, plan).unwrap();
        let (status, bytes, event) = if lose_receipt {
            (GcApplyStatus::RecoveryRequired, 0, "GC_RECOVERY_REQUIRED")
        } else {
            (GcApplyStatus::Applied, 17, "GC_APPLIED")
        };
        assert_eq!(
            result,
            DockerGcResult {
                status,
                reclaimed_bytes: bytes
            }
        );
        assert_eq!(
            scenario.audit(plan),
            ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT", event]
        );
        assert_eq!(scenario.backend.attempts, 0);
        assert!(scenario.backend.contains("untouched"));
        scenario
            .registry
            .finish_lease(lease, AuditOutcome::Success, "FIXTURE_WORKER_CLOSED")
            .unwrap();
        let ledger = scenario.ledger();
        let reclaimed: i64 = ledger
            .query_row(
                "SELECT count(*) FROM audit_event WHERE reason_code = 'LEASE_RECLAIMED'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let integrity: String = ledger
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!((reclaimed, integrity.as_str()), (1, "ok"));
        assert!(!scenario.registry.has_open_execution_lease().unwrap());
    }
}

#[test]
fn unresolved_intent_blocks_another_plan_until_receipt_settles_it() {
    let mut scenario = Scenario::new(DockerGcKind::BuildCache);
    let pending = scenario.plan();
    let next = scenario.plan();
    let awaiting_approval = scenario.plan();
    let approval = scenario.approve(pending);
    let next_approval = scenario.approve(next);
    let lease = scenario.lease();
    scenario.backend.change_after_preflight = true;
    scenario.backend.interrupt_after_commit = true;
    scenario.backend.lose_receipt = true;
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scenario.apply(&lease, pending, &approval)
    }))
    .is_err());
    assert!(scenario.backend.contains(CACHE));
    scenario.backend.change_after_preflight = false;
    scenario.backend.interrupt_after_commit = false;
    scenario.backend.observation.unreferenced = Some(true);

    assert!(matches!(
        scenario.apply(&lease, next, &next_approval),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));
    assert_eq!(
        scenario.recover(&lease, pending).unwrap().status,
        GcApplyStatus::RecoveryRequired
    );
    assert!(matches!(
        scenario.apply(&lease, next, &next_approval),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));
    assert_eq!(scenario.audit(next), ["PLAN_SAVED", "GC_APPROVED"]);
    assert_eq!(scenario.backend.attempts, 1);
    scenario
        .registry
        .finish_lease(lease, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .unwrap();
    scenario.registry = Registry::open(&scenario.temp.path().join("state")).unwrap();

    assert!(matches!(
        scenario.backend.service().approve(
            &mut scenario.registry,
            scenario.project,
            awaiting_approval,
            POLICY,
        ),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));
    assert_eq!(scenario.audit(awaiting_approval), ["PLAN_SAVED"]);
    let original = scenario.backend.observation.binding.clone();
    scenario.backend.observation.binding.generation = hash(b"new generation");
    scenario.backend.observation.binding.snapshot = hash(b"new snapshot");
    assert!(matches!(
        scenario.try_plan(),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));

    scenario.backend.observation.binding.endpoint = hash(b"other endpoint");
    assert!(scenario.try_plan().is_ok());
    scenario.backend.observation.binding.endpoint = original.endpoint.clone();
    scenario.backend.observation.binding.id = "untouched".into();
    assert!(scenario.try_plan().is_ok());
    let mut image = TestBackend::new(
        &scenario.temp.path().join("image-resources.sqlite3"),
        scenario.backend.registry_path.clone(),
        scenario.project,
        original.owner.clone(),
        DockerGcKind::Image,
    );
    assert!(image
        .service()
        .plan(
            &mut scenario.registry,
            scenario.project,
            POLICY,
            DockerGcKind::Image,
            IMAGE
        )
        .is_ok());

    let file_identity = crate::filesystem::FileIdentity {
        device: 1,
        inode: 2,
    };
    let file_fingerprint = hash(&serde_json::to_vec(&file_identity).unwrap());
    scenario
        .registry
        .register_gc_output(scenario.project, "build", &file_fingerprint)
        .unwrap();
    let file = scenario
        .registry
        .save_plan(
            scenario.project,
            POLICY,
            &json!({"dry_run": true, "candidates": [{"relative_path": "build", "identity": file_identity}]}),
            PLAN_TTL,
        )
        .unwrap();
    scenario
        .registry
        .approve_gc(
            scenario.project,
            file.plan_id,
            "build",
            POLICY,
            &file_fingerprint,
            OTHER_POLICY,
            APPROVAL_TTL,
        )
        .unwrap();
    let lease = scenario.lease();
    scenario
        .registry
        .claim_gc(&lease, file.plan_id, POLICY, OTHER_POLICY)
        .unwrap();
    scenario
        .registry
        .finish_gc(&lease, file.plan_id, GcApplyStatus::Preserved, 0)
        .unwrap();

    let payload = scenario
        .registry
        .read_gc_plan(scenario.project, pending)
        .unwrap();
    let intent = Intent::load(&payload, scenario.project, &original.owner).unwrap();
    scenario
        .backend
        .state
        .execute(
            "INSERT INTO receipts (operation, intent, bytes) VALUES (?1, ?2, NULL)",
            params![intent.operation, serde_json::to_string(&intent).unwrap()],
        )
        .unwrap();
    assert_eq!(
        scenario.recover(&lease, pending).unwrap().status,
        GcApplyStatus::Preserved
    );
    assert_eq!(scenario.backend.attempts, 1);
    scenario.backend.observation.binding = original;
    scenario.backend.lose_receipt = false;
    assert_eq!(
        scenario.apply(&lease, next, &next_approval).unwrap().status,
        GcApplyStatus::Applied
    );
    assert_eq!(scenario.backend.attempts, 2);
}
