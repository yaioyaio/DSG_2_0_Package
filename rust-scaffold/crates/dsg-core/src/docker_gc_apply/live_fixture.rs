use super::*;
use crate::docker_image_gc::{ImageGcDisposition, ImageGcReason};
use crate::registry::AuditOutcome;
use rusqlite::{params, Connection, OpenFlags};
use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

const EVIDENCE: &str = "dist/dsg-dedicated-engine-cf83a703d2fd/PREAPPROVAL.json";
const PROOF_DIGEST: &str = "ec0a07dcfd16b2e35fb7a98f4bdb133a243d7060927d125f9b965fc18ee29db8";
const TRIAL: &str = "dsg-gc-cf83a703d2fd";
const CONSUMED_RECEIPT_LOSS_TRIAL: &str = "dsg-gc-93292d9f465f";
const ENGINE: &str = "3f0311f3-c456-43e5-a123-014f500108fd";
const ENDPOINT: &str = "a89c4fafeca5767dd5ea0bae7b80cc29d3256b785bf3cd6400797e87636217a3";
const GENERATION: &str = "91258d7a9be96b3a41320dc67c09329b0e81fa23d865cd059fffe37ad9296e50";
const IMAGE: &str = "sha256:a854697496f4c843431566255d0cb2ae780ebf51f9ca70773445c1f52549f94c";
const SNAPSHOT: &str = "521b8b2bfbb96467c43bac1e44daa3453833eda3bd346742fe81d31e2a36211e";
const GUEST_OWNER: &str = "2a35531c6dc5407c1208ac8639a77b7898ef739ea29e821705b5abcc3e1ae81c";
const ARTIFACT_LIMIT: u64 = 4096;
const DISPATCH_FILE: &str = "DISPATCH.json";
const RECEIPT_FILE: &str = "RECEIPT.json";
const WITNESS_FILE: &str = "DELETE_RESPONSE_WITNESS.json";
const RESPONSE_LOSS_WITNESS_FILE: &str = "DELETE_RESPONSE_LOSS_WITNESS.json";
const DELETE_ATTEMPT_FILE: &str = "DELETE_ATTEMPT.json";
const RETRY_REGISTRY: &str = "retry-registry";
const RESUME_LOCK: &str = "PREFLIGHT_RESUME.lock";
const RESUME_PREFLIGHT: &str = "DSG_DEDICATED_GC_RESUME_PREFLIGHT";
const TRANSIENT_OBSERVATION: &str = "E_OBSERVE_TRANSIENT";
const TRANSPORT_FAILURE: &str = "E_TRANSPORT";
const RESUME_OBSERVATION_ATTEMPTS: u32 = 3;
const REGISTRY_FILES: [&str; 3] = [
    "registry.sqlite3",
    "registry.sqlite3-wal",
    "registry.sqlite3-shm",
];

#[derive(Clone, Copy, Eq, PartialEq)]
enum TrialMode {
    RetainReceipt,
    LoseReceipt,
    LoseDeleteResponse,
}

#[derive(Deserialize)]
struct TrialIdentity {
    trial: String,
    engine_id: String,
    endpoint: String,
    generation: String,
    image_id: String,
    snapshot: String,
    author_fingerprint: String,
    apply_trial: Option<String>,
}

struct Trial {
    mode: TrialMode,
    source: PathBuf,
    identity: TrialIdentity,
    digest: String,
    wire: Value,
}

