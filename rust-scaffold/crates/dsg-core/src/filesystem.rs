use crate::policy::{is_direct_child, Policy};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, DirEntry, Metadata, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_DIRECTORY_DEPTH: usize = 64;

#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub project_id: String,
    pub entries: Vec<ScanEntry>,
    pub total_bytes: u64,
    pub complete: bool,
    pub disk_free_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub observed_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct ScanEntry {
    pub path: String,
    pub bytes: u64,
    pub files: usize,
    pub complete: bool,
    pub issues: Vec<InspectionIssue>,
}

#[derive(Debug, Serialize)]
pub struct GcPlan {
    pub project_id: String,
    pub candidates: Vec<Candidate>,
    pub excluded: Vec<ExcludedCandidate>,
    pub dry_run: bool,
    pub observed_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub relative_path: String,
    pub bytes: u64,
    pub modified_at_unix: u64,
    pub modified_at_subsec_nanos: u32,
    pub identity: Option<FileIdentity>,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Serialize)]
pub struct ExcludedCandidate {
    pub relative_path: String,
    pub reason: InspectionIssue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionIssue {
    ProtectedName,
    InvalidOutputName,
    UnsupportedEntryName,
    LinkOrReparsePoint,
    MountBoundary,
    UnsupportedType,
    MetadataUnavailable,
    ChangedDuringInspection,
    ScanLimitReached,
    DepthLimitReached,
    CandidateLimitReached,
    TooRecent,
    DuplicateOutput,
    SizeOverflow,
}

pub fn canonical_root(path: &Path) -> io::Result<PathBuf> {
    resolve_root(path).map(|(canonical, _)| canonical)
}

pub fn scan(root: &Path, policy: &Policy) -> io::Result<ScanReport> {
    let (root, root_metadata) = resolve_root(root)?;
    let observed_at_unix = observed_time()?.1;
    let disk_free_bytes = fs2::available_space(&root).ok();
    let disk_total_bytes = fs2::total_space(&root).ok();
    let mut report = ScanReport {
        project_id: policy.project_id.clone(),
        entries: Vec::new(),
        total_bytes: 0,
        complete: disk_free_bytes.is_some() && disk_total_bytes.is_some(),
        disk_free_bytes,
        disk_total_bytes,
        observed_at_unix,
    };
    let mut remaining = policy.max_scan_files;
    let mut seen_outputs = HashSet::new();

    for name in &policy.scan_dirs {
        let inspection = match inspect_output(&root, &root_metadata, name, policy) {
            Ok(Some(output)) if seen_outputs.insert(output.key()) => {
                inspect_tree(&output, &root_metadata, policy, &mut remaining, Mode::Scan)
            }
            Ok(_) => continue,
            Err(issue) => Inspection {
                issues: vec![issue],
                ..Inspection::default()
            },
        };
        let complete = inspection.issues.is_empty();
        report.complete &= complete;
        match report.total_bytes.checked_add(inspection.bytes) {
            Some(bytes) => report.total_bytes = bytes,
            None => {
                report.total_bytes = u64::MAX;
                report.complete = false;
            }
        }
        report.entries.push(ScanEntry {
            path: name.clone(),
            bytes: inspection.bytes,
            files: inspection.files,
            complete,
            issues: inspection.issues,
        });
    }
    Ok(report)
}

pub fn plan(root: &Path, policy: &Policy) -> io::Result<GcPlan> {
    let (root, root_metadata) = resolve_root(root)?;
    let (now, observed_at_unix) = observed_time()?;
    let mut plan = GcPlan {
        project_id: policy.project_id.clone(),
        candidates: Vec::new(),
        excluded: Vec::new(),
        dry_run: true,
        observed_at_unix,
    };
    let mut remaining = policy.max_scan_files;
    let mut seen_outputs = HashSet::new();

    for name in &policy.scan_dirs {
        let result = inspect_output(&root, &root_metadata, name, policy).and_then(|output| {
            let Some(output) = output else {
                return Ok(None);
            };
            if plan.candidates.len() >= policy.max_candidates {
                return Err(InspectionIssue::CandidateLimitReached);
            }
            if !seen_outputs.insert(output.key()) {
                return Err(InspectionIssue::DuplicateOutput);
            }
            candidate(name, output, &root_metadata, policy, &mut remaining, now).map(Some)
        });
        match result {
            Ok(Some(candidate)) => plan.candidates.push(candidate),
            Ok(None) => (),
            Err(reason) => plan.excluded.push(ExcludedCandidate {
                relative_path: name.clone(),
                reason,
            }),
        }
    }
    Ok(plan)
}

fn resolve_root(path: &Path) -> io::Result<(PathBuf, Metadata)> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project root must resolve to a directory",
        ));
    }
    Ok((canonical, metadata))
}

