use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::policy::is_direct_child;

const DATABASE_NAME: &str = "registry.sqlite3";
const APPLICATION_ID: i64 = 0x4453_4732;
const SCHEMA_VERSION: i64 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const OPEN_RETRY_WINDOW: Duration = Duration::from_secs(3);
const MAX_REASON_CODE_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 200;
const MAX_PLAN_BYTES: usize = 1024 * 1024;
const LEASE_SLOTS: i64 = 2;
const MAX_LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_APPROVAL_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    None,
    Active,
    ExpiredUnreleased,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    AwaitingApproval,
    ApprovedRecorded,
    Expired,
    PolicyChanged,
    OwnershipChanged,
    Applying,
    RecoveryRequired,
    Applied,
    Preserved,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateObservation {
    Execution {
        status: ExecutionState,
        observed_at_ms: i64,
        heartbeat_at_ms: Option<i64>,
        expires_at_ms: Option<i64>,
    },
    Approval {
        status: ApprovalState,
        observed_at_ms: i64,
        expires_at_ms: Option<i64>,
    },
}

const INITIAL_SCHEMA: &str = "
    CREATE TABLE project (
        id INTEGER PRIMARY KEY,
        root_path TEXT NOT NULL UNIQUE,
        label TEXT NOT NULL,
        policy_version INTEGER NOT NULL CHECK (policy_version > 0),
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL
    );
    CREATE TABLE gc_plan (
        id INTEGER PRIMARY KEY,
        project_id INTEGER NOT NULL REFERENCES project(id),
        policy_digest TEXT NOT NULL,
        payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL CHECK (expires_at > created_at),
        status TEXT NOT NULL DEFAULT 'planned'
    );
    CREATE TABLE audit_event (
        id INTEGER PRIMARY KEY,
        project_id INTEGER REFERENCES project(id),
        plan_id INTEGER REFERENCES gc_plan(id),
        actor TEXT NOT NULL DEFAULT 'local_cli',
        action TEXT NOT NULL,
        outcome TEXT NOT NULL,
        reason_code TEXT NOT NULL,
        timestamp INTEGER NOT NULL
    );
";

const LEASE_SCHEMA: &str = "
    CREATE TABLE execution_lease (
        id INTEGER PRIMARY KEY,
        project_id INTEGER NOT NULL REFERENCES project(id),
        scope_key TEXT NOT NULL CHECK (length(scope_key) = 64),
        slot INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 1),
        process_id INTEGER CHECK (process_id BETWEEN 1 AND 4294967295),
        acquired_at_ms INTEGER NOT NULL,
        heartbeat_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > acquired_at_ms),
        released_at_ms INTEGER
    );
    CREATE UNIQUE INDEX active_lease_slot ON execution_lease(slot) WHERE released_at_ms IS NULL;
    CREATE UNIQUE INDEX active_lease_project ON execution_lease(project_id)
        WHERE released_at_ms IS NULL;
    CREATE UNIQUE INDEX active_lease_scope ON execution_lease(scope_key) WHERE released_at_ms IS NULL;
    ALTER TABLE audit_event ADD COLUMN lease_id INTEGER REFERENCES execution_lease(id);
";

const GC_APPROVAL_SCHEMA: &str = "
    CREATE TABLE gc_output (
        project_id INTEGER NOT NULL REFERENCES project(id),
        relative_path TEXT NOT NULL,
        identity TEXT NOT NULL CHECK (length(identity) = 64),
        registered_at INTEGER NOT NULL,
        PRIMARY KEY (project_id, relative_path)
    );
    CREATE TABLE gc_application (
        plan_id INTEGER PRIMARY KEY REFERENCES gc_plan(id),
        relative_path TEXT NOT NULL,
        identity TEXT NOT NULL CHECK (length(identity) = 64),
        token_digest TEXT NOT NULL UNIQUE CHECK (length(token_digest) = 64),
        approved_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > approved_at_ms),
        claimed_at_ms INTEGER,
        bytes_applied INTEGER CHECK (bytes_applied >= 0)
    );
";

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("레지스트리 상태 경로를 준비할 수 없습니다")]
    StateIo(#[source] io::Error),
    #[error("레지스트리 상태 경로가 일반 파일 또는 쓰기가 제한된 디렉터리가 아닙니다")]
    UnsafeStatePath,
    #[error("DSG 레지스트리로 초기화되지 않은 상태 DB입니다")]
    UnrecognizedDatabase,
    #[error("지원하지 않는 레지스트리 스키마 버전입니다: {0}")]
    UnsupportedSchema(i64),
    #[error("상태 DB에 SQLite WAL을 설정할 수 없습니다")]
    WalUnavailable,
    #[error("레지스트리 SQLite 작업에 실패했습니다")]
    Sqlite(#[from] rusqlite::Error),
    #[error("프로젝트 루트에 접근할 수 없습니다")]
    ProjectIo(#[source] io::Error),
    #[error("프로젝트 루트가 UTF-8 경로의 디렉터리가 아닙니다")]
    InvalidProjectRoot,
    #[error("프로젝트 이름 또는 정책 버전이 유효하지 않습니다")]
    InvalidProjectMetadata,
    #[error("이 경로는 다른 프로젝트 메타데이터로 이미 등록되어 있습니다")]
    RegistrationConflict,
    #[error("등록되지 않은 프로젝트입니다")]
    ProjectNotFound,
    #[error("감사 사유 코드는 대문자, 숫자, 밑줄만 허용합니다")]
    InvalidReasonCode,
    #[error("계획 정책 다이제스트는 64자의 SHA-256 16진수여야 합니다")]
    InvalidPolicyDigest,
    #[error("계획은 1 MiB 이하의 JSON 객체여야 합니다")]
    InvalidPlanPayload,
    #[error("계획 유효 기간이 유효하지 않습니다")]
    InvalidPlanTtl,
    #[error("시스템 시간을 상태 DB에 기록할 수 없습니다")]
    Clock,
    #[error("실행 lease 범위는 64자의 SHA-256 16진수여야 합니다")]
    InvalidLeaseScope,
    #[error("실행 lease 유효 기간은 1밀리초 이상 24시간 이하여야 합니다")]
    InvalidLeaseTtl,
    #[error("이 프로젝트 또는 실행 범위에 활성 lease가 있습니다")]
    LeaseConflict,
    #[error("관리형 실행의 전역 lease 슬롯을 모두 사용 중입니다")]
    LeaseCapacity,
    #[error("유효하지 않거나 이미 다른 프로세스가 연결된 실행 lease입니다")]
    InvalidLeaseProcess,
    #[error("이 레지스트리가 해당 활성 lease의 소유권을 확인할 수 없습니다")]
    InvalidLeaseOwner,
    #[error("실행 lease 슬롯의 파일 잠금에 실패했습니다")]
    LeaseLock(#[source] io::Error),
    #[error("프로젝트 소유 정리 계획을 찾을 수 없습니다")]
    GcPlanNotFound,
    #[error("정리 대상 이름 또는 파일 식별 다이제스트가 유효하지 않습니다")]
    InvalidGcOutput,
    #[error("정리 대상의 등록된 소유권을 확인할 수 없습니다")]
    GcOwnershipConflict,
    #[error("정리 대상이 계획의 후보와 일치하지 않습니다")]
    GcCandidateMismatch,
    #[error("정리 계획이 현재 정책과 일치하지 않습니다")]
    GcPolicyChanged,
    #[error("정리 계획 또는 승인이 만료되었습니다")]
    GcExpired,
    #[error("정리 승인 증명을 확인할 수 없습니다")]
    GcApprovalInvalid,
    #[error("정리 계획의 현재 상태에서는 이 작업을 할 수 없습니다")]
    GcStateConflict,
    #[error("정리 승인 유효 기간이 지원되는 범위를 벗어났습니다")]
    InvalidGcApprovalTtl,
    #[error("적용 바이트 수를 기록할 수 없습니다")]
    InvalidGcBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub id: i64,
    pub root_path: String,
    pub label: String,
    pub policy_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredPlan {
    pub plan_id: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub struct GcClaim {
    pub plan_id: i64,
    pub relative_path: String,
    pub identity: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GcApplyStatus {
    Applied,
    Preserved,
    RecoveryRequired,
}

impl GcApplyStatus {
    fn audit_fields(self) -> (&'static str, AuditOutcome, &'static str) {
        match self {
            Self::Applied => ("applied", AuditOutcome::Success, "GC_APPLIED"),
            Self::Preserved => ("preserved", AuditOutcome::Success, "GC_PRESERVED"),
            Self::RecoveryRequired => (
                "recovery_required",
                AuditOutcome::Incomplete,
                "GC_RECOVERY_REQUIRED",
            ),
        }
    }
}

pub struct Lease {
    pub id: i64,
    pub project_id: i64,
    state_dir: PathBuf,
    slot: i64,
    _slot_lock: File,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuditAction {
    ProjectRegister,
    PolicyCheck,
    Scan,
    GcPlan,
    GcOutputRegister,
    GcApprove,
    GcApply,
    ManagedExecution,
    ComposeReuse,
}

impl AuditAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProjectRegister => "project_register",
            Self::PolicyCheck => "policy_check",
            Self::Scan => "scan",
            Self::GcPlan => "gc_plan",
            Self::GcOutputRegister => "gc_output_register",
            Self::GcApprove => "gc_approve",
            Self::GcApply => "gc_apply",
            Self::ManagedExecution => "managed_execution",
            Self::ComposeReuse => "compose_reuse",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuditOutcome {
    Success,
    Allow,
    Warn,
    Deny,
    ApprovalRequired,
    Incomplete,
    Failure,
}

impl AuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::ApprovalRequired => "approval_required",
            Self::Incomplete => "incomplete",
            Self::Failure => "failure",
        }
    }
}

pub struct Registry {
    connection: Connection,
    state_dir: PathBuf,
}

/// Observe the registered project under a process-level filesystem write denial.
/// The SQLite read-only flag alone does not prevent WAL sidecar operations.
#[cfg(target_os = "macos")]
pub fn observe_state(
    state_dir: &Path,
    root: &Path,
    label: &str,
    policy_version: u32,
    policy_digest: &str,
    plan_id: Option<i64>,
) -> Result<StateObservation, RegistryError> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

    let metadata = fs::symlink_metadata(state_dir).map_err(RegistryError::StateIo)?;
    let owner = unsafe { libc::geteuid() };
    if owner == 0 || metadata.uid() != owner || !metadata.is_dir() || is_shared_writable(&metadata)
    {
        return Err(RegistryError::UnsafeStatePath);
    }
    let dir = state_dir.canonicalize().map_err(RegistryError::StateIo)?;
    let database = dir.join(DATABASE_NAME);
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let path = dir.join(format!("{DATABASE_NAME}{suffix}"));
        validate_state_file(&path)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.uid() == owner => {}
            Err(error) if !suffix.is_empty() && error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(RegistryError::UnsafeStatePath),
        }
    }

    let mut uri = String::from("file:");
    for &byte in database.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(uri, "%{byte:02X}").expect("writing to a String");
        }
    }
    uri.push_str("?mode=ro&readonly_shm=1");
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut connection = Connection::open_with_flags(uri, flags)?;
    connection.busy_timeout(Duration::from_millis(150))?;
    connection.pragma_update(None, "trusted_schema", false)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "query_only", true)?;
    let snapshot = connection.transaction()?;
    let application_id: i64 =
        snapshot.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(RegistryError::UnrecognizedDatabase);
    }
    let version: i64 = snapshot.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(RegistryError::UnsupportedSchema(version));
    }
    let mode: String = snapshot.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(RegistryError::WalUnavailable);
    }
    observe_snapshot(
        &snapshot,
        root,
        label,
        policy_version,
        policy_digest,
        plan_id,
    )
}

