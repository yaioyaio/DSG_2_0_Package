//! Opt-in retention for files from one verified successful Playwright run.
//! The journal stores filesystem identities and a hash of the test-directory name.
//! Trace bytes and test labels are never read into the journal or returned in a report.
use super::*;
use crate::filesystem::{entry_digest, protected_entry, secure_gc_permissions, MountId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    time::{SystemTime, UNIX_EPOCH},
};

const TRACE: &str = "trace.zip";
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub struct PlaywrightEvidence {
    leaf: String,
    state: File,
    run: File,
}

fn run_name(name: &str) -> bool {
    name.strip_prefix("playwright-")
        .is_some_and(|s| s.len() == 32 && lower_hex(s))
}

fn lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn options(scope: &str, ttl: Duration) -> io::Result<i64> {
    require(scope.len() == 64 && lower_hex(scope))?;
    require(ttl >= Duration::from_secs(1) && ttl <= WEEK)?;
    i64::try_from(ttl.as_nanos()).map_err(|_| denied())
}

fn now_ns() -> io::Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| denied())?
            .as_nanos(),
    )
    .map_err(|_| denied())
}

fn directory(path: &Path, private: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    require(secure_gc_permissions(&file.metadata()?, private))?;
    Ok(file)
}

impl PlaywrightEvidence {
    pub fn create(state_dir: &Path, run_leaf: &str) -> io::Result<Self> {
        require(run_name(run_leaf))?;
        let state = directory(state_dir, false)?;
        let name = cname(OsStr::new(run_leaf))?;
        // A preexisting directory, even one with a valid-looking name, is never enrolled.
        if unsafe { libc::mkdirat(state.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let run = open_dir(&state, OsStr::new(run_leaf))?;
        require(secure_gc_permissions(&run.metadata()?, true))?;
        Ok(Self {
            leaf: run_leaf.to_owned(),
            state,
            run,
        })
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct Proof {
    state: String,
    run: String,
    output: String,
    case: String,
    case_label_hash: [u8; 32],
    file: [u8; 32],
    after_move: [u8; 32],
    bytes: u64,
}

fn mount(path: &Path) -> io::Result<MountId> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        MountId::read(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }
}

// The canonical layout contains exactly one test-directory component. Only inspect
// directory metadata and the literal trace.zip entry, never unrelated attachments.
struct RunDirectory {
    state: File,
    run: File,
    output: File,
    path: PathBuf,
    mount: MountId,
}

impl RunDirectory {
    fn open(state: &Path, leaf: &str) -> io::Result<Self> {
        require(run_name(leaf))?;
        let state_fd = directory(state, false)?;
        let run = open_dir(&state_fd, OsStr::new(leaf))?;
        let output = open_dir(&run, OsStr::new("output"))?;
        let path = state.join(leaf).join("output");
        let mount = mount(state)?;
        require(mount.contains(path.parent().ok_or_else(denied)?) && mount.contains(&path))?;
        require(
            secure_gc_permissions(&run.metadata()?, true)
                && secure_gc_permissions(&output.metadata()?, false),
        )?;
        require(
            identity(&fs::symlink_metadata(state)?) == identity(&state_fd.metadata()?)
                && identity(&fs::symlink_metadata(state.join(leaf))?) == identity(&run.metadata()?)
                && identity(&fs::symlink_metadata(&path)?) == identity(&output.metadata()?),
        )?;
        Ok(Self {
            state: state_fd,
            run,
            output,
            path,
            mount,
        })
    }

    fn candidates(&self, deadline: Instant) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        let policy = Policy::default();
        for (index, item) in fs::read_dir(&self.path)?.enumerate() {
            require(index < ENTRY_LIMIT && Instant::now() < deadline)?;
            let item = item?;
            let name = item.file_name();
            if name
                .to_str()
                .is_some_and(|s| !s.starts_with('.') && !protected_entry(&policy, s))
                && item.file_type()?.is_dir()
            {
                names.push(name);
            }
        }
        require(
            identity(&fs::symlink_metadata(&self.path)?) == identity(&self.output.metadata()?),
        )?;
        Ok(names)
    }

    fn inspect(&self, name: &OsStr) -> io::Result<Option<(File, Proof)>> {
        let case = open_dir(&self.output, name)?;
        let path = self.path.join(name);
        let case_meta = case.metadata()?;
        require(
            secure_gc_permissions(&case_meta, false)
                && self.mount.contains(&path)
                && identity(&fs::symlink_metadata(&path)?) == identity(&case_meta),
        )?;
        let leaf = OsStr::new(TRACE);
        let stat = match stat_child(&case, leaf) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        let file = path.join(leaf);
        let meta = fs::symlink_metadata(&file)?;
        require(
            meta.is_file()
                && meta.nlink() == 1
                && meta.len() <= BYTE_LIMIT
                && secure_gc_permissions(&meta, false)
                && self.mount.contains(&file)
                && meta.dev() == stat.st_dev as u64
                && meta.ino() == stat.st_ino as u64,
        )?;
        let proof = Proof {
            state: identity(&self.state.metadata()?),
            run: identity(&self.run.metadata()?),
            output: identity(&self.output.metadata()?),
            case: identity(&case_meta),
            case_label_hash: Sha256::digest(name.as_bytes()).into(),
            file: entry_digest(TRACE, &meta, false).map_err(|_| denied())?,
            after_move: entry_digest(TRACE, &meta, true).map_err(|_| denied())?,
            bytes: meta.len(),
        };
        Ok(Some((case, proof)))
    }
}

fn no_due() -> CleanupReport {
    CleanupReport {
        removed: 0,
        preserved: 0,
        reclaimed_bytes: 0,
        preserved_bytes: Some(0),
        reason: "no_expired_success_trace",
    }
}

impl CleanupQueue {
    /// Called only after the runner's all-passed attestation and durable terminal audit.
    pub fn register_success_traces(
        &self,
        scope: &str,
        evidence: &PlaywrightEvidence,
        ttl: Duration,
    ) -> io::Result<u32> {
        let ttl_ns = options(scope, ttl)?;
        let _worker = self.worker()?;
        let deadline = Instant::now() + WINDOW;
        let state = self.root.parent().ok_or_else(denied)?;
        let dir = RunDirectory::open(state, &evidence.leaf)?;
        require(
            identity(&evidence.state.metadata()?) == identity(&dir.state.metadata()?)
                && identity(&evidence.run.metadata()?) == identity(&dir.run.metadata()?),
        )?;
        let mut proofs = Vec::new();
        let mut total = 0u64;
        for name in dir.candidates(deadline)? {
            require(Instant::now() < deadline)?;
            if let Some((_, proof)) = dir.inspect(&name)? {
                total = total.checked_add(proof.bytes).ok_or_else(denied)?;
                require(total <= BYTE_LIMIT && proofs.len() < QUEUE_LIMIT as usize)?;
                proofs.push(serde_json::to_string(&proof).map_err(|_| denied())?);
            }
        }
        let transaction = self.db.unchecked_transaction().map_err(database)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM jobs WHERE kind='trace' AND leaf=?1)",
                [&evidence.leaf],
                |r| r.get(0),
            )
            .map_err(database)?;
        if exists {
            return Ok(0);
        }
        let active: i64 = transaction
            .query_row("SELECT count(*) FROM jobs WHERE state!='done'", [], |r| {
                r.get(0)
            })
            .map_err(database)?;
        require(active + proofs.len() as i64 <= QUEUE_LIMIT)?;
        let registered = now_ns()?;
        let expires = registered.checked_add(ttl_ns).ok_or_else(denied)?;
        for proof in &proofs {
            transaction.execute("INSERT INTO jobs(kind,state,owner,leaf,identity,reason) VALUES('trace','trace_ready',?1,?2,?3,'verified_success')",
                params![scope,evidence.leaf,proof]).map_err(database)?;
            transaction
                .execute(
                    "INSERT INTO success_trace_due(job,registered_ns,expires_ns) VALUES(?1,?2,?3)",
                    params![transaction.last_insert_rowid(), registered, expires],
                )
                .map_err(database)?;
        }
        transaction.commit().map_err(database)?;
        Ok(proofs.len() as u32)
    }

