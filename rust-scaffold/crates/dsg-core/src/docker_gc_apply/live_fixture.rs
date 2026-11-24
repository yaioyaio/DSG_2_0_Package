use super::*;
use crate::docker_image_gc::{ImageGcDisposition, ImageGcReason};
use crate::registry::AuditOutcome;
use rusqlite::{params, Connection, OpenFlags};
use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EVIDENCE: &str = "dist/dsg-dedicated-engine-cf83a703d2fd/PREAPPROVAL.json";
const PROOF_DIGEST: &str = "ec0a07dcfd16b2e35fb7a98f4bdb133a243d7060927d125f9b965fc18ee29db8";
const TRIAL: &str = "dsg-gc-cf83a703d2fd";
const ENGINE: &str = "3f0311f3-c456-43e5-a123-014f500108fd";
const ENDPOINT: &str = "a89c4fafeca5767dd5ea0bae7b80cc29d3256b785bf3cd6400797e87636217a3";
const GENERATION: &str = "91258d7a9be96b3a41320dc67c09329b0e81fa23d865cd059fffe37ad9296e50";
const IMAGE: &str = "sha256:a854697496f4c843431566255d0cb2ae780ebf51f9ca70773445c1f52549f94c";
const SNAPSHOT: &str = "521b8b2bfbb96467c43bac1e44daa3453833eda3bd346742fe81d31e2a36211e";
const GUEST_OWNER: &str = "2a35531c6dc5407c1208ac8639a77b7898ef739ea29e821705b5abcc3e1ae81c";
const ARTIFACT_LIMIT: u64 = 4096;
const DISPATCH_FILE: &str = "DISPATCH.json";
const RECEIPT_FILE: &str = "RECEIPT.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GuestProof {
    verified: bool,
    engine_id: String,
    generation: String,
    image_id: String,
    snapshot: String,
    author_fingerprint: String,
    delete_requests: u64,
    after_images: Option<u64>,
    effect: Option<String>,
}

impl GuestProof {
    fn matches(&self, applied: bool) -> bool {
        self.verified
            && self.engine_id == ENGINE
            && self.generation == GENERATION
            && self.image_id == IMAGE
            && self.snapshot == SNAPSHOT
            && self.author_fingerprint == GUEST_OWNER
            && if applied {
                self.delete_requests == 1
                    && self.after_images == Some(0)
                    && self.effect.as_deref() == Some("applied")
            } else {
                self.delete_requests == 0 && self.after_images.is_none() && self.effect.is_none()
            }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Dispatch {
    intent: Intent,
    intent_digest: String,
    proof_digest: String,
}

impl Dispatch {
    fn new(intent: &Intent) -> Option<Self> {
        Some(Self {
            intent: intent.clone(),
            intent_digest: hash(&serde_json::to_vec(intent).ok()?),
            proof_digest: PROOF_DIGEST.into(),
        })
    }

    fn matches(&self, intent: &Intent) -> bool {
        self.intent == *intent
            && self.proof_digest == PROOF_DIGEST
            && serde_json::to_vec(intent)
                .is_ok_and(|serialized| self.intent_digest == hash(&serialized))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableReceipt {
    dispatch: Dispatch,
    guest: GuestProof,
}

struct LiveFixture {
    // The registered host project and the pinned guest author are distinct identities.
    // Only this fixture maps a verified guest result to the local registered owner.
    binding: Binding,
    state: PathBuf,
    helper: PathBuf,
    proof: Option<Value>,
}

impl LiveFixture {
    fn service(&mut self) -> DockerGcService<'_> {
        DockerGcService {
            controller: Some(self),
        }
    }

    fn guest(&self, verb: &str, intent: Option<&Intent>) -> Option<GuestProof> {
        let proof = self.proof.as_ref()?;
        let input = serde_json::to_vec(&json!({ "proof": proof, "intent": intent })).ok()?;
        let mut child = Command::new("/usr/bin/python3")
            .arg("-E")
            .arg(&self.helper)
            .arg(verb)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let sent = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(&input).is_ok());
        let stdout = child.stdout.take();
        let output = stdout.and_then(|pipe| read_limited(pipe));
        let status = child.wait().ok()?;
        if !sent || !status.success() {
            return None;
        }
        let result: GuestProof = serde_json::from_slice(&output?).ok()?;
        result.matches(intent.is_some()).then_some(result)
    }

    fn registry_path(&self) -> PathBuf {
        self.state.join("registry/registry.sqlite3")
    }

    fn intent_committed(&self, intent: &Intent) -> bool {
        let payload = json!({ (MANAGED_DOCKER_TARGET): intent });
        let Ok(connection) =
            Connection::open_with_flags(self.registry_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            return false;
        };
        matches!(
            connection.query_row(
                "SELECT COUNT(*) FROM audit_event e
                 JOIN gc_plan p ON p.id = e.plan_id
                 JOIN gc_application a ON a.plan_id = p.id
                 WHERE e.reason_code = 'GC_APPLY_INTENT' AND e.lease_id IS NOT NULL
                   AND p.status = 'applying' AND p.project_id = ?1 AND p.payload_json = ?2
                   AND a.claimed_at_ms IS NOT NULL AND a.bytes_applied IS NULL",
                params![intent.binding.project, payload.to_string()],
                |row| row.get::<_, i64>(0),
            ),
            Ok(1)
        )
    }
}

impl ControlledDockerGc for LiveFixture {
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
        if owner != self.binding.owner || kind != DockerGcKind::Image || id != IMAGE {
            return None;
        }
        self.guest("observe", None)?;
        Some(Observation {
            binding: self.binding.clone(),
            unreferenced: Some(true),
            usage: Usage::Image(ImageGcPlan {
                context: TRIAL.into(),
                engine_id: Some(ENGINE.into()),
                image_id: IMAGE.into(),
                disposition: ImageGcDisposition::Candidate,
                reason: ImageGcReason::Eligible,
                fingerprint: Some(SNAPSHOT.into()),
                apply_gate: DockerApplyGate::Controlled,
            }),
        })
    }

    fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt> {
        if self.proof.is_none()
            || self.binding != intent.binding
            || intent.policy != PROOF_DIGEST
            || !self.intent_committed(intent)
        {
            return None;
        }
        let dispatch = Dispatch::new(intent)?;
        write_new(&self.state, DISPATCH_FILE, &dispatch)?;
        let guest = self.guest("apply", Some(intent))?;
        write_new(
            &self.state,
            RECEIPT_FILE,
            &DurableReceipt { dispatch, guest },
        )?;
        resume_unwind(Box::new(InterruptedAfterReceipt))
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        let dispatch: Dispatch =
            serde_json::from_slice(&read_regular(&self.state.join(DISPATCH_FILE))?).ok()?;
        let receipt: DurableReceipt =
            serde_json::from_slice(&read_regular(&self.state.join(RECEIPT_FILE))?).ok()?;
        (self.binding == intent.binding
            && dispatch.matches(intent)
            && receipt.dispatch.matches(intent)
            && receipt.guest.matches(true))
        .then(|| Receipt {
            intent: receipt.dispatch.intent,
            effect: Effect::Applied(0),
        })
    }
}

struct InterruptedAfterReceipt;

fn read_limited(reader: impl Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(ARTIFACT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= ARTIFACT_LIMIT).then_some(bytes)
}

fn read_regular(path: &Path) -> Option<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > ARTIFACT_LIMIT {
        return None;
    }
    read_limited(file)
}