#[cfg(any(target_os = "macos", test))]
fn observe_snapshot(
    snapshot: &Connection,
    root: &Path,
    label: &str,
    policy_version: u32,
    policy_digest: &str,
    plan_id: Option<i64>,
) -> Result<StateObservation, RegistryError> {
    let root = root.to_str().ok_or(RegistryError::InvalidProjectRoot)?;
    let project_id: i64 = snapshot
        .query_row(
            "SELECT id FROM project WHERE root_path = ?1 AND label = ?2 AND policy_version = ?3",
            params![root, label, policy_version],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(RegistryError::ProjectNotFound)?;
    let observed_at_ms = lease_now_ms()?;
    if let Some(plan_id) = plan_id {
        let plan = snapshot
            .query_row(
                "SELECT p.status, p.expires_at, p.policy_digest = ?3,
                        a.expires_at_ms, a.claimed_at_ms IS NOT NULL,
                        EXISTS (SELECT 1 FROM gc_output o WHERE o.project_id = p.project_id
                                AND o.relative_path = a.relative_path AND o.identity = a.identity)
                 FROM gc_plan p LEFT JOIN gc_application a ON a.plan_id = p.id
                 WHERE p.id = ?1 AND p.project_id = ?2",
                params![plan_id, project_id, policy_digest],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(RegistryError::GcPlanNotFound)?;
        let (state, plan_expiry, same_policy, approval_expiry, claimed, owned) = plan;
        let plan_expiry = plan_expiry.saturating_mul(1000);
        let expiry = approval_expiry.map_or(plan_expiry, |approval| approval.min(plan_expiry));
        let status = match (state.as_str(), approval_expiry, claimed) {
            ("applying", Some(_), true) => ApprovalState::Applying,
            ("recovery_required", Some(_), true) => ApprovalState::RecoveryRequired,
            ("applied", Some(_), true) => ApprovalState::Applied,
            ("preserved", Some(_), true) => ApprovalState::Preserved,
            ("planned", None, false) | ("approved", Some(_), false) if !same_policy => {
                ApprovalState::PolicyChanged
            }
            ("planned", None, false) | ("approved", Some(_), false) if expiry <= observed_at_ms => {
                ApprovalState::Expired
            }
            ("planned", None, false) => ApprovalState::AwaitingApproval,
            ("approved", Some(_), false) if !owned => ApprovalState::OwnershipChanged,
            ("approved", Some(_), false) => ApprovalState::ApprovedRecorded,
            _ => return Err(RegistryError::GcStateConflict),
        };
        return Ok(StateObservation::Approval {
            status,
            observed_at_ms,
            expires_at_ms: Some(expiry),
        });
    }
    let lease = snapshot
        .query_row(
            "SELECT heartbeat_at_ms, expires_at_ms FROM execution_lease
             WHERE project_id = ?1 AND released_at_ms IS NULL",
            [project_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let status = match lease {
        Some((_, expiry)) if expiry > observed_at_ms => ExecutionState::Active,
        Some(_) => ExecutionState::ExpiredUnreleased,
        None => ExecutionState::None,
    };
    Ok(StateObservation::Execution {
        status,
        observed_at_ms,
        heartbeat_at_ms: lease.map(|(heartbeat, _)| heartbeat),
        expires_at_ms: lease.map(|(_, expiry)| expiry),
    })
}

impl Registry {
    pub fn open(state_dir: &Path) -> Result<Self, RegistryError> {
        let state_dir: PathBuf = state_dir.components().collect();
        prepare_state_directory(&state_dir)?;
        let state_dir = state_dir.canonicalize().map_err(RegistryError::StateIo)?;
        let database_path = state_dir.join(DATABASE_NAME);
        let start = Instant::now();
        loop {
            validate_state_file(&database_path)?;
            for suffix in ["-journal", "-wal", "-shm"] {
                validate_state_file(&state_dir.join(format!("{DATABASE_NAME}{suffix}")))?;
            }
            create_state_file(&database_path)?;
            match Self::open_database(&database_path) {
                Err(RegistryError::Sqlite(error))
                    if matches!(
                        error.sqlite_error_code(),
                        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                    ) && start.elapsed() < OPEN_RETRY_WINDOW =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                result => {
                    return result.map(|connection| Self {
                        connection,
                        state_dir,
                    });
                }
            }
        }
    }

    fn open_database(database_path: &Path) -> Result<Connection, RegistryError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(database_path, flags)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        migrate(&mut connection)?;
        let mode: String =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(RegistryError::WalUnavailable);
        }
        #[cfg(target_os = "macos")]
        {
            // Keep SQLite's sidecars after checkpoint so the sandboxed
            // observer can open an idle registry without creating any files.
            let mut keep: libc::c_int = 1;
            let status = unsafe {
                rusqlite::ffi::sqlite3_file_control(
                    connection.handle(),
                    c"main".as_ptr(),
                    rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
                    std::ptr::from_mut(&mut keep).cast(),
                )
            };
            if status != rusqlite::ffi::SQLITE_OK {
                return Err(RegistryError::WalUnavailable);
            }
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }

    pub fn register(
        &mut self,
        root: &Path,
        label: &str,
        policy_version: u32,
    ) -> Result<Project, RegistryError> {
        if label.trim().is_empty()
            || label.len() > MAX_LABEL_BYTES
            || label.chars().any(char::is_control)
            || policy_version == 0
        {
            return Err(RegistryError::InvalidProjectMetadata);
        }
        let root_path = canonical_root(root)?;
        let timestamp = timestamp()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(project) = project_by_root(&tx, &root_path)? {
            if project.label != label || project.policy_version != policy_version {
                return Err(RegistryError::RegistrationConflict);
            }
            tx.execute(
                "UPDATE project SET last_seen_at = MAX(last_seen_at, ?1) WHERE id = ?2",
                params![timestamp, project.id],
            )?;
            tx.commit()?;
            return Ok(project);
        }

        tx.execute(
            "INSERT INTO project (root_path, label, policy_version, created_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![root_path, label, policy_version, timestamp],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            Some(id),
            EventReference::None,
            AuditAction::ProjectRegister,
            AuditOutcome::Success,
            "PROJECT_REGISTERED",
            timestamp,
        )?;
        tx.commit()?;
        Ok(Project {
            id,
            root_path,
            label: label.to_owned(),
            policy_version,
        })
    }

    pub fn find_project(&self, root: &Path) -> Result<Option<Project>, RegistryError> {
        let root_path = canonical_root(root)?;
        project_by_root(&self.connection, &root_path)
    }

    pub fn record_event(
        &mut self,
        project_id: Option<i64>,
        action: AuditAction,
        outcome: AuditOutcome,
        reason_code: &str,
    ) -> Result<(), RegistryError> {
        let timestamp = timestamp()?;
        insert_event(
            &self.connection,
            project_id,
            EventReference::None,
            action,
            outcome,
            reason_code,
            timestamp,
        )
    }

    pub fn save_plan(
        &mut self,
        project_id: i64,
        policy_digest: &str,
        payload: &Value,
        ttl: Duration,
    ) -> Result<StoredPlan, RegistryError> {
        if policy_digest.len() != 64 || !policy_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(RegistryError::InvalidPolicyDigest);
        }
        if !payload.is_object() {
            return Err(RegistryError::InvalidPlanPayload);
        }
        let payload_json =
            serde_json::to_string(payload).map_err(|_| RegistryError::InvalidPlanPayload)?;
        if payload_json.len() > MAX_PLAN_BYTES {
            return Err(RegistryError::InvalidPlanPayload);
        }
        let ttl = i64::try_from(ttl.as_secs()).map_err(|_| RegistryError::InvalidPlanTtl)?;
        let created_at = timestamp()?;
        let expires_at = created_at
            .checked_add(ttl)
            .filter(|expires_at| *expires_at > created_at)
            .ok_or(RegistryError::InvalidPlanTtl)?;

        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM project WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(RegistryError::ProjectNotFound);
        }
        tx.execute(
            "INSERT INTO gc_plan (project_id, policy_digest, payload_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project_id,
                policy_digest.to_ascii_lowercase(),
                payload_json,
                created_at,
                expires_at,
            ],
        )?;
        let plan_id = tx.last_insert_rowid();
        insert_event(
            &tx,
            Some(project_id),
            EventReference::Plan(plan_id),
            AuditAction::GcPlan,
            AuditOutcome::Success,
            "PLAN_SAVED",
            created_at,
        )?;
        tx.commit()?;
        Ok(StoredPlan {
            plan_id,
            created_at,
            expires_at,
        })
    }

    pub fn register_gc_output(
        &mut self,
        project_id: i64,
        relative: &str,
        identity: &str,
    ) -> Result<(), RegistryError> {
        let identity = gc_output_identity(relative, identity)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = gc_registered_identity(&tx, project_id, relative)? {
            return if previous == identity {
                tx.commit()?;
                Ok(())
            } else {
                Err(RegistryError::GcOwnershipConflict)
            };
        }
        let now = timestamp()?;
        let updated = tx.execute(
            "INSERT INTO gc_output (project_id, relative_path, identity, registered_at)
             SELECT id, ?2, ?3, ?4 FROM project WHERE id = ?1",
            params![project_id, relative, identity, now],
        )?;
        if updated != 1 {
            return Err(RegistryError::ProjectNotFound);
        }
        insert_event(
            &tx,
            Some(project_id),
            EventReference::None,
            AuditAction::GcOutputRegister,
            AuditOutcome::Success,
            "GC_OUTPUT_REGISTERED",
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn read_gc_plan(&self, project_id: i64, plan_id: i64) -> Result<Value, RegistryError> {
        gc_plan_row(&self.connection, project_id, plan_id)?.payload()
    }

    pub fn read_current_gc_plan(
        &self,
        project_id: i64,
        plan_id: i64,
        policy_digest: &str,
    ) -> Result<Value, RegistryError> {
        let plan = gc_plan_row(&self.connection, project_id, plan_id)?;
        plan.require_current(policy_digest, lease_now_ms()?)?;
        if plan.status != "planned" {
            return Err(RegistryError::GcStateConflict);
        }
        plan.payload()
    }

    pub fn has_open_execution_lease(&self) -> Result<bool, RegistryError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_lease WHERE released_at_ms IS NULL)",
            [],
            |row| row.get(0),
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn approve_gc(
        &mut self,
        project_id: i64,
        plan_id: i64,
        relative: &str,
        policy_digest: &str,
        identity: &str,
        token_digest: &str,
        ttl: Duration,
    ) -> Result<(), RegistryError> {
        let identity = gc_output_identity(relative, identity)?;
        let policy_digest =
            normalized_digest(policy_digest).ok_or(RegistryError::InvalidPolicyDigest)?;
        let token_digest =
            normalized_digest(token_digest).ok_or(RegistryError::GcApprovalInvalid)?;
        if ttl < Duration::from_millis(1) || ttl > MAX_APPROVAL_TTL {
            return Err(RegistryError::InvalidGcApprovalTtl);
        }
        let ttl_ms =
            i64::try_from(ttl.as_millis()).map_err(|_| RegistryError::InvalidGcApprovalTtl)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now_ms = lease_now_ms()?;
        let plan = gc_plan_row(&tx, project_id, plan_id)?;
        let plan_expires_at_ms = plan.require_current(&policy_digest, now_ms)?;
        if plan.status != "planned" {
            return Err(RegistryError::GcStateConflict);
        }
        require_gc_ownership(&tx, project_id, relative, &identity)?;
        require_gc_candidate(&plan.payload()?, relative, &identity)?;
        let duplicate_token: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM gc_application WHERE token_digest = ?1)",
            [&token_digest],
            |row| row.get(0),
        )?;
        if duplicate_token {
            return Err(RegistryError::GcApprovalInvalid);
        }
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(RegistryError::InvalidGcApprovalTtl)?
            .min(plan_expires_at_ms);
        tx.execute(
            "INSERT INTO gc_application
             (plan_id, relative_path, identity, token_digest, approved_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                plan_id,
                relative,
                identity,
                token_digest,
                now_ms,
                expires_at_ms
            ],
        )?;
        set_gc_status(&tx, plan_id, "approved")?;
        insert_event(
            &tx,
            Some(project_id),
            EventReference::Plan(plan_id),
            AuditAction::GcApprove,
            AuditOutcome::Success,
            "GC_APPROVED",
            now_ms / 1000,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn claim_gc(
        &mut self,
        lease: &Lease,
        plan_id: i64,
        policy_digest: &str,
        token_digest: &str,
    ) -> Result<GcClaim, RegistryError> {
        self.validate_lease_owner(lease)?;
        let token_digest =
            normalized_digest(token_digest).ok_or(RegistryError::GcApprovalInvalid)?;
        let policy_digest =
            normalized_digest(policy_digest).ok_or(RegistryError::InvalidPolicyDigest)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_lease(&tx, lease)?;
        let now_ms = lease_now_ms()?;
        let plan = gc_plan_row(&tx, lease.project_id, plan_id)?;
        if plan.status != "approved" {
            return Err(RegistryError::GcStateConflict);
        }
        plan.require_current(&policy_digest, now_ms)?;
        let approval = gc_application_row(&tx, plan_id)?;
        if approval.claimed || approval.token_digest != token_digest {
            return Err(RegistryError::GcApprovalInvalid);
        }
        if approval.expires_at_ms <= now_ms {
            return Err(RegistryError::GcExpired);
        }
        let claim = approval.into_claim(plan_id, plan.payload()?);
        require_gc_ownership(&tx, lease.project_id, &claim.relative_path, &claim.identity)?;
        require_gc_candidate(&claim.payload, &claim.relative_path, &claim.identity)?;
        let claimed = tx.execute(
            "UPDATE gc_application SET claimed_at_ms = ?1
             WHERE plan_id = ?2 AND claimed_at_ms IS NULL",
            params![now_ms, plan_id],
        )?;
        if claimed != 1 {
            return Err(RegistryError::GcApprovalInvalid);
        }
        set_gc_status(&tx, plan_id, "applying")?;
        insert_event(
            &tx,
            Some(lease.project_id),
            EventReference::PlanLease(plan_id, lease.id),
            AuditAction::GcApply,
            AuditOutcome::Allow,
            "GC_APPLY_INTENT",
            now_ms / 1000,
        )?;
        tx.commit()?;
        Ok(claim)
    }

    pub fn pending_gc(&self, lease: &Lease, plan_id: i64) -> Result<GcClaim, RegistryError> {
        self.validate_lease_owner(lease)?;
        require_active_lease(&self.connection, lease)?;
        pending_gc_claim(&self.connection, lease.project_id, plan_id)
    }

    pub fn finish_gc(
        &mut self,
        lease: &Lease,
        plan_id: i64,
        status: GcApplyStatus,
        bytes: u64,
    ) -> Result<(), RegistryError> {
        self.validate_lease_owner(lease)?;
        let bytes = i64::try_from(bytes).map_err(|_| RegistryError::InvalidGcBytes)?;
        if status == GcApplyStatus::Preserved && bytes != 0 {
            return Err(RegistryError::InvalidGcBytes);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_lease(&tx, lease)?;
        pending_gc_claim(&tx, lease.project_id, plan_id)?;
        let (status, outcome, reason_code) = status.audit_fields();
        let now = timestamp()?;
        let updated = tx.execute(
            "UPDATE gc_application SET bytes_applied = MAX(COALESCE(bytes_applied, 0), ?1)
             WHERE plan_id = ?2 AND claimed_at_ms IS NOT NULL",
            params![bytes, plan_id],
        )?;
        if updated != 1 {
            return Err(RegistryError::GcApprovalInvalid);
        }
        set_gc_status(&tx, plan_id, status)?;
        insert_event(
            &tx,
            Some(lease.project_id),
            EventReference::PlanLease(plan_id, lease.id),
            AuditAction::GcApply,
            outcome,
            reason_code,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The bounded reclaim probe must return true only when the recorded process scope has ended.
    pub fn acquire_lease(
        &mut self,
        project_id: i64,
        scope_key: &str,
        ttl: Duration,
        can_reclaim: impl Fn(u32) -> bool,
    ) -> Result<Lease, RegistryError> {
        if scope_key.len() != 64 || !scope_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RegistryError::InvalidLeaseScope);
        }
        let scope_key = scope_key.to_ascii_lowercase();
        let ttl_ms = lease_ttl_ms(ttl)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM project WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(RegistryError::ProjectNotFound);
        }
        let now_ms = lease_now_ms()?;
        let mut available = None;
        for slot in 0..LEASE_SLOTS {
            let Some(lock) = try_lock_slot(&self.state_dir, slot)? else {
                continue;
            };
            let current = tx
                .query_row(
                    "SELECT id, project_id, process_id, expires_at_ms FROM execution_lease
                     WHERE slot = ?1 AND released_at_ms IS NULL",
                    [slot],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<u32>>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((id, owner, process_id, expires_at_ms)) = current {
                // Without a registered PID, the start/attach gap is ambiguous.
                if expires_at_ms > now_ms || !process_id.is_some_and(&can_reclaim) {
                    continue;
                }
                release_lease(
                    &tx,
                    (id, owner, slot),
                    AuditOutcome::Incomplete,
                    "LEASE_RECLAIMED",
                    now_ms,
                )?;
            }
            if available.is_none() {
                available = Some((slot, lock));
            }
        }
        let conflicts: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_lease
             WHERE released_at_ms IS NULL AND (project_id = ?1 OR scope_key = ?2))",
            params![project_id, scope_key],
            |row| row.get(0),
        )?;
        if conflicts {
            tx.commit()?;
            return Err(RegistryError::LeaseConflict);
        }
        let Some((slot, lock)) = available else {
            tx.commit()?;
            return Err(RegistryError::LeaseCapacity);
        };
        let acquired_at_ms = lease_now_ms()?;
        let expires_at_ms = lease_expiry(acquired_at_ms, ttl_ms)?;
        tx.execute(
            "INSERT INTO execution_lease
             (project_id, scope_key, slot, acquired_at_ms, heartbeat_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![project_id, scope_key, slot, acquired_at_ms, expires_at_ms],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Lease {
            id,
            project_id,
            state_dir: self.state_dir.clone(),
            slot,
            _slot_lock: lock,
        })
    }

    pub fn attach_lease_process(&mut self, lease: &Lease, pid: u32) -> Result<(), RegistryError> {
        self.validate_lease_owner(lease)?;
        if pid == 0 {
            return Err(RegistryError::InvalidLeaseProcess);
        }
        let updated = self.connection.execute(
            "UPDATE execution_lease SET process_id = ?1
             WHERE id = ?2 AND project_id = ?3 AND slot = ?4
             AND released_at_ms IS NULL AND (process_id IS NULL OR process_id = ?1)",
            params![pid, lease.id, lease.project_id, lease.slot],
        )?;
        if updated != 1 {
            return Err(RegistryError::InvalidLeaseProcess);
        }
        Ok(())
    }

    pub fn heartbeat(&mut self, lease: &Lease, ttl: Duration) -> Result<(), RegistryError> {
        self.validate_lease_owner(lease)?;
        let now_ms = lease_now_ms()?;
        let expires_at_ms = lease_expiry(now_ms, lease_ttl_ms(ttl)?)?;
        let updated = self.connection.execute(
            "UPDATE execution_lease
             SET heartbeat_at_ms = MAX(heartbeat_at_ms, ?1), expires_at_ms = MAX(expires_at_ms, ?2)
             WHERE id = ?3 AND project_id = ?4 AND slot = ?5 AND released_at_ms IS NULL",
            params![
                now_ms,
                expires_at_ms,
                lease.id,
                lease.project_id,
                lease.slot
            ],
        )?;
        if updated != 1 {
            return Err(RegistryError::InvalidLeaseOwner);
        }
        Ok(())
    }

    /// Call after the process scope has ended, or before any process has started.
    pub fn finish_lease(
        &mut self,
        lease: Lease,
        outcome: AuditOutcome,
        reason_code: &str,
    ) -> Result<(), RegistryError> {
        self.validate_lease_owner(&lease)?;
        let now_ms = lease_now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        release_lease(
            &tx,
            (lease.id, lease.project_id, lease.slot),
            outcome,
            reason_code,
            now_ms,
        )?;
        tx.commit()?;
        drop(lease);
        Ok(())
    }

    fn validate_lease_owner(&self, lease: &Lease) -> Result<(), RegistryError> {
        if self.state_dir != lease.state_dir {
            return Err(RegistryError::InvalidLeaseOwner);
        }
        Ok(())
    }
}

fn prepare_state_directory(path: &Path) -> Result<(), RegistryError> {
    if path.as_os_str().is_empty() {
        return Err(RegistryError::UnsafeStatePath);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(RegistryError::StateIo)?;
        }
        Err(error) => return Err(RegistryError::StateIo(error)),
    }
    let metadata = fs::symlink_metadata(path).map_err(RegistryError::StateIo)?;
    if !metadata.is_dir() || is_link(&metadata) || is_shared_writable(&metadata) {
        return Err(RegistryError::UnsafeStatePath);
    }
    Ok(())
}

