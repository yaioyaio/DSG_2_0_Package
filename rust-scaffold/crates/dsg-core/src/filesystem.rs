use crate::policy::{is_direct_child, Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, DirEntry, Metadata, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::{open_metadata, resolve_inspection_root, ObservedMetadata};

#[cfg(not(windows))]
type ObservedMetadata = Metadata;
#[cfg(not(windows))]
use fs::symlink_metadata as open_metadata;
#[cfg(not(windows))]
use resolve_root as resolve_inspection_root;

pub(crate) const MAX_DIRECTORY_DEPTH: usize = 64;
pub(crate) type EntryProof = HashMap<FileIdentity, [u8; 32]>;

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

#[derive(Debug, Deserialize, Serialize)]
pub struct GcPlan {
    pub project_id: String,
    pub candidates: Vec<Candidate>,
    pub excluded: Vec<ExcludedCandidate>,
    pub dry_run: bool,
    pub observed_at_unix: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub relative_path: String,
    pub bytes: u64,
    pub modified_at_unix: u64,
    pub modified_at_subsec_nanos: u32,
    pub identity: Option<FileIdentity>,
    pub subtree_digest: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExcludedCandidate {
    pub relative_path: String,
    pub reason: InspectionIssue,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionIssue {
    ProtectedName,
    InvalidOutputName,
    UnsupportedEntryName,
    LinkOrReparsePoint,
    MountBoundary,
    SharedFile,
    SharedOrUnowned,
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
    let (root, root_metadata) = resolve_inspection_root(root)?;
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
            Ok(Some(output)) if seen_outputs.insert(output.key()) => inspect_tree(
                &output,
                &root_metadata,
                policy,
                &mut remaining,
                InspectionOptions::scan(),
            ),
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
    let (root, root_metadata) = resolve_inspection_root(root)?;
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
            candidate(
                name,
                output,
                &root_metadata,
                policy,
                &mut remaining,
                now,
                InspectionOptions::candidate(None, None),
            )
            .map(Some)
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
    fn from_metadata(metadata: &ObservedMetadata) -> Result<Self, InspectionIssue> {
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
    #[cfg(windows)]
    _pin: std::sync::Arc<fs::File>,
    path: PathBuf,
    snapshot: DirectorySnapshot,
    entry_digest: Option<[u8; 32]>,
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
        self.verified_metadata().map(|_| ())
    }

    fn verified_metadata(&self) -> Result<ObservedMetadata, InspectionIssue> {
        let metadata =
            open_metadata(&self.path).map_err(|_| InspectionIssue::MetadataUnavailable)?;
        if is_link_or_reparse_point(&metadata) {
            return Err(InspectionIssue::LinkOrReparsePoint);
        }
        if !metadata.is_dir() || DirectorySnapshot::from_metadata(&metadata)? != self.snapshot {
            return Err(InspectionIssue::ChangedDuringInspection);
        }
        Ok(metadata)
    }

    fn read(self) -> Result<OpenDirectory, InspectionIssue> {
        self.verify()?;
        let entries = fs::read_dir(&self.path).map_err(|_| InspectionIssue::MetadataUnavailable)?;
        self.verify()?;
        Ok(OpenDirectory {
            directory: self,
            entries,
            child_digests: Vec::new(),
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn output_identity(
    root: &Path,
    name: &str,
    policy: &Policy,
    mount: &MountId,
) -> Option<FileIdentity> {
    let (root, metadata) = resolve_root(root).ok()?;
    let output = inspect_output(&root, &metadata, name, policy).ok()??;
    if !mount.contains(&output.path) {
        return None;
    }
    let metadata = output.verified_metadata().ok()?;
    if !secure_gc_permissions(&metadata, false) {
        return None;
    }
    output.snapshot.identity
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn measure_gc_candidate(
    root: &Path,
    parent: &Path,
    name: &str,
    policy: &Policy,
    mount: &MountId,
    proof: Option<&mut EntryProof>,
) -> Result<Candidate, InspectionIssue> {
    let (root, root_metadata) =
        resolve_root(root).map_err(|_| InspectionIssue::MetadataUnavailable)?;
    if !parent.starts_with(&root) || !mount.contains(parent) {
        return Err(InspectionIssue::MountBoundary);
    }
    let output = inspect_output(parent, &root_metadata, name, policy)?
        .ok_or(InspectionIssue::MetadataUnavailable)?;
    let mut remaining = policy.max_scan_files;
    candidate(
        name,
        output,
        &root_metadata,
        policy,
        &mut remaining,
        SystemTime::now(),
        InspectionOptions::candidate(Some(mount), proof),
    )
}

struct OpenDirectory {
    directory: Directory,
    entries: ReadDir,
    child_digests: Vec<[u8; 32]>,
}

fn inspect_output(
    root: &Path,
    root_metadata: &ObservedMetadata,
    name: &str,
    policy: &Policy,
) -> Result<Option<Directory>, InspectionIssue> {
    if !is_direct_child(name) {
        return Err(InspectionIssue::InvalidOutputName);
    }
    if protected_entry(policy, name) {
        return Err(InspectionIssue::ProtectedName);
    }
    let path = root.join(name);
    let metadata = match open_metadata(&path) {
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
    if protected_entry(policy, canonical_name) {
        return Err(InspectionIssue::ProtectedName);
    }
    Ok(Some(Directory {
        #[cfg(windows)]
        _pin: metadata.pin.clone(),
        path: canonical,
        snapshot: DirectorySnapshot::from_metadata(&metadata)?,
        entry_digest: None,
    }))
}

fn candidate(
    name: &str,
    mut output: Directory,
    root_metadata: &ObservedMetadata,
    policy: &Policy,
    remaining: &mut usize,
    now: SystemTime,
    options: InspectionOptions<'_>,
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
    let metadata = output.verified_metadata()?;
    if options.mount.is_some() && !secure_gc_permissions(&metadata, false) {
        return Err(InspectionIssue::SharedOrUnowned);
    }
    output.entry_digest = Some(entry_digest("", &metadata, true)?);
    let inspection = inspect_tree(&output, root_metadata, policy, remaining, options);
    if let Some(issue) = inspection.issues.first() {
        return Err(*issue);
    }
    Ok(Candidate {
        relative_path: name.to_owned(),
        bytes: inspection.bytes,
        modified_at_unix: modified.as_secs(),
        modified_at_subsec_nanos: modified.subsec_nanos(),
        identity: snapshot.identity,
        subtree_digest: inspection
            .subtree_digest
            .ok_or(InspectionIssue::MetadataUnavailable)?,
        reason: "expired_generated_output".to_owned(),
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Scan,
    Candidate,
}

struct InspectionOptions<'a> {
    mode: Mode,
    mount: Option<&'a MountId>,
    proof: Option<&'a mut EntryProof>,
}

impl<'a> InspectionOptions<'a> {
    fn scan() -> Self {
        Self {
            mode: Mode::Scan,
            mount: None,
            proof: None,
        }
    }

    fn candidate(mount: Option<&'a MountId>, proof: Option<&'a mut EntryProof>) -> Self {
        Self {
            mode: Mode::Candidate,
            mount,
            proof,
        }
    }

    fn record_entry(&mut self, identity: Option<FileIdentity>, digest: Option<[u8; 32]>) {
        if let (Some(proof), Some(identity), Some(digest)) =
            (self.proof.as_deref_mut(), identity, digest)
        {
            proof.insert(identity, digest);
        }
    }
}

#[derive(Default)]
struct Inspection {
    bytes: u64,
    files: usize,
    issues: Vec<InspectionIssue>,
    subtree_digest: Option<String>,
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
    root_metadata: &ObservedMetadata,
    policy: &Policy,
    remaining: &mut usize,
    mut options: InspectionOptions<'_>,
) -> Inspection {
    let mode = options.mode;
    let mut result = Inspection::default();
    if options
        .mount
        .is_some_and(|expected| !expected.contains(&output.path))
    {
        result.record_issue(InspectionIssue::MountBoundary, mode);
        return result;
    }
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
            let finished = stack.pop().expect("current directory is on the stack");
            if let Some(entry_digest) = finished.directory.entry_digest {
                let digest = directory_digest(entry_digest, finished.child_digests);
                match stack.last_mut() {
                    Some(parent) => parent.child_digests.push(digest),
                    None => {
                        result.subtree_digest = Some(format!(
                            "{:x}",
                            sha2::digest::Output::<Sha256>::from(digest)
                        ))
                    }
                }
            }
            continue;
        };
        if *remaining == 0 {
            result.record_issue(InspectionIssue::ScanLimitReached, mode);
            break;
        }
        *remaining -= 1;
        let inspected = entry
            .map_err(|_| InspectionIssue::MetadataUnavailable)
            .and_then(|entry| {
                inspect_child(entry, &current.directory, root_metadata, policy, &options)
            });
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
                    options.record_entry(directory.snapshot.identity, directory.entry_digest);
                    match directory.read() {
                        Ok(open) => {
                            stack.push(open);
                            continue;
                        }
                        Err(issue) => issue,
                    }
                }
            }
            Ok(Child::File {
                bytes,
                identity,
                digest,
            }) => {
                result.files += 1;
                options.record_entry(identity, digest);
                if let Some(digest) = digest {
                    current.child_digests.push(digest);
                }
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
    File {
        bytes: u64,
        identity: Option<FileIdentity>,
        digest: Option<[u8; 32]>,
    },
}

fn inspect_child(
    entry: DirEntry,
    parent: &Directory,
    root_metadata: &ObservedMetadata,
    policy: &Policy,
    options: &InspectionOptions<'_>,
) -> Result<Child, InspectionIssue> {
    let name = entry.file_name();
    let name = name.to_str().ok_or(InspectionIssue::UnsupportedEntryName)?;
    if protected_entry(policy, name) {
        return Err(InspectionIssue::ProtectedName);
    }
    #[cfg(windows)]
    let metadata =
        open_metadata(&entry.path()).map_err(|_| InspectionIssue::MetadataUnavailable)?;
    #[cfg(not(windows))]
    let metadata = entry
        .metadata()
        .map_err(|_| InspectionIssue::MetadataUnavailable)?;
    check_boundary(&metadata, root_metadata, parent.snapshot.identity)?;
    if options
        .mount
        .is_some_and(|expected| !expected.contains(&entry.path()))
    {
        return Err(InspectionIssue::MountBoundary);
    }
    if options.mount.is_some() && !secure_gc_permissions(&metadata, false) {
        return Err(InspectionIssue::SharedOrUnowned);
    }
    if metadata.is_file() {
        if options.mode == Mode::Candidate && shared_file(&metadata) {
            return Err(InspectionIssue::SharedFile);
        }
        let digest = (options.mode == Mode::Candidate)
            .then(|| entry_digest(name, &metadata, false))
            .transpose()?;
        return Ok(Child::File {
            bytes: metadata.len(),
            identity: identity(&metadata),
            digest,
        });
    }
    if metadata.is_dir() {
        return Ok(Child::Directory(Directory {
            #[cfg(windows)]
            _pin: metadata.pin.clone(),
            path: entry.path(),
            snapshot: DirectorySnapshot::from_metadata(&metadata)?,
            entry_digest: (options.mode == Mode::Candidate)
                .then(|| entry_digest(name, &metadata, false))
                .transpose()?,
        }));
    }
    Err(InspectionIssue::UnsupportedType)
}

pub(crate) fn entry_digest(
    name: &str,
    metadata: &ObservedMetadata,
    root: bool,
) -> Result<[u8; 32], InspectionIssue> {
    let mut hash = Sha256::new();
    hash.update(b"DSG metadata entry v1");
    hash.update((name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    hash.update([u8::from(metadata.is_dir())]);
    if metadata.is_file() {
        hash.update(metadata.len().to_le_bytes());
    }
    let modified = metadata
        .modified()
        .map_err(|_| InspectionIssue::MetadataUnavailable)?;
    let (negative, duration) = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => (false, duration),
        Err(error) => (true, error.duration()),
    };
    hash.update([u8::from(negative)]);
    hash.update(duration.as_secs().to_le_bytes());
    hash.update(duration.subsec_nanos().to_le_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash.update(metadata.dev().to_le_bytes());
        hash.update(metadata.ino().to_le_bytes());
        hash.update(metadata.mode().to_le_bytes());
        hash.update(metadata.uid().to_le_bytes());
        hash.update(metadata.gid().to_le_bytes());
        if !root {
            // Moving the approved top directory can change its ctime.
            hash.update(metadata.ctime().to_le_bytes());
            hash.update(metadata.ctime_nsec().to_le_bytes());
        }
    }
    #[cfg(windows)]
    {
        hash.update(metadata.id.device.to_le_bytes());
        hash.update(metadata.id.inode.to_le_bytes());
        hash.update(metadata.links.to_le_bytes());
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(hash.finalize().into())
}

fn directory_digest(entry: [u8; 32], mut children: Vec<[u8; 32]>) -> [u8; 32] {
    // At most one 32-byte digest per visited child is retained across open frames.
    children.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"DSG metadata directory v1");
    hash.update(entry);
    for child in children {
        hash.update(child);
    }
    hash.finalize().into()
}

#[cfg(unix)]
pub(crate) fn shared_file(metadata: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() != 1
}

#[cfg(windows)]
fn shared_file(metadata: &ObservedMetadata) -> bool {
    metadata.links != 1
}

#[cfg(not(any(unix, windows)))]
fn shared_file(_: &Metadata) -> bool {
    false
}

pub(crate) fn protected_entry(policy: &Policy, name: &str) -> bool {
    policy.protects(name)
        || name.eq_ignore_ascii_case(".dsg")
        || name.eq_ignore_ascii_case(".dsg.json")
}

#[cfg(unix)]
pub(crate) fn secure_gc_permissions(metadata: &Metadata, private: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    let forbidden = if private { 0o077 } else { 0o022 };
    // Getting the effective owner ID does not dereference any pointers.
    metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & forbidden == 0
}

#[cfg(not(unix))]
fn secure_gc_permissions(_: &Metadata, _: bool) -> bool {
    false
}

#[derive(Eq, PartialEq)]
pub(crate) struct MountId {
    #[cfg(target_os = "linux")]
    id: u64,
    #[cfg(target_os = "macos")]
    name: [libc::c_char; 1024],
}

impl MountId {
    #[cfg(target_os = "linux")]
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
        let path = CString::new(path.as_os_str().as_bytes())?;
        let mut info = MaybeUninit::<libc::statx>::zeroed();
        // The path and writable statx storage outlive the call.
        let status = unsafe {
            libc::statx(
                libc::AT_FDCWD,
                path.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                libc::STATX_MNT_ID,
                info.as_mut_ptr(),
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        if info.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(Self {
            id: info.stx_mnt_id,
        })
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
        let path = CString::new(path.as_os_str().as_bytes())?;
        let mut info = MaybeUninit::<libc::statfs>::zeroed();
        // statfs returns volume metadata; no filesystem source field is used.
        if unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        if info.f_flags & libc::MNT_LOCAL as u32 == 0 || info.f_mntonname[0] == 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let mut name = info.f_mntonname;
        if let Some(end) = name.iter().position(|byte| *byte == 0) {
            name[end..].fill(0);
        } else {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(Self { name })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn read(_: &Path) -> io::Result<Self> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        Self::read(path).is_ok_and(|current| current == *self)
    }
}

fn check_boundary(
    metadata: &ObservedMetadata,
    root_metadata: &ObservedMetadata,
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
pub(crate) fn identity(metadata: &Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn identity(metadata: &ObservedMetadata) -> Option<FileIdentity> {
    Some(metadata.id)
}

#[cfg(not(any(unix, windows)))]
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
        #[cfg(any(unix, windows))]
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
    #[test]
    #[cfg(windows)]
    fn windows_directory_is_pinned_until_inspection_finishes() {
        let root = tempdir().unwrap();
        let file = artifact(root.path(), "build/report.txt");
        let (path, root_metadata) = resolve_inspection_root(root.path()).unwrap();
        let policy = policy(&["build"]);
        let directory = inspect_output(&path, &root_metadata, "build", &policy)
            .unwrap()
            .unwrap();
        let before = directory.snapshot.identity.unwrap();
        let opened = directory.read().unwrap();
        let moved = path.join("saved");
        let build = path.join("build");
        assert!(fs::rename(&build, &moved).is_err());
        assert!(!moved.exists());
        assert!(file.is_file());
        assert_eq!(opened.directory.verified_metadata().unwrap().id, before);
        drop(opened);
        drop(root_metadata);
        fs::rename(&build, &moved).unwrap();
        fs::create_dir(&build).unwrap();
        let replacement = open_metadata(&build).unwrap();
        assert_ne!(replacement.id, before);
    }

    #[test]
    #[cfg(windows)]
    fn windows_subtree_digest_identifies_equal_metadata_file_replacement() {
        use std::fs::FileTimes;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };

        fn restore_time(path: &Path, modified: SystemTime) {
            fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        }
        let root = tempdir().unwrap();
        let file = artifact(root.path(), "build/report.txt");
        let build = file.parent().unwrap();
        let file_time = fs::metadata(&file).unwrap().modified().unwrap();
        let dir_time = fs::metadata(build).unwrap().modified().unwrap();
        let policy = policy(&["build"]);
        let before = plan(root.path(), &policy).unwrap().candidates.remove(0);
        fs::rename(&file, root.path().join("old.txt")).unwrap();
        fs::write(&file, b"generated").unwrap();
        restore_time(&file, file_time);
        restore_time(build, dir_time);
        let after = plan(root.path(), &policy).unwrap().candidates.remove(0);
        assert_eq!(before.identity, after.identity);
        assert_eq!(before.bytes, after.bytes);
        assert_eq!(
            before.modified_at_subsec_nanos,
            after.modified_at_subsec_nanos
        );
        assert_eq!(before.modified_at_unix, after.modified_at_unix);
        assert_ne!(before.subtree_digest, after.subtree_digest);
    }

    #[test]
    #[cfg(windows)]
    fn windows_plan_excludes_hard_linked_file() {
        let root = tempdir().unwrap();
        let file = artifact(root.path(), "build/report.txt");
        let original = root.path().join("original.txt");
        fs::hard_link(&file, &original).unwrap();
        let proposed = plan(root.path(), &policy(&["build"])).unwrap();
        assert!(proposed.candidates.is_empty());
        assert_eq!(proposed.excluded[0].reason, InspectionIssue::SharedFile);
        assert!(original.is_file() && file.is_file());
    }

    #[test]
    #[cfg(windows)]
    fn windows_scan_and_plan_preserve_junction_targets() {
        let root = tempdir().unwrap();
        let external = tempdir().unwrap();
        let sentinel = artifact(external.path(), "outside.txt");
        let junction = root.path().join("build");
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/j"])
            .arg(&junction)
            .arg(external.path())
            .output()
            .unwrap();
        assert!(result.status.success());
        let policy = policy(&["build"]);
        let observed = scan(root.path(), &policy).unwrap();
        let proposed = plan(root.path(), &policy).unwrap();
        assert!(!observed.complete);
        assert_eq!(observed.total_bytes, 0);
        assert!(proposed.candidates.is_empty());
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::LinkOrReparsePoint
        );
        fs::remove_dir(junction).unwrap();
        assert!(sentinel.is_file());
    }

    #[test]
    #[cfg(windows)]
    fn windows_scan_and_plan_fail_closed_on_sharing_conflicts() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = tempdir().unwrap();
        let output = artifact(root.path(), "build/report.txt");
        let _exclusive = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&output)
            .unwrap();
        let policy = policy(&["build"]);
        let observed = scan(root.path(), &policy).unwrap();
        let proposed = plan(root.path(), &policy).unwrap();
        assert!(!observed.complete);
        assert_eq!(observed.total_bytes, 0);
        assert!(proposed.candidates.is_empty());
        assert_eq!(
            proposed.excluded[0].reason,
            InspectionIssue::MetadataUnavailable
        );
        assert!(output.is_file());
    }
}