fn write_new(directory: &Path, name: &str, record: &impl Serialize) -> Option<()> {
    let bytes = serde_json::to_vec(record).ok()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(name))
        .ok()?;
    file.write_all(&bytes).ok()?;
    file.sync_all().ok()?;
    File::open(directory).ok()?.sync_all().ok()
}

fn checked_proof(repo: &Path) -> Option<(PathBuf, Value)> {
    if env::var("DSG_DEDICATED_GC_APPROVED_IMAGE").ok()?.as_str() != IMAGE
        || env::var("DSG_DEDICATED_GC_APPROVED_GENERATION")
            .ok()?
            .as_str()
            != GENERATION
    {
        return None;
    }
    let source = repo.join(PathBuf::from(env::var_os("DSG_DEDICATED_GC_EVIDENCE")?));
    if source != repo.join(EVIDENCE) || fs::canonicalize(&source).ok()? != source {
        return None;
    }
    let bytes = read_regular(&source)?;
    if hash(&bytes) != PROOF_DIGEST {
        return None;
    }
    let proof: Value = serde_json::from_slice(&bytes).ok()?;
    let expected = [
        ("trial", TRIAL),
        ("engine_id", ENGINE),
        ("endpoint", ENDPOINT),
        ("generation", GENERATION),
        ("image_id", IMAGE),
        ("snapshot", SNAPSHOT),
        ("author_fingerprint", GUEST_OWNER),
    ];
    (expected.iter().all(|(key, value)| proof[*key] == *value)
        && proof["delete_requests"] == 0
        && proof["before_images"] == 0
        && proof["after_images"] == 1
        && proof["guest_restored_to_suspended"] == true)
        .then_some((source, proof))
}