fn validate_state_file(path: &Path) -> Result<(), RegistryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || is_link(&metadata)
                || is_shared_writable(&metadata)
                || has_multiple_links(&metadata)
            {
                return Err(RegistryError::UnsafeStatePath);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(RegistryError::StateIo(error)),
    }
    Ok(())
}

fn create_state_file(path: &Path) -> Result<(), RegistryError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => validate_state_file(path),
        Err(error) => Err(RegistryError::StateIo(error)),
    }
}

fn try_lock_slot(state_dir: &Path, slot: i64) -> Result<Option<File>, RegistryError> {
    let path = state_dir.join(format!("execution-slot-{slot}.lock"));
    validate_state_file(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(RegistryError::LeaseLock)?;
    let metadata = file.metadata().map_err(RegistryError::LeaseLock)?;
    if !metadata.is_file()
        || is_link(&metadata)
        || is_shared_writable(&metadata)
        || has_multiple_links(&metadata)
    {
        return Err(RegistryError::UnsafeStatePath);
    }
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(None)
        }
        Err(error) => Err(RegistryError::LeaseLock(error)),
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn is_shared_writable(_metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _metadata.permissions().mode() & 0o022 != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn has_multiple_links(_metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        _metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn migrate(connection: &mut Connection) -> Result<(), RegistryError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application_id: i64 = tx.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match (application_id, version) {
        (APPLICATION_ID, SCHEMA_VERSION) => {
            tx.commit()?;
            return Ok(());
        }
        (APPLICATION_ID, 1 | 2) => {}
        (APPLICATION_ID, version) => return Err(RegistryError::UnsupportedSchema(version)),
        (0, 0) => {
            let has_objects: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
                    row.get(0)
                })?;
            if has_objects {
                return Err(RegistryError::UnrecognizedDatabase);
            }
            tx.execute_batch(INITIAL_SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        }
        _ => return Err(RegistryError::UnrecognizedDatabase),
    }
    if version < 2 {
        tx.execute_batch(LEASE_SCHEMA)?;
    }
    tx.execute_batch(GC_APPROVAL_SCHEMA)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn canonical_root(root: &Path) -> Result<String, RegistryError> {
    let root = root.canonicalize().map_err(RegistryError::ProjectIo)?;
    if !root.metadata().map_err(RegistryError::ProjectIo)?.is_dir() {
        return Err(RegistryError::InvalidProjectRoot);
    }
    root.into_os_string()
        .into_string()
        .map_err(|_| RegistryError::InvalidProjectRoot)
}

fn project_by_root(connection: &Connection, root: &str) -> Result<Option<Project>, RegistryError> {
    Ok(connection
        .query_row(
            "SELECT id, root_path, label, policy_version FROM project WHERE root_path = ?1",
            [root],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    root_path: row.get(1)?,
                    label: row.get(2)?,
                    policy_version: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn timestamp() -> Result<i64, RegistryError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RegistryError::Clock)?;
    i64::try_from(elapsed.as_secs()).map_err(|_| RegistryError::Clock)
}

fn lease_now_ms() -> Result<i64, RegistryError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RegistryError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| RegistryError::Clock)
}

fn lease_ttl_ms(ttl: Duration) -> Result<i64, RegistryError> {
    if ttl < Duration::from_millis(1) || ttl > MAX_LEASE_TTL {
        return Err(RegistryError::InvalidLeaseTtl);
    }
    i64::try_from(ttl.as_millis()).map_err(|_| RegistryError::InvalidLeaseTtl)
}

fn lease_expiry(now_ms: i64, ttl_ms: i64) -> Result<i64, RegistryError> {
    now_ms
        .checked_add(ttl_ms)
        .ok_or(RegistryError::InvalidLeaseTtl)
}

fn release_lease(
    transaction: &rusqlite::Transaction<'_>,
    (id, project_id, slot): (i64, i64, i64),
    outcome: AuditOutcome,
    reason_code: &str,
    now_ms: i64,
) -> Result<(), RegistryError> {
    let updated = transaction.execute(
        "UPDATE execution_lease SET released_at_ms = MAX(acquired_at_ms, ?1)
         WHERE id = ?2 AND project_id = ?3 AND slot = ?4 AND released_at_ms IS NULL",
        params![now_ms, id, project_id, slot],
    )?;
    if updated != 1 {
        return Err(RegistryError::InvalidLeaseOwner);
    }
    insert_event(
        transaction,
        Some(project_id),
        EventReference::Lease(id),
        AuditAction::ManagedExecution,
        outcome,
        reason_code,
        now_ms / 1000,
    )
}

fn require_active_lease(connection: &Connection, lease: &Lease) -> Result<(), RegistryError> {
    let active: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM execution_lease WHERE
            id = ?1 AND project_id = ?2 AND slot = ?3 AND released_at_ms IS NULL)",
        params![lease.id, lease.project_id, lease.slot],
        |row| row.get(0),
    )?;
    if !active {
        return Err(RegistryError::InvalidLeaseOwner);
    }
    Ok(())
}

