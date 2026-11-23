use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

const DATABASE_NAME: &str = "registry.sqlite3";
const APPLICATION_ID: i64 = 0x4453_4732;
const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const OPEN_RETRY_WINDOW: Duration = Duration::from_secs(3);
const MAX_REASON_CODE_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 200;
const MAX_PLAN_BYTES: usize = 1024 * 1024;
const LEASE_SLOTS: i64 = 2;
const MAX_LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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
        (APPLICATION_ID, 1) => {}
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
    tx.execute_batch(LEASE_SCHEMA)?;
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

enum EventReference {
    None,
    Plan(i64),
    Lease(i64),
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
        assert_eq!(version, 2);
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