fn ledger(backend: &LiveFixture, plan: i64) -> (String, bool, Option<i64>, Vec<String>) {
    let conn =
        Connection::open_with_flags(backend.registry_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open independent read-only registry");
    let (status, claimed, bytes) = conn
        .query_row(
            "SELECT p.status, a.claimed_at_ms IS NOT NULL, a.bytes_applied
             FROM gc_plan p JOIN gc_application a ON a.plan_id = p.id WHERE p.id = ?1",
            [plan],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read plan status, claim and accounting columns");
    let events = conn
        .prepare("SELECT reason_code FROM audit_event WHERE plan_id = ?1 ORDER BY id")
        .expect("query audit reason codes")
        .query_map([plan], |row| row.get(0))
        .expect("read plan audit events")
        .collect::<Result<Vec<String>, _>>()
        .expect("collect plan audit events");
    (status, claimed, bytes, events)
}

#[test]
#[ignore = "requires explicit approval of the pinned, dedicated Docker trial"]
fn controlled_docker_live_approved_receipt() {
    let repo = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .expect("resolve repository path");
    let (source, proof) = checked_proof(&repo).expect("explicit pinned trial proof required");
    let state = source.parent().unwrap().join(".dsg-live-apply");
    DirBuilder::new()
        .mode(0o700)
        .create(&state)
        .expect("one-use private trial state must not already exist");
    File::open(source.parent().unwrap())
        .and_then(|directory| directory.sync_all())
        .expect("persist the new trial state directory");
    let project_root = state.join("synthetic-project");
    DirBuilder::new()
        .mode(0o700)
        .create(&project_root)
        .expect("create isolated fixture project");
    let registry_dir = state.join("registry");
    let mut registry = Registry::open(&registry_dir).expect("open isolated fixture registry");
    let project = registry
        .register(&project_root, "dedicated-docker-trial", 1)
        .expect("register synthetic project")
        .id;
    let owner = registry
        .docker_gc_project_owner(project)
        .expect("derive local registered owner");
    assert!(owner != GUEST_OWNER && valid_owner_fingerprint(&owner));
    let mut backend = LiveFixture {
        binding: Binding {
            project,
            owner,
            kind: DockerGcKind::Image,
            id: IMAGE.into(),
            endpoint: ENDPOINT.into(),
            generation: GENERATION.into(),
            snapshot: SNAPSHOT.into(),
        },
        state,
        helper: repo.join("scripts/check-dedicated-docker-apply.py"),
        proof: Some(proof),
    };
    let plan = backend
        .service()
        .plan(
            &mut registry,
            project,
            PROOF_DIGEST,
            DockerGcKind::Image,
            IMAGE,
        )
        .expect("plan the verified image")
        .plan_id;
    let approval = backend
        .service()
        .approve(&mut registry, project, plan, PROOF_DIGEST)
        .expect("issue one-use local approval");
    let lease = registry
        .acquire_lease(project, PROOF_DIGEST, Duration::from_secs(180), |_| false)
        .expect("acquire isolated trial lease");
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        backend
            .service()
            .apply(&mut registry, &lease, plan, PROOF_DIGEST, &approval)
    }));
    assert!(
        interrupted
            .err()
            .is_some_and(|error| error.is::<InterruptedAfterReceipt>()),
        "expected interruption between durable receipt and registry accounting"
    );
    let expected_pending = ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT"];
    let (status, claimed, bytes, events) = ledger(&backend, plan);
    assert_eq!(status, "applying");
    assert!(claimed);
    assert_eq!(bytes, None);
    assert_eq!(events, expected_pending);
    drop(registry);
    backend.proof = None;
    let mut registry = Registry::open(&registry_dir).expect("reopen isolated fixture registry");
    let recovered = backend
        .service()
        .recover(&mut registry, &lease, plan)
        .expect("recover from local receipt");
    assert_eq!(
        recovered,
        DockerGcResult {
            status: GcApplyStatus::Applied,
            reclaimed_bytes: 0,
        }
    );
    assert!(matches!(
        backend
            .service()
            .apply(&mut registry, &lease, plan, PROOF_DIGEST, &approval),
        Err(DockerGcError::Registry(RegistryError::GcStateConflict))
    ));
    assert!(matches!(
        backend.service().recover(&mut registry, &lease, plan),
        Err(DockerGcError::Registry(RegistryError::GcStateConflict))
    ));
    let (status, claimed, bytes, events) = ledger(&backend, plan);
    assert_eq!(status, "applied");
    assert!(claimed);
    assert_eq!(bytes, Some(0));
    assert_eq!(
        events,
        ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT", "GC_APPLIED"]
    );
    let stored: DurableReceipt =
        serde_json::from_slice(&read_regular(&backend.state.join(RECEIPT_FILE)).unwrap())
            .expect("read persisted bounded receipt evidence");
    registry
        .finish_lease(lease, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .expect("close isolated trial lease");
    println!(
        "DSG_LIVE_GC_RESULT {}",
        json!({
            "status": status,
            "reclaimed_bytes": bytes,
            "engine_id": stored.guest.engine_id,
            "generation": stored.guest.generation,
            "image_id": stored.guest.image_id,
            "intent_events": events.iter().filter(|event| *event == "GC_APPLY_INTENT").count(),
            "applied_events": events.iter().filter(|event| *event == "GC_APPLIED").count(),
            "delete_requests": stored.guest.delete_requests,
            "after_images": stored.guest.after_images,
            "approval_reuse_rejected": true,
            "recovery_reuse_rejected": true,
        })
    );
}