struct GcPlanRow {
    policy_digest: String,
    payload_json: String,
    expires_at: i64,
    status: String,
}

impl GcPlanRow {
    fn payload(&self) -> Result<Value, RegistryError> {
        serde_json::from_str(&self.payload_json).map_err(|_| RegistryError::InvalidPlanPayload)
    }

    fn require_current(&self, policy_digest: &str, now_ms: i64) -> Result<i64, RegistryError> {
        if self.policy_digest != policy_digest {
            return Err(RegistryError::GcPolicyChanged);
        }
        let expires_at_ms = self.expires_at.saturating_mul(1000);
        if expires_at_ms <= now_ms {
            return Err(RegistryError::GcExpired);
        }
        Ok(expires_at_ms)
    }
}

fn gc_plan_row(
    connection: &Connection,
    project_id: i64,
    plan_id: i64,
) -> Result<GcPlanRow, RegistryError> {
    connection
        .query_row(
            "SELECT policy_digest, payload_json, expires_at, status FROM gc_plan
             WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id],
            |row| {
                Ok(GcPlanRow {
                    policy_digest: row.get(0)?,
                    payload_json: row.get(1)?,
                    expires_at: row.get(2)?,
                    status: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or(RegistryError::GcPlanNotFound)
}

struct GcApprovalRow {
    relative_path: String,
    identity: String,
    token_digest: String,
    expires_at_ms: i64,
    claimed: bool,
}

impl GcApprovalRow {
    fn into_claim(self, plan_id: i64, payload: Value) -> GcClaim {
        GcClaim {
            plan_id,
            relative_path: self.relative_path,
            identity: self.identity,
            payload,
        }
    }
}

fn gc_application_row(
    connection: &Connection,
    plan_id: i64,
) -> Result<GcApprovalRow, RegistryError> {
    connection
        .query_row(
            "SELECT relative_path, identity, token_digest, expires_at_ms, claimed_at_ms IS NOT NULL
             FROM gc_application WHERE plan_id = ?1",
            [plan_id],
            |row| {
                Ok(GcApprovalRow {
                    relative_path: row.get(0)?,
                    identity: row.get(1)?,
                    token_digest: row.get(2)?,
                    expires_at_ms: row.get(3)?,
                    claimed: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or(RegistryError::GcApprovalInvalid)
}

fn pending_gc_claim(
    connection: &Connection,
    project_id: i64,
    plan_id: i64,
) -> Result<GcClaim, RegistryError> {
    let plan = gc_plan_row(connection, project_id, plan_id)?;
    if !matches!(plan.status.as_str(), "applying" | "recovery_required") {
        return Err(RegistryError::GcStateConflict);
    }
    let approval = gc_application_row(connection, plan_id)?;
    if !approval.claimed {
        return Err(RegistryError::GcApprovalInvalid);
    }
    let claim = approval.into_claim(plan_id, plan.payload()?);
    require_gc_ownership(
        connection,
        project_id,
        &claim.relative_path,
        &claim.identity,
    )?;
    require_gc_candidate(&claim.payload, &claim.relative_path, &claim.identity)?;
    Ok(claim)
}

fn set_gc_status(connection: &Connection, plan_id: i64, status: &str) -> Result<(), RegistryError> {
    let updated = connection.execute(
        "UPDATE gc_plan SET status = ?1 WHERE id = ?2",
        params![status, plan_id],
    )?;
    if updated != 1 {
        return Err(RegistryError::GcStateConflict);
    }
    Ok(())
}

fn normalized_digest(digest: &str) -> Option<String> {
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn gc_output_identity(relative: &str, identity: &str) -> Result<String, RegistryError> {
    if !is_direct_child(relative) {
        return Err(RegistryError::InvalidGcOutput);
    }
    normalized_digest(identity).ok_or(RegistryError::InvalidGcOutput)
}

fn gc_registered_identity(
    connection: &Connection,
    project_id: i64,
    relative: &str,
) -> Result<Option<String>, RegistryError> {
    Ok(connection
        .query_row(
            "SELECT identity FROM gc_output WHERE project_id = ?1 AND relative_path = ?2",
            params![project_id, relative],
            |row| row.get(0),
        )
        .optional()?)
}

fn require_gc_ownership(
    connection: &Connection,
    project_id: i64,
    relative: &str,
    identity: &str,
) -> Result<(), RegistryError> {
    if gc_registered_identity(connection, project_id, relative)?.as_deref() != Some(identity) {
        return Err(RegistryError::GcOwnershipConflict);
    }
    Ok(())
}

fn require_gc_candidate(
    payload: &Value,
    relative: &str,
    identity: &str,
) -> Result<(), RegistryError> {
    let candidates = payload["candidates"]
        .as_array()
        .filter(|_| payload["dry_run"].as_bool() == Some(true))
        .ok_or(RegistryError::GcCandidateMismatch)?;
    let mut matches = candidates
        .iter()
        .filter(|candidate| candidate["relative_path"].as_str() == Some(relative));
    let candidate = matches
        .next()
        .filter(|_| matches.next().is_none())
        .ok_or(RegistryError::GcCandidateMismatch)?;
    let clean_identity = candidate["identity"]
        .as_object()
        .filter(|fields| fields.len() == 2)
        .and_then(|fields| {
            let device = fields.get("device")?.as_u64()?;
            let inode = fields.get("inode")?.as_u64()?;
            Some(crate::filesystem::FileIdentity { device, inode })
        })
        .ok_or(RegistryError::GcCandidateMismatch)?;
    let encoded =
        serde_json::to_vec(&clean_identity).map_err(|_| RegistryError::GcCandidateMismatch)?;
    let candidate_digest = format!("{:x}", Sha256::digest(encoded));
    if candidate_digest != identity {
        return Err(RegistryError::GcCandidateMismatch);
    }
    Ok(())
}

enum EventReference {
    None,
    Plan(i64),
    Lease(i64),
    PlanLease(i64, i64),
}

fn insert_event(
    connection: &Connection,
    project_id: Option<i64>,
    reference: EventReference,
    action: AuditAction,
    outcome: AuditOutcome,
    reason_code: &str,
    timestamp: i64,
) -> Result<(), RegistryError> {
    let bytes = reason_code.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_uppercase)
        || bytes.len() > MAX_REASON_CODE_BYTES
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(RegistryError::InvalidReasonCode);
    }
    let (plan_id, lease_id) = match reference {
        EventReference::None => (None, None),
        EventReference::Plan(id) => (Some(id), None),
        EventReference::Lease(id) => (None, Some(id)),
        EventReference::PlanLease(plan, lease) => (Some(plan), Some(lease)),
    };
    connection.execute(
        "INSERT INTO audit_event
         (project_id, plan_id, lease_id, action, outcome, reason_code, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            project_id,
            plan_id,
            lease_id,
            action.as_str(),
            outcome.as_str(),
            reason_code,
            timestamp
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn registration_persists_without_overwriting_or_duplicating_audit() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let mut registry = Registry::open(&state).unwrap();
        assert_eq!(registry.find_project(&root).unwrap(), None);
        let first = registry.register(&root, "project", 2).unwrap();
        assert_eq!(
            registry.register(&root.join("."), "project", 2).unwrap(),
            first
        );
        for (label, version) in [("other", 2), ("project", 3)] {
            assert!(matches!(
                registry.register(&root, label, version),
                Err(RegistryError::RegistrationConflict)
            ));
        }
        drop(registry);

        let reopened = Registry::open(&state).unwrap();
        assert_eq!(reopened.find_project(&root).unwrap(), Some(first));
        let count: i64 = reopened
            .connection
            .query_row("SELECT count(*) FROM audit_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn concurrent_first_opens_and_registrations_share_the_same_wal_state() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let state = state.clone();
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let mut registry = Registry::open(&state).unwrap();
                    let mode: String = registry
                        .connection
                        .pragma_query_value(None, "journal_mode", |row| row.get(0))
                        .unwrap();
                    assert_eq!(mode, "wal");
                    registry.register(&root, "project", 2).unwrap()
                })
            })
            .collect();
        let projects: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(projects.iter().all(|project| project == &projects[0]));
        let registry = Registry::open(&state).unwrap();
        assert_eq!(
            registry.find_project(&root).unwrap(),
            Some(projects[0].clone())
        );
    }

    #[test]
    fn validates_audit_codes_and_records_only_explicit_metadata() {
        let temp = tempdir().unwrap();
        let mut registry = Registry::open(temp.path()).unwrap();
        for reason in ["", "raw command --flag", "a", "_KEY", &"A".repeat(65)] {
            assert!(matches!(
                registry.record_event(None, AuditAction::PolicyCheck, AuditOutcome::Deny, reason),
                Err(RegistryError::InvalidReasonCode)
            ));
        }
        registry
            .record_event(
                None,
                AuditAction::PolicyCheck,
                AuditOutcome::Deny,
                "PROTECTED_VOLUME",
            )
            .unwrap();
        let row: (i64, String, String, String) = registry
            .connection
            .query_row(
                "SELECT count(*), action, outcome, reason_code FROM audit_event",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                1,
                "policy_check".into(),
                "deny".into(),
                "PROTECTED_VOLUME".into()
            )
        );
    }

    #[test]
    fn plan_and_audit_commit_together_with_expiry() {
        let temp = tempdir().unwrap();
        let mut registry = Registry::open(&temp.path().join("state")).unwrap();
        let project = registry.register(temp.path(), "project", 2).unwrap();
        let payload = json!({"candidates": []});
        let digest = "a".repeat(64);
        let ttl = Duration::from_secs(120);
        assert!(matches!(
            registry.save_plan(project.id + 1, &digest, &payload, ttl),
            Err(RegistryError::ProjectNotFound)
        ));
        assert!(matches!(
            registry.save_plan(project.id, &digest, &payload, Duration::ZERO),
            Err(RegistryError::InvalidPlanTtl)
        ));
        let plan = registry
            .save_plan(project.id, &digest, &payload, ttl)
            .unwrap();
        assert_eq!(plan.expires_at - plan.created_at, 120);
        let (stored, action): (String, String) = registry
            .connection
            .query_row(
                "SELECT gc_plan.payload_json, audit_event.action
                 FROM gc_plan JOIN audit_event ON gc_plan.id = audit_event.plan_id
                 WHERE gc_plan.id = ?1",
                [plan.plan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), payload);
        assert_eq!(action, "gc_plan");

        registry
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_plan_audit BEFORE INSERT ON audit_event
                 WHEN NEW.action = 'gc_plan'
                 BEGIN SELECT RAISE(ABORT, 'fixture'); END;",
            )
            .unwrap();
        assert!(registry
            .save_plan(project.id, &digest, &payload, ttl)
            .is_err());
        let count: i64 = registry
            .connection
            .query_row("SELECT count(*) FROM gc_plan", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn unrecognized_database_is_preserved() {
        let temp = tempdir().unwrap();
        let database = temp.path().join(DATABASE_NAME);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER); INSERT INTO fixture VALUES (7);")
            .unwrap();
        drop(connection);
        let before = fs::read(&database).unwrap();
        assert!(matches!(
            Registry::open(temp.path()),
            Err(RegistryError::UnrecognizedDatabase)
        ));
        assert_eq!(fs::read(&database).unwrap(), before);
    }

    #[test]
    fn v1_migration_preserves_projects_plans_and_audit() {
        let temp = tempdir().unwrap();
        let fixture = Connection::open(temp.path().join(DATABASE_NAME)).unwrap();
        fixture.execute_batch(INITIAL_SCHEMA).unwrap();
        fixture
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        fixture.pragma_update(None, "user_version", 1).unwrap();
        fixture
            .execute_batch(
                "INSERT INTO project VALUES (7, 'fixture', 'project', 2, 3, 4);
                 INSERT INTO gc_plan VALUES (8, 7, 'digest', '{}', 5, 6, 'planned');
                 INSERT INTO audit_event VALUES
                    (9, 7, 8, 'local_cli', 'gc_plan', 'success', 'PLAN_SAVED', 5);",
            )
            .unwrap();
        drop(fixture);

        let mut registry = Registry::open(temp.path()).unwrap();
        let version: i64 = registry
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let previous: (i64, i64, i64, String) = registry
            .connection
            .query_row(
                "SELECT project.id, gc_plan.id, audit_event.id, reason_code
                 FROM project JOIN gc_plan ON project.id = gc_plan.project_id
                 JOIN audit_event ON gc_plan.id = audit_event.plan_id
                 WHERE audit_event.lease_id IS NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(previous, (7, 8, 9, "PLAN_SAVED".into()));
        let lease = registry
            .acquire_lease(7, &"a".repeat(64), Duration::from_secs(1), |_| false)
            .unwrap();
        registry
            .finish_lease(lease, AuditOutcome::Success, "EXECUTION_COMPLETED")
            .unwrap();
    }

    fn lease_fixture_projects(root: &Path, registry: &mut Registry) -> [i64; 3] {
        ["first", "second", "third"].map(|label| {
            let project = root.join(label);
            fs::create_dir(&project).unwrap();
            registry.register(&project, label, 2).unwrap().id
        })
    }

    fn force_lease_expiry(registry: &Registry, id: i64) {
        registry
            .connection
            .execute(
                "UPDATE execution_lease SET acquired_at_ms = 0, heartbeat_at_ms = 0,
                 expires_at_ms = 1 WHERE id = ?1",
                [id],
            )
            .unwrap();
    }

    #[test]
    fn live_lock_enforces_project_scope_and_global_limit_even_when_expired() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let mut owner = Registry::open(&state).unwrap();
        let mut contender = Registry::open(&state).unwrap();
        let [first, second, third] = lease_fixture_projects(temp.path(), &mut owner);
        let [scope_a, scope_b, scope_c] = ["a", "b", "c"].map(|part| part.repeat(64));
        let ttl = Duration::from_secs(30);
        let lease_a = owner
            .acquire_lease(first, &scope_a, ttl, |_| false)
            .unwrap();
        owner.attach_lease_process(&lease_a, 101).unwrap();
        force_lease_expiry(&owner, lease_a.id);
        let no_probe = |_| panic!("the OS lock must prevent probing a live lease");
        for (project, scope) in [(first, &scope_b), (second, &scope_a)] {
            assert!(matches!(
                contender.acquire_lease(project, scope, ttl, no_probe),
                Err(RegistryError::LeaseConflict)
            ));
        }
        let lease_b = contender
            .acquire_lease(second, &scope_b, ttl, no_probe)
            .unwrap();
        assert!(matches!(
            contender.acquire_lease(third, &scope_c, ttl, no_probe),
            Err(RegistryError::LeaseCapacity)
        ));
        owner.heartbeat(&lease_a, ttl).unwrap();
        let (heartbeat_at, expires_at): (i64, i64) = owner
            .connection
            .query_row(
                "SELECT heartbeat_at_ms, expires_at_ms FROM execution_lease WHERE id = ?1",
                [lease_a.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(heartbeat_at > 0);
        assert_eq!(expires_at - heartbeat_at, 30_000);
        owner
            .finish_lease(lease_a, AuditOutcome::Success, "EXECUTION_COMPLETED")
            .unwrap();
        let lease_c = contender
            .acquire_lease(third, &scope_c, ttl, no_probe)
            .unwrap();
        for lease in [lease_b, lease_c] {
            contender
                .finish_lease(lease, AuditOutcome::Success, "EXECUTION_COMPLETED")
                .unwrap();
        }
    }

    #[test]
    fn orphan_reclaim_requires_expiry_and_confirmed_process_exit() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let mut registry = Registry::open(&state).unwrap();
        let [first, second, third] = lease_fixture_projects(temp.path(), &mut registry);
        let [scope_a, scope_b, scope_c] = ["a", "b", "c"].map(|part| part.repeat(64));
        let ttl = Duration::from_secs(30);
        let lease_a = registry
            .acquire_lease(first, &scope_a, ttl, |_| false)
            .unwrap();
        let lease_b = registry
            .acquire_lease(second, &scope_b, ttl, |_| false)
            .unwrap();
        registry.attach_lease_process(&lease_a, 101).unwrap();
        assert!(matches!(
            registry.attach_lease_process(&lease_a, 102),
            Err(RegistryError::InvalidLeaseProcess)
        ));
        let (lease_a_id, lease_b_id) = (lease_a.id, lease_b.id);
        drop((lease_a, lease_b));

        assert!(matches!(
            registry.acquire_lease(third, &scope_c, ttl, |_| panic!("unexpired lease probe")),
            Err(RegistryError::LeaseCapacity)
        ));
        force_lease_expiry(&registry, lease_a_id);
        force_lease_expiry(&registry, lease_b_id);
        let probe_count = std::cell::Cell::new(0);
        assert!(matches!(
            registry.acquire_lease(third, &scope_c, ttl, |pid| {
                assert_eq!(pid, 101);
                probe_count.set(probe_count.get() + 1);
                false
            }),
            Err(RegistryError::LeaseCapacity)
        ));
        assert_eq!(probe_count.get(), 1);
        let lease_c = registry
            .acquire_lease(third, &scope_c, ttl, |pid| pid == 101)
            .unwrap();
        let audit: (String, String) = registry
            .connection
            .query_row(
                "SELECT outcome, reason_code FROM audit_event WHERE lease_id = ?1",
                [lease_a_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(audit, ("incomplete".into(), "LEASE_RECLAIMED".into()));
        assert!(matches!(
            registry.acquire_lease(second, &scope_b, ttl, |_| panic!("lease has no PID")),
            Err(RegistryError::LeaseConflict)
        ));
        registry
            .finish_lease(lease_c, AuditOutcome::Success, "EXECUTION_COMPLETED")
            .unwrap();
    }

    #[test]
    fn failed_finish_audit_leaves_lease_active() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let mut registry = Registry::open(&state).unwrap();
        let [first, _, _] = lease_fixture_projects(temp.path(), &mut registry);
        let scope = "a".repeat(64);
        let ttl = Duration::from_secs(30);
        let lease = registry
            .acquire_lease(first, &scope, ttl, |_| false)
            .unwrap();
        assert!(matches!(
            registry.finish_lease(lease, AuditOutcome::Failure, "raw input"),
            Err(RegistryError::InvalidReasonCode)
        ));
        assert!(matches!(
            registry.acquire_lease(first, &scope, ttl, |_| true),
            Err(RegistryError::LeaseConflict)
        ));
    }

    struct GcFixture {
        project_id: i64,
        plan_id: i64,
        policy_digest: String,
        identity: String,
    }

    impl GcFixture {
        fn approve(&self, registry: &mut Registry, token: &str) -> Result<(), RegistryError> {
            registry.approve_gc(
                self.project_id,
                self.plan_id,
                "build",
                &self.policy_digest,
                &self.identity,
                token,
                Duration::from_secs(60),
            )
        }
    }

    fn gc_fixture(root: &Path, registry: &mut Registry) -> GcFixture {
        let project_id = registry.register(root, "fixture", 2).unwrap().id;
        let file_identity = crate::filesystem::FileIdentity {
            device: 7,
            inode: 11,
        };
        let identity = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&file_identity).unwrap())
        );
        let policy_digest = "a".repeat(64);
        let plan_id = registry
            .save_plan(
                project_id,
                &policy_digest,
                &json!({
                    "dry_run": true,
                    "candidates": [{"relative_path": "build", "identity": file_identity}]
                }),
                Duration::from_secs(600),
            )
            .unwrap()
            .plan_id;
        GcFixture {
            project_id,
            plan_id,
            policy_digest,
            identity,
        }
    }

    #[test]
    fn observation_reports_only_scoped_lease_and_approval_state() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("project");
        let other = temp.path().join("other");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&other).unwrap();
        let root = root.canonicalize().unwrap();
        let other = other.canonicalize().unwrap();
        let mut registry = Registry::open(&temp.path().join("state")).unwrap();
        let fixture = gc_fixture(&root, &mut registry);
        let other_fixture = gc_fixture(&other, &mut registry);
        let observe = |registry: &Registry, root: &Path, plan_id, digest: &str| {
            observe_snapshot(&registry.connection, root, "fixture", 2, digest, plan_id)
        };
        let approval = |registry: &Registry, digest: &str| {
            observe(registry, &root, Some(fixture.plan_id), digest).unwrap()
        };
        assert!(matches!(
            observe(
                &registry,
                &root,
                Some(other_fixture.plan_id),
                &fixture.policy_digest
            ),
            Err(RegistryError::GcPlanNotFound)
        ));
        assert!(matches!(
            approval(&registry, &fixture.policy_digest),
            StateObservation::Approval {
                status: ApprovalState::AwaitingApproval,
                ..
            }
        ));
        let lease = registry
            .acquire_lease(
                fixture.project_id,
                &"c".repeat(64),
                Duration::from_secs(10),
                |_| false,
            )
            .unwrap();
        assert!(matches!(
            observe(&registry, &other, None, &fixture.policy_digest).unwrap(),
            StateObservation::Execution {
                status: ExecutionState::None,
                ..
            }
        ));
        assert!(matches!(
            observe(&registry, &root, None, &fixture.policy_digest).unwrap(),
            StateObservation::Execution {
                status: ExecutionState::Active,
                ..
            }
        ));
        force_lease_expiry(&registry, lease.id);
        assert!(matches!(
            observe(&registry, &root, None, &fixture.policy_digest).unwrap(),
            StateObservation::Execution {
                status: ExecutionState::ExpiredUnreleased,
                ..
            }
        ));
        registry
            .register_gc_output(fixture.project_id, "build", &fixture.identity)
            .unwrap();
        let token_digest = "b".repeat(64);
        fixture.approve(&mut registry, &token_digest).unwrap();
        let recorded = approval(&registry, &fixture.policy_digest);
        assert!(matches!(
            recorded,
            StateObservation::Approval {
                status: ApprovalState::ApprovedRecorded,
                ..
            }
        ));
        let output = serde_json::to_value(recorded).unwrap();
        assert_eq!(output.as_object().unwrap().len(), 4);
        assert!(!output.to_string().contains(&token_digest));
        assert!(matches!(
            approval(&registry, &"e".repeat(64)),
            StateObservation::Approval {
                status: ApprovalState::PolicyChanged,
                ..
            }
        ));
        registry
            .connection
            .execute("UPDATE gc_application SET expires_at_ms = 2, approved_at_ms = 1 WHERE plan_id = ?1", [fixture.plan_id])
            .unwrap();
        assert!(matches!(
            approval(&registry, &fixture.policy_digest),
            StateObservation::Approval {
                status: ApprovalState::Expired,
                ..
            }
        ));
        registry
            .connection
            .execute(
                "UPDATE gc_application SET claimed_at_ms = 2 WHERE plan_id = ?1",
                [fixture.plan_id],
            )
            .unwrap();
        set_gc_status(&registry.connection, fixture.plan_id, "applied").unwrap();
        assert!(matches!(
            approval(&registry, &fixture.policy_digest),
            StateObservation::Approval {
                status: ApprovalState::Applied,
                ..
            }
        ));
    }

    #[test]
    fn v2_migration_preserves_existing_leases_and_events() {
        let temp = tempdir().unwrap();
        let connection = Connection::open(temp.path().join(DATABASE_NAME)).unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        connection.execute_batch(LEASE_SCHEMA).unwrap();
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        connection
            .execute_batch(
                "INSERT INTO project VALUES (1, 'fixture', 'project', 2, 3, 4);
                 INSERT INTO execution_lease VALUES (2, 1, printf('%064d', 0), 0, 7, 3, 4, 5, 6);
                 INSERT INTO audit_event VALUES
                    (3, 1, NULL, 'local_cli', 'managed_execution', 'success',
                     'EXECUTION_COMPLETED', 5, 2);",
            )
            .unwrap();
        drop(connection);

        let registry = Registry::open(temp.path()).unwrap();
        let version: i64 = registry
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let existing: (u32, String) = registry
            .connection
            .query_row(
                "SELECT process_id, reason_code FROM execution_lease JOIN audit_event
                 ON execution_lease.id = audit_event.lease_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(existing, (7, "EXECUTION_COMPLETED".into()));
    }

    #[test]
    fn gc_approval_requires_registered_identity_matching_plan_snapshot() {
        let temp = tempdir().unwrap();
        let mut registry = Registry::open(&temp.path().join("state")).unwrap();
        let fixture = gc_fixture(temp.path(), &mut registry);
        let token = "b".repeat(64);
        assert!(matches!(
            fixture.approve(&mut registry, &token),
            Err(RegistryError::GcOwnershipConflict)
        ));
        let other_identity = "c".repeat(64);
        registry
            .register_gc_output(fixture.project_id, "build", &other_identity)
            .unwrap();
        assert!(matches!(
            registry.register_gc_output(fixture.project_id, "build", &fixture.identity),
            Err(RegistryError::GcOwnershipConflict)
        ));
        assert!(matches!(
            registry.approve_gc(
                fixture.project_id,
                fixture.plan_id,
                "build",
                &fixture.policy_digest,
                &other_identity,
                &token,
                Duration::from_secs(60)
            ),
            Err(RegistryError::GcCandidateMismatch)
        ));
        assert!(matches!(
            registry.read_gc_plan(fixture.project_id + 1, fixture.plan_id),
            Err(RegistryError::GcPlanNotFound)
        ));
    }

    #[test]
    fn concurrent_approval_has_one_winner_and_terminal_apply_cannot_replay() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let mut registry = Registry::open(&state).unwrap();
        let fixture = Arc::new(gc_fixture(temp.path(), &mut registry));
        registry
            .register_gc_output(fixture.project_id, "build", &fixture.identity)
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let workers = ["b".repeat(64), "c".repeat(64)].map(|token| {
            let state = state.clone();
            let barrier = Arc::clone(&barrier);
            let fixture = Arc::clone(&fixture);
            thread::spawn(move || {
                let mut connection = Registry::open(&state).unwrap();
                barrier.wait();
                let result = fixture.approve(&mut connection, &token);
                (token, result)
            })
        });
        let approvals = workers.map(|worker| worker.join().unwrap());
        let token = approvals
            .iter()
            .find(|(_, result)| result.is_ok())
            .map(|(token, _)| token)
            .unwrap();
        assert_eq!(
            approvals
                .iter()
                .filter(|(_, result)| matches!(result, Err(RegistryError::GcStateConflict)))
                .count(),
            1
        );
        let ttl = Duration::from_secs(60);
        let lease = registry
            .acquire_lease(fixture.project_id, &"d".repeat(64), ttl, |_| false)
            .unwrap();
        assert!(matches!(
            registry.claim_gc(&lease, fixture.plan_id, &"e".repeat(64), token),
            Err(RegistryError::GcPolicyChanged)
        ));
        let claim = registry
            .claim_gc(&lease, fixture.plan_id, &fixture.policy_digest, token)
            .unwrap();
        assert_eq!(claim.relative_path, "build");
        assert_eq!(claim.identity, fixture.identity);
        assert!(matches!(
            registry.claim_gc(&lease, fixture.plan_id, &fixture.policy_digest, token),
            Err(RegistryError::GcStateConflict)
        ));
        registry
            .finish_gc(&lease, fixture.plan_id, GcApplyStatus::RecoveryRequired, 0)
            .unwrap();
        registry
            .finish_lease(lease, AuditOutcome::Incomplete, "GC_INTERRUPTED")
            .unwrap();
        let recovery_lease = registry
            .acquire_lease(fixture.project_id, &"d".repeat(64), ttl, |_| false)
            .unwrap();
        assert_eq!(
            registry
                .pending_gc(&recovery_lease, fixture.plan_id)
                .unwrap()
                .identity,
            claim.identity
        );
        registry
            .finish_gc(&recovery_lease, fixture.plan_id, GcApplyStatus::Applied, 17)
            .unwrap();
        assert!(matches!(
            registry.pending_gc(&recovery_lease, fixture.plan_id),
            Err(RegistryError::GcStateConflict)
        ));
        let evidence: (i64, i64) = registry
            .connection
            .query_row(
                "SELECT bytes_applied, count(*) FROM gc_application
                 JOIN audit_event ON gc_application.plan_id = audit_event.plan_id
                 WHERE gc_application.plan_id = ?1 AND action = 'gc_apply'",
                [fixture.plan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(evidence, (17, 3));
    }

    #[test]
    fn expired_approval_and_plan_cannot_be_claimed() {
        let temp = tempdir().unwrap();
        let mut registry = Registry::open(&temp.path().join("state")).unwrap();
        let fixture = gc_fixture(temp.path(), &mut registry);
        let token = "b".repeat(64);
        registry
            .register_gc_output(fixture.project_id, "build", &fixture.identity)
            .unwrap();
        fixture.approve(&mut registry, &token).unwrap();
        registry
            .connection
            .execute(
                "UPDATE gc_application SET approved_at_ms = 0, expires_at_ms = 1 WHERE plan_id = ?1",
                [fixture.plan_id],
            )
            .unwrap();
        let lease = registry
            .acquire_lease(
                fixture.project_id,
                &"d".repeat(64),
                Duration::from_secs(60),
                |_| false,
            )
            .unwrap();
        assert!(matches!(
            registry.claim_gc(&lease, fixture.plan_id, &fixture.policy_digest, &token),
            Err(RegistryError::GcExpired)
        ));
        registry
            .connection
            .execute(
                "UPDATE gc_plan SET created_at = 0, expires_at = 1 WHERE id = ?1",
                [fixture.plan_id],
            )
            .unwrap();
        assert!(matches!(
            fixture.approve(&mut registry, &token),
            Err(RegistryError::GcExpired)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn state_sidecar_symlinks_and_database_hardlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        let fixture = temp.path().join("untouched");
        fs::write(&fixture, b"fixture").unwrap();
        symlink(&fixture, state.join(format!("{DATABASE_NAME}-wal"))).unwrap();
        assert!(matches!(
            Registry::open(&state),
            Err(RegistryError::UnsafeStatePath)
        ));
        assert!(!state.join(DATABASE_NAME).exists());

        let other_state = temp.path().join("other-state");
        fs::create_dir(&other_state).unwrap();
        fs::hard_link(&fixture, other_state.join(DATABASE_NAME)).unwrap();
        assert!(matches!(
            Registry::open(&other_state),
            Err(RegistryError::UnsafeStatePath)
        ));
        assert_eq!(fs::read(fixture).unwrap(), b"fixture");
    }
}