impl Trial {
    fn guest_command(&self, applying: bool) -> &'static str {
        match (self.mode, applying) {
            (TrialMode::RetainReceipt, false) => "observe",
            (TrialMode::RetainReceipt, true) => "apply",
            (TrialMode::LoseReceipt, false) => "observe-receipt-loss",
            (TrialMode::LoseReceipt, true) => "apply-receipt-loss",
            (TrialMode::LoseDeleteResponse, false) => "observe-response-loss",
            (TrialMode::LoseDeleteResponse, true) => "apply-response-loss",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeleteResponseHandling {
    ClientReadShutdown,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_response_bytes_read: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_response_handling: Option<DeleteResponseHandling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_delete_attempt_absent: Option<bool>,
}

impl GuestProof {
    fn matches(&self, trial: &Trial, applied: bool) -> bool {
        let identity = &trial.identity;
        let response_lost = trial.mode == TrialMode::LoseDeleteResponse && applied;
        self.verified
            && self.engine_id == identity.engine_id
            && self.generation == identity.generation
            && self.image_id == identity.image_id
            && self.snapshot == identity.snapshot
            && self.author_fingerprint == identity.author_fingerprint
            && self.delete_http_status
                == (trial.mode == TrialMode::LoseReceipt && applied).then_some(200)
            && self.delete_response_bytes_read == response_lost.then_some(0)
            && self.delete_response_handling
                == response_lost.then_some(DeleteResponseHandling::ClientReadShutdown)
            && self.guest_delete_attempt_absent
                == (trial.mode == TrialMode::LoseDeleteResponse && !applied).then_some(true)
            && if applied {
                self.delete_requests == 1
                    && self.after_images == Some(0)
                    && self.effect.as_deref() == (!response_lost).then_some("applied")
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
    fn new(intent: &Intent, proof_digest: &str) -> Option<Self> {
        Some(Self {
            intent: intent.clone(),
            intent_digest: hash(&serde_json::to_vec(intent).ok()?),
            proof_digest: proof_digest.into(),
        })
    }

    fn matches(&self, intent: &Intent, proof_digest: &str) -> bool {
        self.intent == *intent
            && self.proof_digest == proof_digest
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeleteResponseWitness {
    delete_http_status: u16,
    delete_requests: u64,
    after_images: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeleteResponseLossWitness {
    delete_requests: u64,
    delete_response_bytes_read: u64,
    delete_response_handling: DeleteResponseHandling,
    after_images: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteAttempt {
    intent_digest: String,
    proof_digest: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FixtureCalls {
    observations: u64,
    guest_observations: u64,
    mutations: u64,
    guest_mutations: u64,
    receipt_reads: u64,
}

struct LiveFixture {
    // The registered host project and the pinned guest author are distinct identities.
    // Only this fixture maps a verified guest result to the local registered owner.
    binding: Binding,
    state: PathBuf,
    registry_dir: PathBuf,
    helper: PathBuf,
    trial: Trial,
    resume_lock: Option<File>,
    guest_enabled: bool,
    calls: FixtureCalls,
}

impl LiveFixture {
    fn service(&mut self) -> DockerGcService<'_> {
        DockerGcService {
            controller: Some(self),
        }
    }

    fn guest(&mut self, intent: Option<&Intent>) -> Option<GuestProof> {
        if !self.guest_enabled {
            return None;
        }
        if intent.is_some() {
            self.calls.guest_mutations += 1;
            return guest_once(&self.helper, &self.trial, intent)
                .map_err(|code| report_guest_failure("apply", code))
                .ok();
        }
        observe_guest(
            &self.helper,
            &self.trial,
            "observe",
            self.resume_lock.is_some(),
            &mut self.calls.guest_observations,
        )
    }

    fn registry_path(&self) -> PathBuf {
        self.registry_dir.join("registry.sqlite3")
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
        self.calls.observations += 1;
        if owner != self.binding.owner || kind != DockerGcKind::Image || id != self.binding.id {
            return None;
        }
        self.guest(None)?;
        let identity = &self.trial.identity;
        Some(Observation {
            binding: self.binding.clone(),
            unreferenced: Some(true),
            usage: Usage::Image(ImageGcPlan {
                context: identity.trial.clone(),
                engine_id: Some(identity.engine_id.clone()),
                image_id: identity.image_id.clone(),
                disposition: ImageGcDisposition::Candidate,
                reason: ImageGcReason::Eligible,
                fingerprint: Some(identity.snapshot.clone()),
                apply_gate: DockerApplyGate::Controlled,
            }),
        })
    }

    fn apply_if_current(&mut self, intent: &Intent) -> Option<Receipt> {
        self.calls.mutations += 1;
        if !self.guest_enabled
            || self.binding != intent.binding
            || intent.policy != self.trial.digest
            || !self.intent_committed(intent)
        {
            return None;
        }
        let dispatch = Dispatch::new(intent, &self.trial.digest)?;
        write_new(&self.state, DISPATCH_FILE, &dispatch)?;
        let guest = self.guest(Some(intent))?;
        match self.trial.mode {
            TrialMode::LoseReceipt => {
                write_new(
                    &self.state,
                    WITNESS_FILE,
                    &DeleteResponseWitness {
                        delete_http_status: guest.delete_http_status?,
                        delete_requests: guest.delete_requests,
                        after_images: guest.after_images?,
                    },
                )?;
                resume_unwind(Box::new(InterruptedBeforeReceipt));
            }
            TrialMode::LoseDeleteResponse => {
                write_new(
                    &self.state,
                    RESPONSE_LOSS_WITNESS_FILE,
                    &DeleteResponseLossWitness {
                        delete_requests: guest.delete_requests,
                        delete_response_bytes_read: guest.delete_response_bytes_read?,
                        delete_response_handling: guest.delete_response_handling?,
                        after_images: guest.after_images?,
                    },
                )?;
                resume_unwind(Box::new(InterruptedBeforeReceipt));
            }
            TrialMode::RetainReceipt => {
                write_new(
                    &self.state,
                    RECEIPT_FILE,
                    &DurableReceipt { dispatch, guest },
                )?;
                resume_unwind(Box::new(InterruptedAfterReceipt))
            }
        }
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        self.calls.receipt_reads += 1;
        let dispatch: Dispatch =
            serde_json::from_slice(&read_regular(&self.state.join(DISPATCH_FILE))?).ok()?;
        let receipt: DurableReceipt =
            serde_json::from_slice(&read_regular(&self.state.join(RECEIPT_FILE))?).ok()?;
        (self.trial.mode != TrialMode::LoseDeleteResponse
            && self.binding == intent.binding
            && dispatch.matches(intent, &self.trial.digest)
            && receipt.dispatch.matches(intent, &self.trial.digest)
            && receipt.guest.matches(&self.trial, true))
        .then(|| Receipt {
            intent: receipt.dispatch.intent,
            effect: Effect::Applied(0),
        })
    }
}

struct InterruptedAfterReceipt;
struct InterruptedBeforeReceipt;

fn observe_guest(
    helper: &Path,
    trial: &Trial,
    phase: &str,
    retry_transient: bool,
    calls: &mut u64,
) -> Option<GuestProof> {
    let attempts = if retry_transient {
        RESUME_OBSERVATION_ATTEMPTS
    } else {
        1
    };
    for attempt in 1..=attempts {
        *calls += 1;
        match guest_once(helper, trial, None) {
            Ok(proof) => return Some(proof),
            Err(code) => {
                report_guest_failure(phase, code);
                if attempt == attempts || !matches!(code, TRANSIENT_OBSERVATION | TRANSPORT_FAILURE)
                {
                    return None;
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

fn guest_once(
    helper: &Path,
    trial: &Trial,
    intent: Option<&Intent>,
) -> Result<GuestProof, &'static str> {
    let applying = intent.is_some();
    let mut envelope = trial.wire.clone();
    envelope["intent"] = serde_json::to_value(intent).map_err(|_| "E_INPUT")?;
    let input = serde_json::to_vec(&envelope).map_err(|_| "E_INPUT")?;
    let mut child = Command::new("/usr/bin/python3")
        .arg("-E")
        .arg(helper)
        .arg(trial.guest_command(applying))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "E_HELPER_START")?;
    let sent = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(&input).is_ok());
    let output = child.stdout.take().and_then(read_limited);
    let errors = child.stderr.take().and_then(read_limited);
    let status = child.wait().map_err(|_| "E_HELPER_WAIT")?;
    if !sent || !status.success() {
        return Err(errors
            .as_deref()
            .and_then(helper_error)
            .unwrap_or("E_HELPER_EXIT"));
    }
    let output = output.ok_or("E_HELPER_OUTPUT")?;
    let result: GuestProof = serde_json::from_slice(&output).map_err(|_| "E_HELPER_OUTPUT")?;
    result
        .matches(trial, applying)
        .then_some(result)
        .ok_or("E_HELPER_PROOF")
}

fn helper_error(bytes: &[u8]) -> Option<&'static str> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    [
        "E_INPUT",
        "E_BINDING",
        "E_VM",
        "E_UNIT",
        "E_PROOF",
        "E_ISOLATION",
        "E_PEER",
        "E_INVENTORY",
        "E_SNAPSHOT",
        "E_TIME",
        "E_DELETE_STATUS",
        "E_DELETE_RESULT",
        "E_AFTER",
        TRANSPORT_FAILURE,
        "E_RUNTIME",
        "E_RESPONSE_LOSS",
        "E_DISPATCH",
        TRANSIENT_OBSERVATION,
    ]
    .into_iter()
    .find(|code| value.get("error").and_then(Value::as_str) == Some(*code))
}

fn report_guest_failure(phase: &str, code: &'static str) {
    eprintln!("DSG_LIVE_GC_HELPER_FAILURE phase={phase} category={code}");
}

fn read_limited(reader: impl Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(ARTIFACT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= ARTIFACT_LIMIT).then_some(bytes)
}

fn open_regular(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > ARTIFACT_LIMIT {
        return None;
    }
    Some(file)
}

fn read_regular(path: &Path) -> Option<Vec<u8>> {
    read_limited(open_regular(path)?)
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

fn checked_pinned_trial(repo: &Path) -> Option<Trial> {
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
    let identity = serde_json::from_value(proof.clone()).ok()?;
    (expected.iter().all(|(key, value)| proof[*key] == *value)
        && proof["delete_requests"] == 0
        && proof["before_images"] == 0
        && proof["after_images"] == 1
        && proof["guest_restored_to_suspended"] == true)
        .then(|| Trial {
            mode: TrialMode::RetainReceipt,
            source,
            identity,
            digest: PROOF_DIGEST.into(),
            wire: json!({ "proof": proof }),
        })
}

fn protected_directory(path: &Path, uid: u32, private: bool) -> Option<()> {
    let info = fs::symlink_metadata(path).ok()?;
    (info.is_dir()
        && info.uid() == uid
        && if private {
            info.mode() & 0o7777 == 0o700
        } else {
            info.mode() & 0o022 == 0
        })
    .then_some(())
}

fn checked_approved_trial(repo: &Path, mode: TrialMode) -> Option<Trial> {
    let source = repo.join(PathBuf::from(env::var_os("DSG_DEDICATED_GC_EVIDENCE")?));
    let dist = repo.join("dist");
    let directory = source.parent()?;
    let suffix = directory
        .file_name()?
        .to_str()?
        .strip_prefix("dsg-dedicated-engine-")?;
    if source.file_name()?.to_str()? != "PREAPPROVAL.json"
        || directory.parent()? != dist
        || suffix.len() != 12
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || fs::canonicalize(&source).ok()? != source
        || source == repo.join(EVIDENCE)
    {
        return None;
    }
    // SAFETY: geteuid reads the process identity and accepts no arguments.
    let uid = unsafe { libc::geteuid() };
    protected_directory(&dist, uid, false)?;
    protected_directory(directory, uid, true)?;
    let evidence = open_regular(&source)?;
    let info = evidence.metadata().ok()?;
    if info.uid() != uid || info.mode() & 0o7177 != 0 || info.nlink() != 1 {
        return None;
    }
    let bytes = read_limited(evidence)?;
    let digest = env::var("DSG_DEDICATED_GC_APPROVED_PROOF_SHA256").ok()?;
    if hash(&bytes) != digest {
        return None;
    }
    let identity: TrialIdentity = serde_json::from_slice(&bytes).ok()?;
    let engine = env::var("DSG_DEDICATED_GC_APPROVED_ENGINE").ok()?;
    let generation = env::var("DSG_DEDICATED_GC_APPROVED_GENERATION").ok()?;
    let image = env::var("DSG_DEDICATED_GC_APPROVED_IMAGE").ok()?;
    let approved_mode = match mode {
        TrialMode::LoseReceipt => identity.apply_trial.is_none(),
        TrialMode::LoseDeleteResponse => {
            identity.apply_trial.as_deref() == Some("response-loss")
                && identity.trial != CONSUMED_RECEIPT_LOSS_TRIAL
        }
        TrialMode::RetainReceipt => false,
    };
    if identity.trial != format!("dsg-gc-{suffix}")
        || !approved_mode
        || identity.engine_id != engine
        || identity.generation != generation
        || identity.image_id != image
        || !valid_full_image_id(&image)
        || ![
            &identity.endpoint,
            &identity.generation,
            &identity.snapshot,
            &identity.author_fingerprint,
        ]
        .into_iter()
        .all(|value| valid_owner_fingerprint(value))
    {
        return None;
    }
    let wire = json!({
        "proof": std::str::from_utf8(&bytes).ok()?,
        "approval": { "proof_sha256": digest, "engine_id": engine, "generation": generation, "image_id": image },
    });
    Some(Trial {
        mode,
        source,
        identity,
        digest,
        wire,
    })
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

fn repository() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .expect("resolve repository path")
}

fn fixture_binding(trial: &Trial, project: i64, owner: String) -> Binding {
    let identity = &trial.identity;
    Binding {
        project,
        owner,
        kind: DockerGcKind::Image,
        id: identity.image_id.clone(),
        endpoint: identity.endpoint.clone(),
        generation: identity.generation.clone(),
        snapshot: identity.snapshot.clone(),
    }
}

fn state_entry_absent(state: &Path, name: &str) -> bool {
    matches!(fs::symlink_metadata(state.join(name)), Err(error) if error.kind() == ErrorKind::NotFound)
}

fn preflight_markers_absent(state: &Path) -> bool {
    [
        DISPATCH_FILE,
        DELETE_ATTEMPT_FILE,
        WITNESS_FILE,
        RESPONSE_LOSS_WITNESS_FILE,
        RECEIPT_FILE,
    ]
    .into_iter()
    .all(|name| state_entry_absent(state, name))
}

fn protected_private_file(file: &fs::Metadata, uid: u32) -> bool {
    file.is_file() && file.uid() == uid && file.mode() & 0o7777 == 0o600 && file.nlink() == 1
}

fn read_protected_registry(dir: &Path, uid: u32) -> Option<Connection> {
    protected_directory(dir, uid, true)?;
    for name in REGISTRY_FILES {
        let file = fs::symlink_metadata(dir.join(name)).ok()?;
        if !protected_private_file(&file, uid) {
            return None;
        }
    }
    if !state_entry_absent(dir, "registry.sqlite3-journal") {
        return None;
    }
    // Committed evidence may still be in the WAL. Read the existing sidecars
    // without asking Registry to migrate or initialize any state.
    let connection = Connection::open_with_flags(
        dir.join("registry.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .ok()?;
    connection.pragma_update(None, "query_only", true).ok()?;
    Some(connection)
}

fn preserved_preflight(state: &Path, project_root: &Path, trial: &Trial, uid: u32) -> Option<()> {
    let mut connection = read_protected_registry(&state.join("registry"), uid)?;
    let tx = connection.transaction().ok()?;
    let root = project_root.to_str()?;
    let project: i64 = tx
        .query_row(
            "SELECT id FROM project WHERE root_path = ?1 AND label = 'dedicated-docker-trial'
             AND policy_version = 1 AND (SELECT COUNT(*) FROM project) = 1",
            [root],
            |row| row.get(0),
        )
        .ok()?;
    let binding = fixture_binding(trial, project, hash(root.as_bytes()));
    let mut plans = tx
        .prepare(
            "SELECT p.id, p.status, a.claimed_at_ms IS NOT NULL, a.bytes_applied, p.payload_json, a.identity
             FROM gc_plan p JOIN gc_application a ON a.plan_id = p.id
             WHERE p.project_id = ?1 AND p.policy_digest = ?2 AND a.relative_path = ''
               AND (SELECT COUNT(*) FROM gc_plan) = 2
               AND (SELECT COUNT(*) FROM gc_application) = 2
             ORDER BY p.id LIMIT 2",
        )
        .ok()?;
    let mut rows = plans.query(params![project, trial.digest]).ok()?;
    let mut plan_ids = Vec::new();
    let mut operations = Vec::new();
    for (status, claimed, bytes) in [("preserved", true, Some(0_i64)), ("approved", false, None)] {
        let row = rows.next().ok()??;
        if row.get::<_, String>(1).ok()? != status
            || row.get::<_, bool>(2).ok()? != claimed
            || row.get::<_, Option<i64>>(3).ok()? != bytes
        {
            return None;
        }
        let payload = serde_json::from_str(&row.get::<_, String>(4).ok()?).ok()?;
        let intent = Intent::load(&payload, project, &binding.owner).ok()?;
        if intent.binding != binding
            || intent.policy != trial.digest
            || managed_docker_identity(&payload).ok()? != row.get::<_, String>(5).ok()?
        {
            return None;
        }
        plan_ids.push(row.get::<_, i64>(0).ok()?);
        operations.push(intent.operation);
    }
    if operations[0] == operations[1] {
        return None;
    }

    let mut events = tx
        .prepare(
            "SELECT a.plan_id, a.reason_code, a.project_id = ?1,
                CASE WHEN a.lease_id IS NULL THEN 0
                     WHEN EXISTS(SELECT 1 FROM execution_lease l WHERE l.id = a.lease_id
                                 AND l.project_id = ?1 AND l.scope_key = ?2) THEN 1
                     ELSE -1 END
             FROM audit_event a WHERE a.plan_id IS NOT NULL
                 OR a.action IN ('gc_plan', 'gc_approve', 'gc_apply')
             ORDER BY a.id LIMIT 7",
        )
        .ok()?;
    let events = events
        .query_map(params![project, trial.digest], |row| {
            Ok((
                row.get(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .ok()?
        .collect::<Result<Vec<(i64, String, bool, i64)>, _>>()
        .ok()?;
    let [plan, competing] = [plan_ids[0], plan_ids[1]];
    events
        .iter()
        .map(|(plan, reason, project_match, lease)| {
            (*plan, reason.as_str(), *project_match, *lease)
        })
        .eq([
            (plan, "PLAN_SAVED", true, 0),
            (competing, "PLAN_SAVED", true, 0),
            (plan, "GC_APPROVED", true, 0),
            (competing, "GC_APPROVED", true, 0),
            (plan, "GC_APPLY_INTENT", true, 1),
            (plan, "GC_PRESERVED", true, 1),
        ])
        .then_some(())
}

fn untouched_retry_project(retry: &Path, root: &Path, uid: u32) -> Option<i64> {
    let connection = read_protected_registry(retry, uid)?;
    for entry in fs::read_dir(retry).ok()? {
        let name = entry.ok()?.file_name();
        if !name
            .to_str()
            .is_some_and(|name| REGISTRY_FILES.contains(&name))
        {
            return None;
        }
    }
    connection
        .query_row(
            "SELECT p.id FROM project p
             JOIN audit_event e ON e.project_id = p.id AND e.timestamp = p.created_at
             WHERE p.root_path = ?1 AND p.label = 'dedicated-docker-trial' AND p.policy_version = 1
               AND p.created_at = p.last_seen_at AND (SELECT COUNT(*) FROM project) = 1
               AND e.actor = 'local_cli' AND e.action = 'project_register' AND e.outcome = 'success'
               AND e.reason_code = 'PROJECT_REGISTERED' AND e.plan_id IS NULL AND e.lease_id IS NULL
               AND (SELECT COUNT(*) FROM audit_event) = 1
               AND NOT EXISTS (SELECT 1 FROM gc_plan)
               AND NOT EXISTS (SELECT 1 FROM gc_application)
               AND NOT EXISTS (SELECT 1 FROM gc_output)
               AND NOT EXISTS (SELECT 1 FROM execution_lease)
               AND NOT EXISTS (SELECT 1 FROM execution_lease_process)",
            [root.to_str()?],
            |row| row.get(0),
        )
        .ok()
}

fn lock_preflight_resume(state: &Path, uid: u32) -> Option<File> {
    let path = state.join(RESUME_LOCK);
    match fs::symlink_metadata(&path) {
        Ok(existing) if protected_private_file(&existing, uid) && existing.len() == 0 => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        _ => return None,
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !protected_private_file(&opened, uid) || opened.len() != 0 {
        return None;
    }
    // SAFETY: the descriptor belongs to the open file and stays owned by this fixture.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let current = fs::symlink_metadata(&path).ok()?;
    if !protected_private_file(&current, uid)
        || current.len() != 0
        || (current.dev(), current.ino()) != (opened.dev(), opened.ino())
    {
        return None;
    }
    file.sync_all().ok()?;
    File::open(state).ok()?.sync_all().ok()?;
    Some(file)
}

fn reserve_preflight_retry(
    state: &Path,
    project_root: &Path,
    trial: &Trial,
    helper: &Path,
) -> (PathBuf, Option<i64>, File) {
    // SAFETY: geteuid reads the process identity and accepts no arguments.
    let uid = unsafe { libc::geteuid() };
    protected_directory(state, uid, true).expect("protected original fixture state required");
    protected_directory(project_root, uid, true)
        .expect("protected original synthetic project required");
    let lock =
        lock_preflight_resume(state, uid).expect("acquire the protected exclusive resume lock");
    assert!(
        preflight_markers_absent(state),
        "original dispatch or outcome evidence exists or is unreadable"
    );
    preserved_preflight(state, project_root, trial, uid)
        .expect("original ledger must prove the exact preserved and unclaimed competing plans");
    let retry = state.join(RETRY_REGISTRY);
    let registered_project = if state_entry_absent(state, RETRY_REGISTRY) {
        None
    } else {
        Some(untouched_retry_project(&retry, project_root, uid).expect(
            "existing retry registry must contain only the original synthetic project registration",
        ))
    };
    let mut observation_calls = 0;
    observe_guest(helper, trial, "resume-check", true, &mut observation_calls)
        .expect("exact guest image and absence of a guest DELETE attempt must be verified");
    assert!(
        preflight_markers_absent(state),
        "dispatch or outcome evidence changed during retry verification"
    );

    if registered_project.is_none() {
        DirBuilder::new()
            .mode(0o700)
            .create(&retry)
            .expect("reserve the one-use preflight retry registry directory");
        File::open(state)
            .and_then(|directory| directory.sync_all())
            .expect("persist the one-use preflight retry reservation");
    }
    (retry, registered_project, lock)
}

fn open_fixture(repo: &Path, trial: Trial) -> (Registry, LiveFixture) {
    let source_directory = trial.source.parent().expect("resolve proof directory");
    let state = source_directory.join(".dsg-live-apply");
    let project_root = state.join("synthetic-project");
    let helper = repo.join("scripts/check-dedicated-docker-apply.py");
    let resume_preflight = match env::var_os(RESUME_PREFLIGHT) {
        None => false,
        Some(value) if value == "1" && trial.mode == TrialMode::LoseDeleteResponse => true,
        Some(_) => panic!("preflight retry requires explicit opt-in for response-loss only"),
    };
    let (registry_dir, registered_project, resume_lock) = if resume_preflight {
        let (dir, project, lock) = reserve_preflight_retry(&state, &project_root, &trial, &helper);
        (dir, project, Some(lock))
    } else {
        DirBuilder::new()
            .mode(0o700)
            .create(&state)
            .expect("one-use private trial state must not already exist");
        File::open(source_directory)
            .and_then(|directory| directory.sync_all())
            .expect("persist the new trial state directory");
        DirBuilder::new()
            .mode(0o700)
            .create(&project_root)
            .expect("create isolated fixture project");
        (state.join("registry"), None, None)
    };
    let mut registry = Registry::open(&registry_dir).expect("open isolated fixture registry");
    let project = registered_project.unwrap_or_else(|| {
        registry
            .register(&project_root, "dedicated-docker-trial", 1)
            .expect("register synthetic project")
            .id
    });
    let owner = registry
        .docker_gc_project_owner(project)
        .expect("derive local registered owner");
    assert!(owner != trial.identity.author_fingerprint && valid_owner_fingerprint(&owner));
    let backend = LiveFixture {
        binding: fixture_binding(&trial, project, owner),
        state,
        registry_dir,
        helper,
        trial,
        resume_lock,
        guest_enabled: true,
        calls: FixtureCalls::default(),
    };
    (registry, backend)
}

#[test]
#[ignore = "requires explicit approval of the pinned, dedicated Docker trial"]
fn controlled_docker_live_approved_receipt() {
    let repo = repository();
    let trial = checked_pinned_trial(&repo).expect("explicit pinned trial proof required");
    let (mut registry, mut backend) = open_fixture(&repo, trial);
    let registry_dir = backend.registry_dir.clone();
    let project = backend.binding.project;
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
    backend.guest_enabled = false;
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

fn assert_receipt_missing(backend: &LiveFixture) {
    assert!(matches!(
        fs::symlink_metadata(backend.state.join(RECEIPT_FILE)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
#[ignore = "requires new explicit approval of the exact dedicated Docker receipt-loss trial"]
fn controlled_docker_live_receipt_loss() {
    run_missing_receipt_trial(TrialMode::LoseReceipt);
}

#[test]
#[ignore = "requires new explicit approval of the exact dedicated Docker DELETE response-loss trial"]
fn controlled_docker_live_response_loss() {
    run_missing_receipt_trial(TrialMode::LoseDeleteResponse);
}

fn run_missing_receipt_trial(mode: TrialMode) {
    let repo = repository();
    let trial = checked_approved_trial(&repo, mode).expect("exact approved trial required");
    let policy = trial.digest.clone();
    let image = trial.identity.image_id.clone();
    let (mut registry, mut backend) = open_fixture(&repo, trial);
    let project = backend.binding.project;
    let registry_dir = backend.registry_dir.clone();
    let plans = [0, 1].map(|_| {
        backend
            .service()
            .plan(&mut registry, project, &policy, DockerGcKind::Image, &image)
            .expect("plan the exact independently verified image")
            .plan_id
    });
    let [plan, competing_plan] = plans;
    let [approval, competing_approval] = plans.map(|candidate| {
        backend
            .service()
            .approve(&mut registry, project, candidate, &policy)
            .expect("issue separate one-use local approvals")
    });
    let lease = registry
        .acquire_lease(project, &policy, Duration::from_secs(180), |_| false)
        .expect("acquire isolated trial lease");
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        backend
            .service()
            .apply(&mut registry, &lease, plan, &policy, &approval)
    }));
    assert!(
        interrupted
            .err()
            .is_some_and(|error| error.is::<InterruptedBeforeReceipt>()),
        "expected interruption after verified deletion and before durable receipt"
    );
    assert_receipt_missing(&backend);
    let (result_label, witness) = match mode {
        TrialMode::LoseReceipt => {
            let witness: DeleteResponseWitness = serde_json::from_slice(
                &read_regular(&backend.state.join(WITNESS_FILE))
                    .expect("read synced deletion witness"),
            )
            .expect("parse deletion witness");
            assert_eq!(witness.delete_http_status, 200);
            assert_eq!(witness.delete_requests, 1);
            assert_eq!(witness.after_images, 0);
            (
                "DSG_LIVE_GC_RECEIPT_LOSS_RESULT",
                serde_json::to_value(witness),
            )
        }
        TrialMode::LoseDeleteResponse => {
            let witness: DeleteResponseLossWitness = serde_json::from_slice(
                &read_regular(&backend.state.join(RESPONSE_LOSS_WITNESS_FILE))
                    .expect("read synced response-loss witness"),
            )
            .expect("parse response-loss witness");
            assert_eq!(witness.delete_requests, 1);
            assert_eq!(witness.delete_response_bytes_read, 0);
            assert_eq!(
                witness.delete_response_handling,
                DeleteResponseHandling::ClientReadShutdown
            );
            assert_eq!(witness.after_images, 0);
            let dispatch: Dispatch = serde_json::from_slice(
                &read_regular(&backend.state.join(DISPATCH_FILE)).expect("read synced dispatch"),
            )
            .expect("parse dispatch");
            let attempt: DeleteAttempt = serde_json::from_slice(
                &read_regular(&backend.state.join(DELETE_ATTEMPT_FILE))
                    .expect("read synced one-use DELETE attempt"),
            )
            .expect("parse DELETE attempt");
            assert_eq!(attempt.intent_digest, dispatch.intent_digest);
            assert_eq!(attempt.proof_digest, dispatch.proof_digest);
            assert_eq!(attempt.proof_digest, policy);
            (
                "DSG_LIVE_GC_RESPONSE_LOSS_RESULT",
                serde_json::to_value(witness),
            )
        }
        TrialMode::RetainReceipt => unreachable!(),
    };
    let witness = witness.expect("encode bounded deletion witness");
    let before_recovery = backend.calls;
    assert_eq!(before_recovery.mutations, 1);
    assert_eq!(before_recovery.guest_mutations, 1);
    assert_eq!(before_recovery.receipt_reads, 0);
    let (status, claimed, bytes, events) = ledger(&backend, plan);
    assert_eq!(status, "applying");
    assert!(claimed);
    assert_eq!(bytes, None);
    assert_eq!(events, ["PLAN_SAVED", "GC_APPROVED", "GC_APPLY_INTENT"]);

    drop(registry);
    backend.guest_enabled = false;
    let mut registry = Registry::open(&registry_dir).expect("reopen isolated fixture registry");
    let recovered = backend
        .service()
        .recover(&mut registry, &lease, plan)
        .expect("recover the pending intent");
    assert_eq!(
        recovered,
        DockerGcResult {
            status: GcApplyStatus::RecoveryRequired,
            reclaimed_bytes: 0,
        }
    );
    let after_recovery = backend.calls;
    assert_eq!(
        after_recovery,
        FixtureCalls {
            receipt_reads: before_recovery.receipt_reads + 1,
            ..before_recovery
        }
    );
    assert!(matches!(
        backend
            .service()
            .apply(&mut registry, &lease, plan, &policy, &approval),
        Err(DockerGcError::Registry(RegistryError::GcStateConflict))
    ));
    assert!(matches!(
        backend.service().apply(
            &mut registry,
            &lease,
            competing_plan,
            &policy,
            &competing_approval
        ),
        Err(DockerGcError::Registry(
            RegistryError::DockerGcRecoveryPending
        ))
    ));
    assert_eq!(
        backend.calls,
        FixtureCalls {
            // The competing apply checks the controller before attempting the claim.
            observations: after_recovery.observations + 1,
            ..after_recovery
        }
    );
    assert_receipt_missing(&backend);
    let (status, claimed, bytes, events) = ledger(&backend, plan);
    assert_eq!(status, "recovery_required");
    assert!(claimed);
    assert_eq!(bytes, Some(0));
    assert_eq!(
        events,
        [
            "PLAN_SAVED",
            "GC_APPROVED",
            "GC_APPLY_INTENT",
            "GC_RECOVERY_REQUIRED"
        ]
    );
    let (competing_status, competing_claimed, competing_bytes, competing_events) =
        ledger(&backend, competing_plan);
    assert_eq!(competing_status, "approved");
    assert!(!competing_claimed);
    assert_eq!(competing_bytes, None);
    assert_eq!(competing_events, ["PLAN_SAVED", "GC_APPROVED"]);
    registry
        .finish_lease(lease, AuditOutcome::Success, "FIXTURE_LEASE_CLOSED")
        .expect("close isolated trial lease");
    let mut result = json!({
        "status": status,
        "reclaimed_bytes": bytes,
        "receipt_present": false,
        "intent_events": events.iter().filter(|event| *event == "GC_APPLY_INTENT").count(),
        "recovery_required_events": events.iter().filter(|event| *event == "GC_RECOVERY_REQUIRED").count(),
        "mutation_calls": backend.calls.mutations,
        "guest_mutation_calls": backend.calls.guest_mutations,
        "guest_observation_calls": backend.calls.guest_observations,
        "recovery_receipt_reads": after_recovery.receipt_reads - before_recovery.receipt_reads,
        "recovery_observation_calls": after_recovery.observations - before_recovery.observations,
        "recovery_mutation_calls": after_recovery.mutations - before_recovery.mutations,
        "approval_reuse_rejected": true,
        "competing_approval_claim_rejected": true,
    });
    result
        .as_object_mut()
        .expect("encode recovery result")
        .extend(witness.as_object().expect("encode witness fields").clone());
    println!("{result_label} {result}");
}
