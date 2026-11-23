use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use thiserror::Error;

pub const PROJECT_CONFIG: &str = ".dsg.json";
pub const POLICY_VERSION: u32 = 2;
const MAX_POLICY_BYTES: usize = 64 * 1024;
const MAX_NAMES: usize = 128;
const MAX_SCAN_ENTRIES: usize = 200_000;
const MAX_CANDIDATES: usize = 100;

const DEFAULT_SCAN_DIRS: &[&str] = &[
    ".next",
    "dist",
    "build",
    "target",
    "coverage",
    "test-results",
    "playwright-report",
    ".pytest_cache",
    ".turbo",
];
const SYSTEM_PROTECTED: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    ".env",
    "data",
    "uploads",
    "postgres-data",
    "mysql-data",
];

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("설정 파일이 이미 있습니다")]
    AlreadyExists,
    #[error("설정 파일에 접근할 수 없습니다")]
    Io(#[source] io::Error),
    #[error("설정 JSON의 형식이 올바르지 않습니다")]
    InvalidJson(#[source] serde_json::Error),
    #[error("설정 항목 {0}의 값 또는 정책 범위를 확인하세요")]
    Invalid(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Policy {
    pub version: u32,
    pub project_id: String,
    pub project_budget_gb: u64,
    pub host_min_free_gb: u64,
    pub warning_percent: u8,
    pub critical_percent: u8,
    pub scan_dirs: Vec<String>,
    pub protected_names: Vec<String>,
    pub gc_min_age_hours: u64,
    pub max_candidates: usize,
    pub max_scan_files: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            version: POLICY_VERSION,
            project_id: "local-project".to_owned(),
            project_budget_gb: 30,
            host_min_free_gb: 20,
            warning_percent: 75,
            critical_percent: 90,
            scan_dirs: DEFAULT_SCAN_DIRS.iter().map(ToString::to_string).collect(),
            protected_names: SYSTEM_PROTECTED.iter().map(ToString::to_string).collect(),
            gc_min_age_hours: 168,
            max_candidates: MAX_CANDIDATES,
            max_scan_files: MAX_SCAN_ENTRIES,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    version: Option<u32>,
    project_id: Option<String>,
    project_budget_gb: Option<u64>,
    host_min_free_gb: Option<u64>,
    warning_percent: Option<u8>,
    critical_percent: Option<u8>,
    scan_dirs: Option<Vec<String>>,
    protected_names: Option<Vec<String>>,
    gc_min_age_hours: Option<u64>,
    max_candidates: Option<usize>,
    max_scan_files: Option<usize>,
}

impl Layer {
    fn validate(&self) -> Result<(), PolicyError> {
        if self
            .version
            .is_some_and(|version| version != POLICY_VERSION)
        {
            return Err(PolicyError::Invalid("version"));
        }
        if let Some(label) = &self.project_id {
            validate_label(label)?;
        }
        for (field, names) in [
            ("scan_dirs", self.scan_dirs.as_deref()),
            ("protected_names", self.protected_names.as_deref()),
        ] {
            if names.is_some_and(|names| {
                names.len() > MAX_NAMES || names.iter().any(|name| !is_direct_child(name))
            }) {
                return Err(PolicyError::Invalid(field));
            }
        }
        if self
            .max_candidates
            .is_some_and(|count| count > MAX_CANDIDATES)
        {
            return Err(PolicyError::Invalid("max_candidates"));
        }
        if self
            .max_scan_files
            .is_some_and(|count| count > MAX_SCAN_ENTRIES)
        {
            return Err(PolicyError::Invalid("max_scan_files"));
        }
        if self
            .warning_percent
            .is_some_and(|percent| !(1..100).contains(&percent))
            || self
                .critical_percent
                .is_some_and(|percent| !(1..=100).contains(&percent))
            || matches!((self.warning_percent, self.critical_percent), (Some(warn), Some(critical)) if warn >= critical)
        {
            return Err(PolicyError::Invalid("warning_percent / critical_percent"));
        }
        if self
            .gc_min_age_hours
            .is_some_and(|hours| hours.checked_mul(3600).is_none())
            || self
                .project_budget_gb
                .is_some_and(|gb| gb.checked_mul(1 << 30).is_none())
            || self
                .host_min_free_gb
                .is_some_and(|gb| gb.checked_mul(1 << 30).is_none())
        {
            return Err(PolicyError::Invalid("budget / gc_min_age_hours"));
        }
        Ok(())
    }
}

impl Policy {
    pub fn protects(&self, name: &str) -> bool {
        let normalized = name.to_lowercase();
        SYSTEM_PROTECTED.contains(&normalized.as_str())
            || self
                .protected_names
                .iter()
                .any(|rule| rule.to_lowercase() == normalized)
            || is_private_name(&normalized)
    }

    pub fn load(
        root: &Path,
        global: Option<&Path>,
        session: Option<&Path>,
    ) -> Result<Self, PolicyError> {
        let mut policy = Self::default();
        if let Some(global) = global {
            policy.merge(read_layer(global, false)?, false)?;
        }
        policy.merge(read_layer(&root.join(PROJECT_CONFIG), true)?, true)?;
        if let Some(session) = session {
            let layer = read_layer(session, false)?;
            if layer.project_id.is_some() {
                return Err(PolicyError::Invalid("session.project_id"));
            }
            policy.merge(layer, true)?;
        }
        Ok(policy)
    }

    fn merge(&mut self, layer: Layer, restricted: bool) -> Result<(), PolicyError> {
        layer.validate()?;
        if let Some(label) = layer.project_id {
            self.project_id = label;
        }
        if let Some(names) = layer.scan_dirs {
            if restricted {
                self.scan_dirs.retain(|name| names.contains(name));
            } else {
                self.scan_dirs = names;
            }
        }
        self.protected_names
            .extend(layer.protected_names.unwrap_or_default());
        self.protected_names.sort_unstable();
        self.protected_names.dedup();
        if let Some(value) = layer.project_budget_gb {
            self.project_budget_gb = if restricted {
                self.project_budget_gb.min(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.host_min_free_gb {
            self.host_min_free_gb = if restricted {
                self.host_min_free_gb.max(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.warning_percent {
            self.warning_percent = if restricted {
                self.warning_percent.min(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.critical_percent {
            self.critical_percent = if restricted {
                self.critical_percent.min(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.gc_min_age_hours {
            self.gc_min_age_hours = if restricted {
                self.gc_min_age_hours.max(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.max_candidates {
            self.max_candidates = if restricted {
                self.max_candidates.min(value)
            } else {
                value
            };
        }
        if let Some(value) = layer.max_scan_files {
            self.max_scan_files = if restricted {
                self.max_scan_files.min(value)
            } else {
                value
            };
        }
        if self.warning_percent >= self.critical_percent {
            return Err(PolicyError::Invalid("warning_percent / critical_percent"));
        }
        Ok(())
    }
}

pub fn is_direct_child(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

pub fn init_project(root: &Path, label: &str) -> Result<(), PolicyError> {
    validate_label(label)?;
    let path = root.join(PROJECT_CONFIG);
    let mut stage = tempfile::NamedTempFile::new_in(root).map_err(PolicyError::Io)?;
    serde_json::to_writer_pretty(
        &mut stage,
        &serde_json::json!({
            "version": POLICY_VERSION,
            "project_id": label,
        }),
    )
    .map_err(PolicyError::InvalidJson)?;
    stage.write_all(b"\n").map_err(PolicyError::Io)?;
    stage.as_file().sync_all().map_err(PolicyError::Io)?;
    stage.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            PolicyError::AlreadyExists
        } else {
            PolicyError::Io(error.error)
        }
    })?;
    Ok(())
}

fn validate_label(label: &str) -> Result<(), PolicyError> {
    if label.trim().is_empty() || label.len() > 200 || label.chars().any(char::is_control) {
        return Err(PolicyError::Invalid("project_id"));
    }
    Ok(())
}

fn is_private_name(name: &str) -> bool {
    name == ".envrc"
        || name.ends_with(".env")
        || name.contains(".env.")
        || (name.contains(".deploy.") && name.ends_with("env"))
        || name == "credentials.json"
        || ((name.starts_with("credentials.") || name.starts_with("service-account"))
            && name.ends_with(".json"))
        || [".pem", ".key", ".p12", ".pfx", ".sqlite", ".sqlite3", ".db"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn read_layer(path: &Path, optional: bool) -> Result<Layer, PolicyError> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json")
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !is_private_name(&name.to_lowercase()))
    {
        return Err(PolicyError::Invalid("policy_file"));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if optional && error.kind() == io::ErrorKind::NotFound => {
            return Ok(Layer::default())
        }
        Err(error) => return Err(PolicyError::Io(error)),
    };
    if !metadata.is_file() || metadata.len() > MAX_POLICY_BYTES as u64 {
        return Err(PolicyError::Invalid("policy_file"));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(PolicyError::Io)?;
    let metadata = file.metadata().map_err(PolicyError::Io)?;
    if !metadata.is_file() {
        return Err(PolicyError::Invalid("policy_file"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(PolicyError::Invalid("policy_file"));
        }
    }
    let mut data = Vec::with_capacity(metadata.len().min(MAX_POLICY_BYTES as u64) as usize);
    file.take(MAX_POLICY_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(PolicyError::Io)?;
    if data.len() > MAX_POLICY_BYTES {
        return Err(PolicyError::Invalid("policy_file"));
    }
    serde_json::from_slice(&data).map_err(PolicyError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_layers_keep_stricter_budgets_and_protection() {
        let fixture = tempfile::tempdir().unwrap();
        let global = fixture.path().join("global.json");
        let session = fixture.path().join("session.json");
        fs::write(&global, r#"{"project_budget_gb":12,"host_min_free_gb":11,"scan_dirs":["build","dist"],"protected_names":["global-data"]}"#).unwrap();
        fs::write(fixture.path().join(PROJECT_CONFIG), r#"{"project_budget_gb":40,"host_min_free_gb":1,"scan_dirs":["build","target"],"protected_names":[]}"#).unwrap();
        fs::write(
            &session,
            r#"{"project_budget_gb":9,"host_min_free_gb":5,"protected_names":["session-data"]}"#,
        )
        .unwrap();
        let policy = Policy::load(fixture.path(), Some(&global), Some(&session)).unwrap();
        assert_eq!(policy.project_budget_gb, 9);
        assert_eq!(policy.host_min_free_gb, 11);
        assert_eq!(policy.scan_dirs, ["build"]);
        for name in ["Data", "global-data", "session-data", "local.env.archive"] {
            assert!(policy.protects(name));
        }
    }

    #[test]
    fn initialization_keeps_existing_file_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        init_project(fixture.path(), "fixture").unwrap();
        assert!(matches!(
            init_project(fixture.path(), "other"),
            Err(PolicyError::AlreadyExists)
        ));
        assert_eq!(
            Policy::load(fixture.path(), None, None).unwrap().project_id,
            "fixture"
        );
    }
}