fn observed_time() -> io::Result<(SystemTime, u64)> {
    let now = SystemTime::now();
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| (now, elapsed.as_secs()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectorySnapshot {
    identity: Option<FileIdentity>,
    modified: SystemTime,
}

impl DirectorySnapshot {
    fn from_metadata(metadata: &Metadata) -> Result<Self, InspectionIssue> {
        Ok(Self {
            identity: identity(metadata),
            modified: metadata
                .modified()
                .map_err(|_| InspectionIssue::MetadataUnavailable)?,
        })
    }
}

#[derive(Clone)]
struct Directory {
    path: PathBuf,
    snapshot: DirectorySnapshot,
}

#[derive(Eq, Hash, PartialEq)]
enum DirectoryKey {
    Identity(FileIdentity),
    Path(PathBuf),
}

impl Directory {
    fn key(&self) -> DirectoryKey {
        match self.snapshot.identity {
            Some(identity) => DirectoryKey::Identity(identity),
            None => DirectoryKey::Path(self.path.clone()),
        }
    }

    fn verify(&self) -> Result<(), InspectionIssue> {
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| InspectionIssue::MetadataUnavailable)?;
        if is_link_or_reparse_point(&metadata) {
            return Err(InspectionIssue::LinkOrReparsePoint);
        }
        if !metadata.is_dir() || DirectorySnapshot::from_metadata(&metadata)? != self.snapshot {
            return Err(InspectionIssue::ChangedDuringInspection);
        }
        // Windows does not provide a stable directory identity through std metadata.
        #[cfg(windows)]
        if fs::canonicalize(&self.path).ok().as_ref() != Some(&self.path) {
            return Err(InspectionIssue::ChangedDuringInspection);
        }
        Ok(())
    }

    fn read(self) -> Result<OpenDirectory, InspectionIssue> {
        self.verify()?;
        let entries = fs::read_dir(&self.path).map_err(|_| InspectionIssue::MetadataUnavailable)?;
        self.verify()?;
        Ok(OpenDirectory {
            directory: self,
            entries,
        })
    }
}

struct OpenDirectory {
    directory: Directory,
    entries: ReadDir,
}

fn inspect_output(
    root: &Path,
    root_metadata: &Metadata,
    name: &str,
    policy: &Policy,
) -> Result<Option<Directory>, InspectionIssue> {
    if !is_direct_child(name) {
        return Err(InspectionIssue::InvalidOutputName);
    }
    if policy.protects(name) {
        return Err(InspectionIssue::ProtectedName);
    }
    let path = root.join(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(InspectionIssue::MetadataUnavailable),
    };
    check_boundary(&metadata, root_metadata, identity(root_metadata))?;
    if !metadata.is_dir() {
        return Err(InspectionIssue::UnsupportedType);
    }
    let canonical = fs::canonicalize(&path).map_err(|_| InspectionIssue::MetadataUnavailable)?;
    if canonical.parent() != Some(root) {
        return Err(InspectionIssue::ChangedDuringInspection);
    }
    let canonical_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(InspectionIssue::UnsupportedEntryName)?;
    if policy.protects(canonical_name) {
        return Err(InspectionIssue::ProtectedName);
    }
    Ok(Some(Directory {
        path: canonical,
        snapshot: DirectorySnapshot::from_metadata(&metadata)?,
    }))
}

fn candidate(
    name: &str,
    output: Directory,
    root_metadata: &Metadata,
    policy: &Policy,
    remaining: &mut usize,
    now: SystemTime,
) -> Result<Candidate, InspectionIssue> {
    let snapshot = output.snapshot;
    let age = now
        .duration_since(snapshot.modified)
        .map_err(|_| InspectionIssue::TooRecent)?;
    if age < Duration::from_secs(policy.gc_min_age_hours.saturating_mul(3600)) {
        return Err(InspectionIssue::TooRecent);
    }
    let modified = snapshot
        .modified
        .duration_since(UNIX_EPOCH)
        .map_err(|_| InspectionIssue::MetadataUnavailable)?;
    let inspection = inspect_tree(&output, root_metadata, policy, remaining, Mode::Candidate);
    if let Some(issue) = inspection.issues.first() {
        return Err(*issue);
    }
    Ok(Candidate {
        relative_path: name.to_owned(),
        bytes: inspection.bytes,
        modified_at_unix: modified.as_secs(),
        modified_at_subsec_nanos: modified.subsec_nanos(),
        identity: snapshot.identity,
        reason: "expired_generated_output",
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Scan,
    Candidate,
}

#[derive(Default)]
struct Inspection {
    bytes: u64,
    files: usize,
    issues: Vec<InspectionIssue>,
}

impl Inspection {
    fn record_issue(&mut self, issue: InspectionIssue, mode: Mode) -> bool {
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
        mode == Mode::Candidate
    }
}

fn inspect_tree(
    output: &Directory,
    root_metadata: &Metadata,
    policy: &Policy,
    remaining: &mut usize,
    mode: Mode,
) -> Inspection {
    let mut result = Inspection::default();
    let start = match output.clone().read() {
        Ok(start) => start,
        Err(issue) => {
            result.record_issue(issue, mode);
            return result;
        }
    };
    let mut seen_directories: HashSet<_> = output.snapshot.identity.into_iter().collect();
    // Limit open handles and count every entry, including skipped names and
    // iterator errors, against the shared operation budget.
    let mut stack = vec![start];
    while let Some(current) = stack.last_mut() {
        let Some(entry) = current.entries.next() else {
            if let Err(issue) = current.directory.verify() {
                if result.record_issue(issue, mode) {
                    break;
                }
            }
            stack.pop();
            continue;
        };
        if *remaining == 0 {
            result.record_issue(InspectionIssue::ScanLimitReached, mode);
            break;
        }
        *remaining -= 1;
        let inspected = entry
            .map_err(|_| InspectionIssue::MetadataUnavailable)
            .and_then(|entry| inspect_child(entry, &current.directory, root_metadata, policy));
        let issue = match inspected {
            Ok(Child::Directory(directory)) => {
                if directory
                    .snapshot
                    .identity
                    .is_some_and(|identity| !seen_directories.insert(identity))
                {
                    InspectionIssue::MountBoundary
                } else if stack.len() >= MAX_DIRECTORY_DEPTH {
                    InspectionIssue::DepthLimitReached
                } else {
                    match directory.read() {
                        Ok(open) => {
                            stack.push(open);
                            continue;
                        }
                        Err(issue) => issue,
                    }
                }
            }
            Ok(Child::File(bytes)) => {
                result.files += 1;
                if let Some(total) = result.bytes.checked_add(bytes) {
                    result.bytes = total;
                    continue;
                }
                result.bytes = u64::MAX;
                InspectionIssue::SizeOverflow
            }
            Err(issue) => issue,
        };
        if result.record_issue(issue, mode) {
            break;
        }
    }
    if let Err(issue) = output.verify() {
        result.record_issue(issue, mode);
    }
    result
}

enum Child {
    Directory(Directory),
    File(u64),
}

fn inspect_child(
    entry: DirEntry,
    parent: &Directory,
    root_metadata: &Metadata,
    policy: &Policy,
) -> Result<Child, InspectionIssue> {
    let name = entry.file_name();
    let name = name.to_str().ok_or(InspectionIssue::UnsupportedEntryName)?;
    if policy.protects(name) {
        return Err(InspectionIssue::ProtectedName);
    }
    let metadata = entry
        .metadata()
        .map_err(|_| InspectionIssue::MetadataUnavailable)?;
    check_boundary(&metadata, root_metadata, parent.snapshot.identity)?;
    if metadata.is_file() {
        return Ok(Child::File(metadata.len()));
    }
    if metadata.is_dir() {
        return Ok(Child::Directory(Directory {
            path: entry.path(),
            snapshot: DirectorySnapshot::from_metadata(&metadata)?,
        }));
    }
    Err(InspectionIssue::UnsupportedType)
}

fn check_boundary(
    metadata: &Metadata,
    root_metadata: &Metadata,
    parent_identity: Option<FileIdentity>,
) -> Result<(), InspectionIssue> {
    if is_link_or_reparse_point(metadata) {
        return Err(InspectionIssue::LinkOrReparsePoint);
    }
    if let (Some(entry), Some(root)) = (identity(metadata), identity(root_metadata)) {
        if entry.device != root.device || Some(entry) == parent_identity {
            return Err(InspectionIssue::MountBoundary);
        }
    }
    // An unrelated same-device bind mount can have its own inode and evade
    // portable metadata checks. Path checks also cannot close all swap races.
    #[cfg(not(any(unix, windows)))]
    return Err(InspectionIssue::MetadataUnavailable);
    #[cfg(any(unix, windows))]
    Ok(())
}

#[cfg(unix)]
fn identity(metadata: &Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn identity(_: &Metadata) -> Option<FileIdentity> {
    None
}

fn is_link_or_reparse_point(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::{File, FileTimes};
    use tempfile::tempdir;

    fn policy(outputs: &[&str]) -> Policy {
        Policy {
            scan_dirs: outputs.iter().map(|name| (*name).to_owned()).collect(),
            gc_min_age_hours: 0,
            ..Policy::default()
        }
    }

    fn artifact(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"generated").unwrap();
        path
    }

    #[test]
    fn scan_counts_managed_artifacts_and_skips_protected_trees() {
        let root = tempdir().unwrap();
        artifact(root.path(), "build/report.txt");
        artifact(root.path(), "build/nested/Data/fixture.txt");
        artifact(root.path(), "unmanaged/fixture.txt");

        let report = scan(root.path(), &policy(&["build", "absent"])).unwrap();

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].path, "build");
        assert_eq!(report.entries[0].files, 1);
        assert_eq!(report.total_bytes, 9);
        assert_eq!(report.entries[0].issues, [InspectionIssue::ProtectedName]);
        assert!(!report.complete);
    }

    #[test]
    fn dry_run_preserves_default_and_project_protected_ancestors() {
        let root = tempdir().unwrap();
        let configured = artifact(root.path(), "data/original.txt");
        let nested = artifact(root.path(), "build/deep/Data/original.txt");
        let project_protected = artifact(root.path(), "target/deep/fixture-private/original.txt");
        let eligible = artifact(root.path(), "dist/report.txt");
        let mut policy = policy(&["data", "build", "target", "dist"]);
        policy.protected_names.push("fixture-private".into());

        let report = plan(root.path(), &policy).unwrap();

        assert!(report.dry_run);
        assert_eq!(report.excluded.len(), 3);
        assert!(report
            .excluded
            .iter()
            .all(|item| item.reason == InspectionIssue::ProtectedName));
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].relative_path, "dist");
        assert_eq!(report.candidates[0].bytes, 9);
        assert!(report.candidates[0].modified_at_unix > 0);
        #[cfg(unix)]
        assert!(report.candidates[0].identity.is_some());
        for path in [configured, nested, project_protected, eligible] {
            assert!(path.is_file());
        }
    }

    #[test]
    fn scan_and_plan_apply_one_entry_budget_across_outputs() {
        let root = tempdir().unwrap();
        artifact(root.path(), "build/report.txt");
        artifact(root.path(), "dist/nested/report.txt");
        let mut policy = policy(&["build", "dist"]);
        policy.max_scan_files = 1;

        let measured = scan(root.path(), &policy).unwrap();
        assert_eq!(measured.total_bytes, 9);
        assert!(!measured.complete);
        assert_eq!(
            measured.entries[1].issues,
            [InspectionIssue::ScanLimitReached]
        );

        let proposed = plan(root.path(), &policy).unwrap();
        assert_eq!(proposed.candidates.len(), 1);
        assert_eq!(proposed.candidates[0].relative_path, "build");
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::ScanLimitReached
        );

        policy.scan_dirs = vec!["dist".into()];
        let proposed = plan(root.path(), &policy).unwrap();
        assert!(proposed.candidates.is_empty());
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::ScanLimitReached
        );
    }

    #[test]
    fn depth_limit_keeps_nested_contents_out_of_the_plan() {
        let root = tempdir().unwrap();
        let mut deep = PathBuf::from("build");
        for _ in 0..MAX_DIRECTORY_DEPTH {
            deep.push("d");
        }
        deep.push("report.txt");
        let original = artifact(root.path(), deep.to_str().unwrap());
        let policy = policy(&["build"]);

        let measured = scan(root.path(), &policy).unwrap();
        assert!(!measured.complete);
        assert_eq!(
            measured.entries[0].issues,
            [InspectionIssue::DepthLimitReached]
        );
        let proposed = plan(root.path(), &policy).unwrap();
        assert!(proposed.candidates.is_empty());
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::DepthLimitReached
        );
        assert!(original.is_file());
    }

    #[test]
    #[cfg(unix)]
    fn unreadable_directory_is_excluded_when_permissions_are_enforced() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let file = artifact(root.path(), "build/unreadable/report.txt");
        let directory = file.parent().unwrap();
        let permissions = fs::metadata(directory).unwrap().permissions();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o000)).unwrap();
        let denied = fs::read_dir(directory)
            .err()
            .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied);
        let policy = policy(&["build"]);
        let measured = scan(root.path(), &policy);
        let proposed = plan(root.path(), &policy);
        fs::set_permissions(directory, permissions).unwrap();

        if denied {
            let measured = measured.unwrap();
            assert!(!measured.complete);
            assert_eq!(
                measured.entries[0].issues,
                [InspectionIssue::MetadataUnavailable]
            );
            let proposed = proposed.unwrap();
            assert!(proposed.candidates.is_empty());
            assert_eq!(
                proposed.excluded[0].reason,
                InspectionIssue::MetadataUnavailable
            );
        }
        assert!(file.is_file());
    }

    #[test]
    #[cfg(unix)]
    fn minimum_age_rejects_recent_artifacts() {
        let root = tempdir().unwrap();
        artifact(root.path(), "build/report.txt");
        let recent = artifact(root.path(), "target/report.txt");
        let mut policy = policy(&["build", "target"]);
        policy.gc_min_age_hours = 24;
        File::open(root.path().join("build"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
            .unwrap();

        let report = plan(root.path(), &policy).unwrap();
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].relative_path, "build");
        assert_eq!(report.excluded[0].reason, InspectionIssue::TooRecent);
        assert!(recent.is_file());
    }

    #[test]
    #[cfg(unix)]
    fn links_and_special_entries_are_never_sized_or_planned() {
        use std::os::unix::{fs::symlink, net::UnixListener};

        let root = tempdir().unwrap();
        let original = artifact(root.path(), "original/fixture.txt");
        artifact(root.path(), "build/report.txt");
        fs::create_dir(root.path().join("dist")).unwrap();
        symlink(root.path().join("original"), root.path().join("target")).unwrap();
        symlink(root.path().join("missing"), root.path().join("build/link")).unwrap();
        let _socket = UnixListener::bind(root.path().join("dist/socket")).unwrap();
        let policy = policy(&["target", "build", "dist"]);

        let measured = scan(root.path(), &policy).unwrap();
        assert_eq!(measured.total_bytes, 9);
        assert!(!measured.complete);
        let proposed = plan(root.path(), &policy).unwrap();
        assert!(proposed.candidates.is_empty());
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::LinkOrReparsePoint
        );
        assert_eq!(
            proposed.excluded[1].reason,
            InspectionIssue::LinkOrReparsePoint
        );
        assert_eq!(
            proposed.excluded[2].reason,
            InspectionIssue::UnsupportedType
        );
        assert!(original.is_file());
    }
}
