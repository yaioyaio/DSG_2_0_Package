use crate::policy::Policy;
use serde::Serialize;
use serde_json::Value;
use std::{io, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GcStatus {
    Applied,
    Preserved,
    RecoveryRequired,
}

#[derive(Debug, Serialize)]
pub struct GcReport {
    pub status: GcStatus,
    pub reclaimed_bytes: u64,
    pub reason: &'static str,
}

impl GcReport {
    fn preserved(reason: &'static str) -> Self {
        Self {
            status: GcStatus::Preserved,
            reclaimed_bytes: 0,
            reason,
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use supported::{apply, ownership, recover};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn ownership(_: &Path, _: &Policy, _: &str) -> io::Result<String> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn apply(_: &Path, _: &Policy, _: i64, _: &str, _: &str, _: &Value) -> GcReport {
    GcReport::preserved("filesystem_gc_unsupported")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn recover(_: &Path, _: &Policy, _: i64, _: &str, _: &str, _: &Value) -> GcReport {
    GcReport::preserved("filesystem_gc_unsupported")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supported {
    use super::*;
    use crate::filesystem::{
        self, Candidate, EntryProof, FileIdentity, GcPlan, MountId, MAX_DIRECTORY_DEPTH,
    };
    use crate::policy::is_direct_child;
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::ffi::{CStr, CString, OsStr};
    use std::fs::{self, File, OpenOptions};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::path::PathBuf;

    pub fn ownership(root: &Path, policy: &Policy, relative: &str) -> io::Result<String> {
        if !is_registered_output(policy, relative) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let project = Project::open(root)?;
        let mut registration_policy = policy.clone();
        registration_policy.gc_min_age_hours = 0;
        let candidate = filesystem::measure_gc_candidate(
            &project.root.path,
            &project.root.path,
            relative,
            &registration_policy,
            &project.mount,
            None,
        )
        .map_err(|_| unknown())?;
        let identity = candidate.identity.ok_or_else(unknown)?;
        if !project.root.verify(&project.mount) {
            return Err(unknown());
        }
        identity_digest(&identity)
    }

    /// The caller must hold the registered project's exclusive cleanup lease.
    pub fn apply(
        root: &Path,
        policy: &Policy,
        plan_id: i64,
        relative: &str,
        identity: &str,
        approved_payload: &Value,
    ) -> GcReport {
        let Some(approved) =
            approved_candidate(policy, plan_id, relative, identity, approved_payload)
        else {
            return GcReport::preserved("gc_approval_mismatch");
        };
        let Ok(project) = Project::open(root) else {
            return GcReport::preserved("gc_project_boundary_unverified");
        };
        let original = project.root.path.join(relative);
        if filesystem::output_identity(&project.root.path, relative, policy, &project.mount)
            != approved.identity
        {
            return GcReport::preserved("gc_original_identity_changed");
        }
        if !project.matches(&project.root.path, policy, &approved, None) {
            return GcReport::preserved("gc_approved_snapshot_changed");
        }
        let Ok(quarantine) = Quarantine::open(&project, plan_id, true) else {
            return GcReport::preserved("gc_quarantine_unavailable");
        };
        let staged = quarantine.plan.path.join(relative);
        if !quarantine.verify(&project) || !is_absent(&staged) {
            return GcReport::preserved("gc_quarantine_occupied_or_unverified");
        }
        if rename_noreplace(&project.root, &quarantine.plan, relative).is_err() {
            return GcReport::preserved("gc_quarantine_rename_failed");
        }

        let mut proof = EntryProof::new();
        if !quarantine.verify(&project)
            || !is_absent(&original)
            || !project.matches(&quarantine.plan.path, policy, &approved, Some(&mut proof))
            || !quarantine.verify(&project)
            || !is_absent(&original)
        {
            return recovery_required("gc_quarantined_snapshot_unverified", 0);
        }
        let mut removal = Removal {
            project: &project,
            policy,
            proof,
            reclaimed_bytes: 0,
        };
        let result =
            removal.remove_directory(&quarantine.plan, OsStr::new(relative), approved.identity, 1);
        if result.is_err() || !removal.proof.is_empty() || removal.reclaimed_bytes != approved.bytes
        {
            return recovery_required("gc_quarantine_removal_incomplete", removal.reclaimed_bytes);
        }
        quarantine.discard_empty_plan(&project);
        GcReport {
            status: GcStatus::Applied,
            reclaimed_bytes: removal.reclaimed_bytes,
            reason: "gc_artifacts_reclaimed",
        }
    }

    /// Recover an intact staged artifact. The caller must hold the cleanup lease.
    pub fn recover(
        root: &Path,
        policy: &Policy,
        plan_id: i64,
        relative: &str,
        identity: &str,
        approved_payload: &Value,
    ) -> GcReport {
        let Some(approved) =
            approved_candidate(policy, plan_id, relative, identity, approved_payload)
        else {
            return recovery_required("gc_approval_mismatch", 0);
        };
        let Ok(project) = Project::open(root) else {
            return recovery_required("gc_project_boundary_unverified", 0);
        };
        let Ok(quarantine) = Quarantine::open(&project, plan_id, false) else {
            return recovery_required("gc_quarantine_unavailable", 0);
        };
        let original = project.root.path.join(relative);
        if !is_absent(&original) {
            return recovery_required("gc_original_occupied_or_unverified", 0);
        }
        if !quarantine.verify(&project)
            || !project.matches(&quarantine.plan.path, policy, &approved, None)
            || !quarantine.verify(&project)
        {
            return recovery_required("gc_quarantined_snapshot_unverified", 0);
        }
        if rename_noreplace(&quarantine.plan, &project.root, relative).is_err() {
            return recovery_required("gc_restore_rename_failed", 0);
        }
        if !project.root.verify(&project.mount)
            || !project.matches(&project.root.path, policy, &approved, None)
        {
            return recovery_required("gc_restored_snapshot_unverified", 0);
        }
        quarantine.discard_empty_plan(&project);
        GcReport::preserved("gc_artifacts_restored")
    }

    fn recovery_required(reason: &'static str, reclaimed_bytes: u64) -> GcReport {
        GcReport {
            status: GcStatus::RecoveryRequired,
            reclaimed_bytes,
            reason,
        }
    }

    fn is_registered_output(policy: &Policy, relative: &str) -> bool {
        is_direct_child(relative)
            && !filesystem::protected_entry(policy, relative)
            && Policy::default()
                .scan_dirs
                .iter()
                .any(|name| name == relative)
            && policy.scan_dirs.iter().any(|name| name == relative)
    }

    fn approved_candidate(
        policy: &Policy,
        plan_id: i64,
        relative: &str,
        identity: &str,
        payload: &Value,
    ) -> Option<Candidate> {
        if plan_id <= 0 || !is_registered_output(policy, relative) || !is_digest(identity) {
            return None;
        }
        let plan = GcPlan::deserialize(payload).ok()?;
        if !plan.dry_run || plan.project_id != policy.project_id {
            return None;
        }
        let mut matches = plan
            .candidates
            .into_iter()
            .filter(|candidate| candidate.relative_path == relative);
        let candidate = matches.next()?;
        if matches.next().is_some() || !is_digest(&candidate.subtree_digest) {
            return None;
        }
        if identity_digest(&candidate.identity?).ok()? != identity {
            return None;
        }
        Some(candidate)
    }

    fn is_digest(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    fn identity_digest(identity: &FileIdentity) -> io::Result<String> {
        let bytes = serde_json::to_vec(identity).map_err(io::Error::other)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    fn is_absent(path: &Path) -> bool {
        fs::symlink_metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    }

    fn unknown() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    struct Project {
        root: Anchor,
        mount: MountId,
    }

    impl Project {
        fn open(root: &Path) -> io::Result<Self> {
            let path = filesystem::canonical_root(root)?;
            let mount = MountId::read(&path)?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            let root = Anchor::new(path, directory, false, &mount)?;
            Ok(Self { root, mount })
        }

        fn matches(
            &self,
            parent: &Path,
            policy: &Policy,
            approved: &Candidate,
            proof: Option<&mut EntryProof>,
        ) -> bool {
            filesystem::measure_gc_candidate(
                &self.root.path,
                parent,
                &approved.relative_path,
                policy,
                &self.mount,
                proof,
            )
            .is_ok_and(|current| current == *approved)
        }
    }

    struct Quarantine {
        storage: Anchor,
        quarantine: Anchor,
        plan: Anchor,
    }

    impl Quarantine {
        fn open(project: &Project, plan_id: i64, create: bool) -> io::Result<Self> {
            let storage = project
                .root
                .child(OsStr::new(".dsg"), create, false, &project.mount)?;
            let quarantine =
                storage.child(OsStr::new("quarantine"), create, true, &project.mount)?;
            let plan_name = plan_id.to_string();
            let plan = quarantine.child(OsStr::new(&plan_name), create, true, &project.mount)?;
            Ok(Self {
                storage,
                quarantine,
                plan,
            })
        }

        fn verify(&self, project: &Project) -> bool {
            project.root.verify(&project.mount)
                && self.storage.verify(&project.mount)
                && self.quarantine.verify(&project.mount)
                && self.plan.verify(&project.mount)
        }

        fn discard_empty_plan(&self, project: &Project) {
            if !self.verify(project) {
                return;
            }
            let Some(name) = self.plan.path.file_name() else {
                return;
            };
            if let Ok(name) = CString::new(name.as_bytes()) {
                // The kernel retains any nonempty quarantine.
                let _ = unlink(&self.quarantine, &name, libc::AT_REMOVEDIR);
            }
        }
    }

    struct Anchor {
        path: PathBuf,
        directory: File,
        identity: FileIdentity,
        private: bool,
    }

    impl Anchor {
        fn new(path: PathBuf, directory: File, private: bool, mount: &MountId) -> io::Result<Self> {
            let identity = filesystem::identity(&directory.metadata()?).ok_or_else(unknown)?;
            let anchor = Self {
                path,
                directory,
                identity,
                private,
            };
            if !anchor.verify(mount) {
                return Err(unknown());
            }
            Ok(anchor)
        }

        fn verify(&self, mount: &MountId) -> bool {
            fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
                metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && filesystem::identity(&metadata) == Some(self.identity)
                    && filesystem::secure_gc_permissions(&metadata, self.private)
                    && mount.contains(&self.path)
            })
        }

        fn child(
            &self,
            child: &OsStr,
            create: bool,
            private: bool,
            mount: &MountId,
        ) -> io::Result<Self> {
            let name = CString::new(child.as_bytes())?;
            if !self.verify(mount) {
                return Err(unknown());
            }
            if create {
                // A relative, single-component name and this open parent stay live.
                let status =
                    unsafe { libc::mkdirat(self.directory.as_raw_fd(), name.as_ptr(), 0o700) };
                if status != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
            }
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // The successful openat returned a new owned descriptor.
            let directory = unsafe { File::from_raw_fd(fd) };
            let anchor = Self::new(self.path.join(child), directory, private, mount)?;
            if anchor.identity.device != self.identity.device {
                return Err(unknown());
            }
            Ok(anchor)
        }
    }

    fn rename_noreplace(source: &Anchor, target: &Anchor, relative: &str) -> io::Result<()> {
        let name = CString::new(relative)?;
        let source_fd = source.directory.as_raw_fd();
        let target_fd = target.directory.as_raw_fd();
        // Both directories and the one-component name remain alive. The kernel
        // rejects a rename if the destination appeared after the earlier check.
        #[cfg(target_os = "linux")]
        let status = unsafe {
            // The kernel interface is also available with musl builds whose libc
            // lacks the renameat2 wrapper. Unsupported calls still fail closed.
            libc::syscall(
                libc::SYS_renameat2,
                libc::c_long::from(source_fd),
                name.as_ptr(),
                libc::c_long::from(target_fd),
                name.as_ptr(),
                libc::c_long::from(libc::RENAME_NOREPLACE),
            )
        };
        #[cfg(target_os = "macos")]
        let status = unsafe {
            libc::renameatx_np(
                source_fd,
                name.as_ptr(),
                target_fd,
                name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if status != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    struct Removal<'a> {
        project: &'a Project,
        policy: &'a Policy,
        proof: EntryProof,
        reclaimed_bytes: u64,
    }

    impl Removal<'_> {
        fn remove_directory(
            &mut self,
            parent: &Anchor,
            name: &OsStr,
            approved: Option<FileIdentity>,
            depth: usize,
        ) -> io::Result<()> {
            if depth > MAX_DIRECTORY_DEPTH {
                return Err(unknown());
            }
            let directory = parent.child(name, false, false, &self.project.mount)?;
            if Some(directory.identity) != approved {
                return Err(unknown());
            }
            for entry in fs::read_dir(&directory.path)? {
                let entry = entry?;
                if !directory.verify(&self.project.mount) {
                    return Err(unknown());
                }
                let child = entry.file_name();
                let relative = child.to_str().ok_or_else(unknown)?;
                if filesystem::protected_entry(self.policy, relative) {
                    return Err(unknown());
                }
                let metadata = entry.metadata()?;
                if metadata.file_type().is_symlink()
                    || !filesystem::secure_gc_permissions(&metadata, false)
                    || !self.project.mount.contains(&entry.path())
                {
                    return Err(unknown());
                }
                let identity = filesystem::identity(&metadata).ok_or_else(unknown)?;
                if identity.device != self.project.root.identity.device {
                    return Err(unknown());
                }
                let digest =
                    filesystem::entry_digest(relative, &metadata, false).map_err(|_| unknown())?;
                if self.proof.remove(&identity) != Some(digest) {
                    return Err(unknown());
                }
                if metadata.is_dir() {
                    self.remove_directory(&directory, &child, Some(identity), depth + 1)?;
                } else if metadata.is_file() && !filesystem::shared_file(&metadata) {
                    unlink(&directory, &CString::new(child.as_bytes())?, 0)?;
                    self.reclaimed_bytes += metadata.len();
                } else {
                    return Err(unknown());
                }
            }
            if !directory.verify(&self.project.mount) || !parent.verify(&self.project.mount) {
                return Err(unknown());
            }
            unlink(parent, &CString::new(name.as_bytes())?, libc::AT_REMOVEDIR)
        }
    }

    fn unlink(parent: &Anchor, name: &CStr, flags: libc::c_int) -> io::Result<()> {
        // The verified parent descriptor is held and name is a single entry.
        if unsafe { libc::unlinkat(parent.directory.as_raw_fd(), name.as_ptr(), flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::registry::{AuditOutcome, GcApplyStatus, Registry, RegistryError};
        use std::io::Write;
        use std::os::unix::{fs::symlink, process::ExitStatusExt};
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};
        use tempfile::{tempdir, TempDir};

        const PLAN_ID: i64 = 3;

        struct Fixture {
            root: TempDir,
            policy: Policy,
            identity: String,
            plan: Value,
        }

        impl Fixture {
            fn new() -> Self {
                let root = tempdir().unwrap();
                let policy = Policy {
                    scan_dirs: vec!["build".into(), "dist".into()],
                    gc_min_age_hours: 0,
                    ..Policy::default()
                };
                fs::create_dir_all(root.path().join("build/nested")).unwrap();
                fs::write(root.path().join("build/nested/file"), b"artifact").unwrap();
                fs::create_dir(root.path().join("dist")).unwrap();
                fs::write(root.path().join("dist/fixture"), b"separate").unwrap();
                let identity = ownership(root.path(), &policy, "build").unwrap();
                let draft = filesystem::plan(root.path(), &policy).unwrap();
                assert_eq!(draft.candidates.len(), 2);
                let plan = serde_json::to_value(draft).unwrap();
                Self {
                    root,
                    policy,
                    identity,
                    plan,
                }
            }

            fn apply(&self) -> GcReport {
                apply(
                    self.root.path(),
                    &self.policy,
                    PLAN_ID,
                    "build",
                    &self.identity,
                    &self.plan,
                )
            }

            fn recover(&self) -> GcReport {
                recover(
                    self.root.path(),
                    &self.policy,
                    PLAN_ID,
                    "build",
                    &self.identity,
                    &self.plan,
                )
            }

            fn stage(&self) -> PathBuf {
                stage_output(self.root.path(), PLAN_ID)
            }
        }

        fn stage_output(root: &Path, plan_id: i64) -> PathBuf {
            let project = Project::open(root).unwrap();
            let quarantine = Quarantine::open(&project, plan_id, true).unwrap();
            rename_noreplace(&project.root, &quarantine.plan, "build").unwrap();
            quarantine.plan.path.join("build")
        }

        struct OwnedChild(Child);

        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if !matches!(self.0.try_wait(), Ok(Some(_))) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
        }

        #[test]
        fn crash_after_quarantine_recovers_file_and_committed_audit() {
            const CHILD_MODE: &str = "__dsg_quarantine_crash_fixture_child";
            let scope = "c".repeat(64);
            let policy_digest = "a".repeat(64);
            let approval_digest = "b".repeat(64);

            // The test harness accepts a skip filter. This private filter selects the
            // child role without an environment hook or any change to the CLI.
            if std::env::args_os().any(|arg| arg == CHILD_MODE) {
                let (project_id, plan_id): (i64, i64) =
                    serde_json::from_reader(io::stdin().lock()).unwrap();
                let mut registry = Registry::open(Path::new("state")).unwrap();
                let lease = registry
                    .acquire_lease(project_id, &scope, Duration::from_secs(1), |_| false)
                    .unwrap();
                registry
                    .attach_lease_process(&lease, std::process::id())
                    .unwrap();
                registry
                    .claim_gc(&lease, plan_id, &policy_digest, &approval_digest)
                    .unwrap();
                stage_output(Path::new("."), plan_id);
                std::thread::sleep(Duration::from_secs(10));
                panic!("parent missed the quarantine boundary");
            }

            let fixture = Fixture::new();
            let root = fixture.root.path();
            let state = root.join("state");
            let file = root.join("build/nested/file");
            let identity = filesystem::identity(&file.metadata().unwrap()).unwrap();
            let expected = Sha256::digest(fs::read(&file).unwrap());
            let mut registry = Registry::open(&state).unwrap();
            let project_id = registry.register(root, "crash-fixture", 1).unwrap().id;
            let ttl = Duration::from_secs(60);
            let plan_id = registry
                .save_plan(project_id, &policy_digest, &fixture.plan, ttl)
                .unwrap()
                .plan_id;
            registry
                .register_gc_output(project_id, "build", &fixture.identity)
                .unwrap();
            registry
                .approve_gc(
                    project_id,
                    plan_id,
                    "build",
                    &policy_digest,
                    &fixture.identity,
                    &approval_digest,
                    ttl,
                )
                .unwrap();
            drop(registry);

            let staged = root
                .join(".dsg/quarantine")
                .join(plan_id.to_string())
                .join("build/nested/file");
            let test_name =
                "gc::supported::tests::crash_after_quarantine_recovers_file_and_committed_audit";
            let mut child = OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", test_name, "--skip", CHILD_MODE])
                    .current_dir(root)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let pid = child.0.id();
            let mut input = child.0.stdin.take().unwrap();
            write!(input, "[{project_id},{plan_id}]").unwrap();
            drop(input);
            let deadline = Instant::now() + Duration::from_secs(8);
            while !staged.is_file() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before staging"
                );
                assert!(Instant::now() < deadline, "quarantine deadline expired");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!file.exists());
            child.0.kill().unwrap();
            assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGKILL));

            let mut registry = Registry::open(&state).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let lease = loop {
                match registry.acquire_lease(project_id, &scope, ttl, |recorded| recorded == pid) {
                    Ok(lease) => break lease,
                    Err(RegistryError::LeaseConflict) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    _ => panic!("could not acquire recovery lease"),
                }
            };
            let claim = registry.pending_gc(&lease, plan_id).unwrap();
            let restored = recover(
                root,
                &fixture.policy,
                plan_id,
                &claim.relative_path,
                &claim.identity,
                &claim.payload,
            );
            assert_eq!(restored.reason, "gc_artifacts_restored");
            assert_eq!(restored.status, GcStatus::Preserved);
            assert_eq!(restored.reclaimed_bytes, 0);
            registry
                .finish_gc(
                    &lease,
                    plan_id,
                    GcApplyStatus::Preserved,
                    restored.reclaimed_bytes,
                )
                .unwrap();
            assert!(matches!(
                registry.pending_gc(&lease, plan_id),
                Err(RegistryError::GcStateConflict)
            ));
            registry
                .finish_lease(lease, AuditOutcome::Incomplete, "GC_LEASE_RELEASED")
                .unwrap();
            drop(registry);
            assert_eq!(
                filesystem::identity(&file.metadata().unwrap()),
                Some(identity)
            );
            assert_eq!(Sha256::digest(fs::read(file).unwrap()), expected);
            assert!(!staged.exists());

            let db = rusqlite::Connection::open_with_flags(
                state.join("registry.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let integrity: String = db
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            let violations: i64 = db
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(violations, 0);
            let status: (String, i64) = db
                .query_row(
                    "SELECT gc_plan.status, gc_application.bytes_applied FROM gc_plan
                 JOIN gc_application ON gc_plan.id=gc_application.plan_id WHERE gc_plan.id=?1",
                    [plan_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, ("preserved".into(), 0));
            let mut query = db
                .prepare(
                    "SELECT reason_code FROM audit_event
                 WHERE plan_id=?1 AND action='gc_apply' ORDER BY id",
                )
                .unwrap();
            let events: Vec<String> = query
                .query_map([plan_id], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(events, ["GC_APPLY_INTENT", "GC_PRESERVED"]);
        }

        #[test]
        fn reclaims_one_verified_artifact_and_preserves_the_other() {
            let fixture = Fixture::new();
            let report = fixture.apply();

            assert_eq!(report.status, GcStatus::Applied, "{}", report.reason);
            assert_eq!(report.reclaimed_bytes, 8);
            assert!(!fixture.root.path().join("build").exists());
            assert!(fixture.root.path().join("dist/fixture").is_file());
        }

        #[test]
        fn deep_identity_name_and_mtime_are_rechecked() {
            let fixture = Fixture::new();
            let directory = fixture.root.path().join("build/nested");
            let file = directory.join("file");
            File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
                .unwrap();
            assert_eq!(fixture.apply().status, GcStatus::Preserved);
            let renamed = directory.join("renamed");
            fs::rename(file, &renamed).unwrap();
            assert_eq!(fixture.apply().status, GcStatus::Preserved);
            assert!(renamed.is_file());
            assert!(fixture.root.path().join("dist/fixture").is_file());
        }

        #[test]
        fn protects_new_shared_or_protected_entries_before_quarantine() {
            let fixture = Fixture::new();
            let nested = fixture.root.path().join("build/nested");
            let shared = fixture.root.path().join("shared");
            fs::hard_link(nested.join("file"), &shared).unwrap();
            assert_eq!(fixture.apply().status, GcStatus::Preserved);
            assert!(shared.is_file());
            assert!(nested.join("file").is_file());

            let protected = nested.join("Data/original");
            fs::create_dir_all(protected.parent().unwrap()).unwrap();
            fs::write(&protected, b"synthetic").unwrap();
            let plan = filesystem::plan(fixture.root.path(), &fixture.policy).unwrap();
            assert!(plan
                .candidates
                .iter()
                .all(|item| item.relative_path != "build"));
            assert_eq!(fixture.apply().status, GcStatus::Preserved);
            assert!(protected.is_file());
        }

        #[test]
        fn restores_exact_quarantine_and_does_not_overwrite() {
            let fixture = Fixture::new();
            let staged = fixture.stage();
            assert!(staged.join("nested/file").is_file());
            let original = fixture.root.path().join("build");
            fs::create_dir(&original).unwrap();
            let project = Project::open(fixture.root.path()).unwrap();
            let quarantine = Quarantine::open(&project, PLAN_ID, false).unwrap();
            assert!(rename_noreplace(&quarantine.plan, &project.root, "build").is_err());
            assert_eq!(fixture.recover().status, GcStatus::RecoveryRequired);
            assert!(original.is_dir());
            assert!(staged.join("nested/file").is_file());
            fs::remove_dir(&original).unwrap();

            let report = fixture.recover();
            assert_eq!(report.status, GcStatus::Preserved, "{}", report.reason);
            assert_eq!(report.reason, "gc_artifacts_restored");
            assert_eq!(report.reclaimed_bytes, 0);
            assert!(original.join("nested/file").is_file());
            assert!(!staged.exists());
        }

        #[test]
        fn quarantine_links_and_unmatched_staged_contents_are_preserved() {
            let fixture = Fixture::new();
            let outside = tempdir().unwrap();
            symlink(outside.path(), fixture.root.path().join(".dsg")).unwrap();
            assert_eq!(fixture.apply().status, GcStatus::Preserved);
            assert!(fixture.root.path().join("build/nested/file").is_file());
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);

            let other = Fixture::new();
            let staged = other.stage();
            fs::write(staged.join("nested/other"), b"fixture").unwrap();
            assert_eq!(other.recover().status, GcStatus::RecoveryRequired);
            assert!(staged.join("nested/file").is_file());
            assert!(staged.join("nested/other").is_file());
            assert!(!other.root.path().join("build").exists());
        }
    }
}
