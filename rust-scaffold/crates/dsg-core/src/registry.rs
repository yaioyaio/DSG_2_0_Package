use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

const DATABASE_NAME: &str = "registry.sqlite3";
const APPLICATION_ID: i64 = 0x4453_4732;
const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const OPEN_RETRY_WINDOW: Duration = Duration::from_secs(3);
const MAX_REASON_CODE_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 200;
const MAX_PLAN_BYTES: usize = 1024 * 1024;

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

#[derive(Debug, Clone, Copy)]
pub enum AuditAction {
    ProjectRegister,
    PolicyCheck,
    Scan,
    GcPlan,
}

impl AuditAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProjectRegister => "project_register",
            Self::PolicyCheck => "policy_check",
            Self::Scan => "scan",
            Self::GcPlan => "gc_plan",
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
                result => return result,
            }
        }
    }

    fn open_database(database_path: &Path) -> Result<Self, RegistryError> {
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
        Ok(Self { connection })
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
            None,
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
            None,
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
            Some(plan_id),
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
        (APPLICATION_ID, SCHEMA_VERSION) => {}
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
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        _ => return Err(RegistryError::UnrecognizedDatabase),
    }
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

fn insert_event(
    connection: &Connection,
    project_id: Option<i64>,
    plan_id: Option<i64>,
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
    connection.execute(
        "INSERT INTO audit_event (project_id, plan_id, action, outcome, reason_code, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project_id,
            plan_id,
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