    /// Dispatch at most one previously verified trace for this opting-in project.
    /// A longer policy extends the hold. A shorter policy cannot shorten the original hold.
    pub fn expire_success_trace(&self, scope: &str, ttl: Duration) -> io::Result<CleanupReport> {
        let ttl_ns = options(scope, ttl)?;
        let _worker = self.worker()?;
        let now = now_ns()?;
        let record:Option<(i64,String,String)>=self.db.query_row(
            "SELECT j.id,j.leaf,j.identity FROM jobs j JOIN success_trace_due d ON d.job=j.id
             WHERE j.kind='trace' AND j.owner=?1 AND j.state='trace_ready' AND d.expires_ns<=?2 AND d.registered_ns<=?3
             ORDER BY d.last_attempt_ns,j.id LIMIT 1",
            params![scope,now,now.checked_sub(ttl_ns).ok_or_else(denied)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(database)?;
        let Some((id, leaf, encoded)) = record else {
            return Ok(no_due());
        };
        let proof: Proof = serde_json::from_str(&encoded).map_err(|_| denied())?;
        self.db
            .execute(
                "UPDATE success_trace_due SET last_attempt_ns=?2 WHERE job=?1",
                params![id, now],
            )
            .map_err(database)?;
        let deadline = Instant::now() + WINDOW;
        let source = self.root.parent().ok_or_else(denied)?;
        let locate = || -> io::Result<OsString> {
            let dir = RunDirectory::open(source, &leaf)?;
            let name = dir
                .candidates(deadline)?
                .into_iter()
                .find(|s| {
                    let hash: [u8; 32] = Sha256::digest(s.as_bytes()).into();
                    hash == proof.case_label_hash
                })
                .ok_or_else(denied)?;
            require(
                dir.inspect(&name)?
                    .is_some_and(|(_, observed)| observed == proof),
            )?;
            Ok(name)
        };
        let Ok(name) = locate() else {
            return self.close_trace(
                id,
                CleanupReport::kept("success_trace_changed_or_unverified"),
            );
        };
        let run_path = source.join(&leaf);
        let output = run_path.join("output");
        let case = output.join(&name);
        let file = case.join(TRACE);
        if idle_paths(
            &self.root,
            [run_path.as_path(), &output, &case, &file],
            deadline,
        )
        .is_err()
        {
            // Rotate within the queue. The current user of an older trace cannot starve others.
            self.db
                .execute(
                    "UPDATE jobs SET reason='success_trace_activity_unverified' WHERE id=?1",
                    [id],
                )
                .map_err(database)?;
            return Ok(CleanupReport::kept("success_trace_activity_unverified"));
        }
        let verified = || -> io::Result<File> {
            let dir = RunDirectory::open(source, &leaf)?;
            let (parent, observed) = dir.inspect(&name)?.ok_or_else(denied)?;
            require(observed == proof && Instant::now() < deadline)?;
            Ok(parent)
        };
        let Ok(parent) = verified() else {
            return self.close_trace(
                id,
                CleanupReport::kept("success_trace_changed_or_unverified"),
            );
        };
        let store = directory(&self.root, true)?;
        let staged = format!("staged-trace-{id}");
        self.db
            .execute(
                "UPDATE jobs SET state='removing',reason='success_trace_staging' WHERE id=?1",
                [id],
            )
            .map_err(database)?;
        if move_exclusive(&parent, OsStr::new(TRACE), &store, OsStr::new(&staged)).is_err() {
            return self.close_trace(id, CleanupReport::kept("success_trace_move_unconfirmed"));
        }
        // Moving may change ctime. All other file properties, including inode, must still
        // match. A crash or an uncertain staged entry remains private and journaled.
        let staged_path = self.root.join(&staged);
        let remove = || -> io::Result<()> {
            require(Instant::now() < deadline)?;
            idle_paths(&self.root, [staged_path.as_path()], deadline)?;
            let meta = fs::symlink_metadata(&staged_path)?;
            require(
                meta.is_file()
                    && meta.nlink() == 1
                    && secure_gc_permissions(&meta, false)
                    && entry_digest(TRACE, &meta, true).map_err(|_| denied())? == proof.after_move,
            )?;
            let observed = stat_child(&store, OsStr::new(&staged))?;
            require(
                observed.st_dev as u64 == meta.dev()
                    && observed.st_ino as u64 == meta.ino()
                    && observed.st_nlink == 1
                    && observed.st_size >= 0
                    && observed.st_size as u64 == proof.bytes
                    && observed.st_mode & libc::S_IFMT == libc::S_IFREG,
            )?;
            unlink(&store, OsStr::new(&staged), false)
        };
        if remove().is_err() {
            if move_exclusive(&store, OsStr::new(&staged), &parent, OsStr::new(TRACE)).is_ok() {
                return self
                    .close_trace(id, CleanupReport::kept("success_trace_restored_unverified"));
            }
            self.db
                .execute(
                    "UPDATE jobs SET reason='staged_trace_recovery_required' WHERE id=?1",
                    [id],
                )
                .map_err(database)?;
            return Ok(CleanupReport::kept("staged_trace_recovery_required"));
        }
        parent.sync_all()?;
        store.sync_all()?;
        self.close_trace(
            id,
            CleanupReport {
                removed: 1,
                preserved: 0,
                reclaimed_bytes: proof.bytes,
                preserved_bytes: Some(0),
                reason: "expired_success_trace_reclaimed",
            },
        )
    }

    fn close_trace(&self, id: i64, report: CleanupReport) -> io::Result<CleanupReport> {
        self.db
            .execute(
                "UPDATE jobs SET state='done',bytes=?2,reason=?3 WHERE id=?1",
                params![
                    id,
                    i64::try_from(report.reclaimed_bytes).map_err(|_| denied())?,
                    report.reason
                ],
            )
            .map_err(database)?;
        Ok(report)
    }
}

fn idle_paths<'a>(
    cwd: &Path,
    paths: impl IntoIterator<Item = &'a Path>,
    deadline: Instant,
) -> io::Result<()> {
    let tool = ["/usr/sbin/lsof", "/usr/bin/lsof", "/bin/lsof"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .ok_or_else(denied)?;
    let mut command = Command::new(tool);
    command.args(["-nP", "-Fp", "--"]).args(paths);
    let (status, output, diagnostic) = bounded(command, cwd, deadline)?;
    require(status == 1 && output.is_empty() && !diagnostic)
}

fn move_exclusive(from: &File, old: &OsStr, to: &File, new: &OsStr) -> io::Result<()> {
    let old = cname(old)?;
    let new = cname(new)?;
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            from.as_raw_fd(),
            old.as_ptr(),
            to.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            old.as_ptr(),
            to.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let status = {
        let _ = (from, old, to, new);
        -1
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const LEAF: &str = "playwright-0123456789abcdef0123456789abcdef";
    const SCOPE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_SCOPE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn registration_is_exclusive_and_requires_private_regular_trace() {
        let state = tempfile::tempdir().unwrap();
        let queue = CleanupQueue::open(state.path()).unwrap();
        let run = PlaywrightEvidence::create(state.path(), LEAF).unwrap();
        assert!(PlaywrightEvidence::create(state.path(), LEAF).is_err());
        let case = state.path().join(LEAF).join("output").join("case");
        fs::create_dir_all(&case).unwrap();
        let other = case.join("attachment");
        fs::write(&other, b"fixture").unwrap();
        symlink(&other, case.join(TRACE)).unwrap();
        assert!(queue
            .register_success_traces(SCOPE, &run, Duration::from_secs(1))
            .is_err());
        assert!(other.is_file());
    }

    #[test]
    fn unrelated_scope_longer_hold_and_replacement_keep_evidence() {
        let state = tempfile::tempdir().unwrap();
        let queue = CleanupQueue::open(state.path()).unwrap();
        let run = PlaywrightEvidence::create(state.path(), LEAF).unwrap();
        let case = state.path().join(LEAF).join("output").join("case");
        fs::create_dir_all(&case).unwrap();
        let trace = case.join(TRACE);
        fs::write(&trace, b"fixture").unwrap();
        let ttl = Duration::from_secs(1);
        assert_eq!(queue.register_success_traces(SCOPE, &run, ttl).unwrap(), 1);
        assert_eq!(queue.register_success_traces(SCOPE, &run, ttl).unwrap(), 0);
        assert_eq!(
            queue.expire_success_trace(SCOPE, ttl).unwrap().reason,
            "no_expired_success_trace"
        );
        let prior = now_ns().unwrap() - 2_000_000_000;
        queue
            .db
            .execute(
                "UPDATE success_trace_due SET registered_ns=?1,expires_ns=?1",
                [prior],
            )
            .unwrap();
        assert_eq!(
            queue.expire_success_trace(OTHER_SCOPE, ttl).unwrap().reason,
            "no_expired_success_trace"
        );
        assert_eq!(
            queue.expire_success_trace(SCOPE, WEEK).unwrap().reason,
            "no_expired_success_trace"
        );
        fs::rename(&trace, case.join("retained")).unwrap();
        fs::write(&trace, b"new data").unwrap();
        let report = queue.expire_success_trace(SCOPE, ttl).unwrap();
        assert_eq!(
            (report.removed, report.preserved, report.reclaimed_bytes),
            (0, 1, 0)
        );
        assert!(trace.is_file());
        assert!(case.join("retained").is_file());
        assert!(queue.expire_success_trace(SCOPE, Duration::ZERO).is_err());
        assert!(queue.expire_success_trace(SCOPE, WEEK + ttl).is_err());
    }
}
