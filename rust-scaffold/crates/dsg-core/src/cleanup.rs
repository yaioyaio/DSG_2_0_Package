//! Bounded, local cleanup of outputs created by the managed entry points.
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
pub enum CleanupEvent {
    Success,
    Failure,
    Interrupted,
}

#[derive(Debug, Serialize)]
pub struct CleanupReport {
    pub removed: u32,
    pub preserved: u32,
    pub reclaimed_bytes: u64,
    pub preserved_bytes: Option<u64>,
    pub reason: &'static str,
}

impl CleanupReport {
    fn kept(reason: &'static str) -> Self {
        Self {
            removed: 0,
            preserved: 1,
            reclaimed_bytes: 0,
            preserved_bytes: None,
            reason,
        }
    }
}

pub struct CargoOutput {
    pub target: PathBuf,
    #[cfg(unix)]
    id: i64,
    #[cfg(unix)]
    live: std::fs::File,
}

#[cfg(unix)]
pub use local::{CleanupQueue, PlaywrightEvidence};

#[cfg(not(unix))]
pub struct PlaywrightEvidence;

#[cfg(not(unix))]
impl PlaywrightEvidence {
    pub fn create(_: &Path, _: &str) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(not(unix))]
pub struct CleanupQueue;

#[cfg(not(unix))]
impl CleanupQueue {
    pub fn open(_: &Path) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn register_success_traces(
        &self,
        _: &str,
        _: &PlaywrightEvidence,
        _: std::time::Duration,
    ) -> io::Result<u32> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn expire_success_trace(
        &self,
        _: &str,
        _: std::time::Duration,
    ) -> io::Result<CleanupReport> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn prepare_cargo(&self) -> io::Result<CargoOutput> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn complete_cargo(&self, _: CargoOutput, _: CleanupEvent) -> io::Result<CleanupReport> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn create_worktree(&self, _: &Path, _: &str, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn complete_git(&self, _: &Path, _: &str) -> io::Result<CleanupReport> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(unix)]
mod local {
    use super::*;
    use crate::policy::Policy;
    use fs2::FileExt;
    use rusqlite::{params, Connection, OptionalExtension};
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        ffi::{CString, OsStr},
        fs::{self, DirBuilder, File, Metadata, OpenOptions},
        io::Read,
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            io::{AsRawFd, FromRawFd},
            process::CommandExt,
        },
        path::Component,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    mod success_trace;
    pub use success_trace::PlaywrightEvidence;

    const ENTRY_LIMIT: usize = 4096;
    const BYTE_LIMIT: u64 = 256 << 20;
    const QUEUE_LIMIT: i64 = 64;
    const OUTPUT_LIMIT: usize = 1 << 20;
    const CHILD_MEMORY_LIMIT: u64 = 512 << 20;
    const WINDOW: Duration = Duration::from_secs(20);

    fn denied() -> io::Error {
        io::Error::other("cleanup prerequisites could not be confirmed")
    }
    fn database(_: rusqlite::Error) -> io::Error {
        io::Error::other("cleanup journal unavailable")
    }
    fn require(value: bool) -> io::Result<()> {
        if value {
            Ok(())
        } else {
            Err(denied())
        }
    }
    fn private_dir(path: &Path) -> io::Result<()> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(path)?;
        require(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        )
    }

    fn identity(meta: &Metadata) -> String {
        format!("{}:{}", meta.dev(), meta.ino())
    }

    pub struct CleanupQueue {
        root: PathBuf,
        db: Connection,
    }

    impl CleanupQueue {
        pub fn open(state_dir: &Path) -> io::Result<Self> {
            require(fs::symlink_metadata(state_dir)?.is_dir())?;
            let root = state_dir.canonicalize()?.join("managed-cleanup");
            private_dir(&root)?;
            // The only readable data files in this directory are private regular journal files.
            for name in [
                "jobs.sqlite",
                "jobs.sqlite-journal",
                "jobs.sqlite-wal",
                "jobs.sqlite-shm",
            ] {
                match fs::symlink_metadata(root.join(name)) {
                    Ok(m) => require(
                        m.is_file() && m.nlink() == 1 && m.uid() == unsafe { libc::geteuid() },
                    )?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            let db = Connection::open(root.join("jobs.sqlite")).map_err(database)?;
            db.busy_timeout(Duration::from_secs(1)).map_err(database)?;
            db.execute_batch("PRAGMA synchronous=FULL;
                CREATE TABLE IF NOT EXISTS jobs(
                    id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL DEFAULT '', leaf TEXT NOT NULL DEFAULT '', branch TEXT NOT NULL DEFAULT '',
                    identity TEXT NOT NULL DEFAULT '', head TEXT NOT NULL DEFAULT '',
                    bytes INTEGER NOT NULL DEFAULT 0, reason TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state,id);
                CREATE INDEX IF NOT EXISTS jobs_trace_leaf ON jobs(leaf) WHERE kind='trace';
                CREATE INDEX IF NOT EXISTS jobs_inflight ON jobs(id) WHERE state!='done';
                CREATE TABLE IF NOT EXISTS success_trace_due(
                    job INTEGER PRIMARY KEY REFERENCES jobs(id), registered_ns INTEGER NOT NULL,
                    expires_ns INTEGER NOT NULL, last_attempt_ns INTEGER NOT NULL DEFAULT 0
                );").map_err(database)?;
            let queue = Self { root, db };
            // Recover at most one interrupted success dispatch. An interrupted destructive operation
            // stays journaled for inspection, as its remaining ownership is uncertain.
            if let Ok(_worker) = queue.worker() {
                queue.reconcile()?;
                let pending: Option<i64> = queue.db.query_row(
                    "SELECT id FROM jobs WHERE kind='cargo' AND state='pending' ORDER BY id LIMIT 1",
                    [], |r| r.get(0)).optional().map_err(database)?;
                if let Some(id) = pending {
                    queue.clean_cargo(id)?;
                }
            }
            Ok(queue)
        }

        fn reconcile(&self) -> io::Result<()> {
            // The queue cap also bounds recovery. A dead supervisor is never proof that its
            // descendants have exited: retain its output and record why it needs inspection.
            let mut query = self.db.prepare(
                "SELECT id,state,identity FROM jobs WHERE kind='cargo' AND state IN ('creating','active','removing','preserved') ORDER BY id LIMIT ?1"
            ).map_err(database)?;
            let rows: Vec<(i64, String, String)> = query
                .query_map([QUEUE_LIMIT], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(database)?
                .collect::<Result<_, _>>()
                .map_err(database)?;
            drop(query);
            for (id, state, key) in rows {
                let path = self.target(id);
                let outcome = match fs::symlink_metadata(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        Some(("done", "previous_output_absent"))
                    }
                    Ok(_) if state == "creating" || state == "removing" => {
                        Some(("preserved", "interrupted_dispatch"))
                    }
                    Ok(meta) if state == "active" && meta.is_dir() && identity(&meta) == key => {
                        let directory = OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                            .open(path)?;
                        if directory.try_lock_exclusive().is_ok() {
                            Some(("preserved", "supervisor_ended"))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((next, reason)) = outcome {
                    self.db
                        .execute(
                            "UPDATE jobs SET state=?2,reason=?3 WHERE id=?1 AND state=?4",
                            params![id, next, reason, state],
                        )
                        .map_err(database)?;
                }
            }
            Ok(())
        }

        fn worker(&self) -> io::Result<File> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.root.join("worker.lock"))?;
            let m = file.metadata()?;
            require(m.is_file() && m.nlink() == 1 && m.uid() == unsafe { libc::geteuid() })?;
            file.try_lock_exclusive()?;
            Ok(file)
        }

        fn reserve(&self, kind: &str, owner: &str, leaf: &str, branch: &str) -> io::Result<i64> {
            let changed = self.db.execute(
                "INSERT INTO jobs(kind,state,owner,leaf,branch) SELECT ?1,'creating',?2,?3,?4
                 WHERE (SELECT count(*) FROM jobs WHERE state!='done') < ?5
                 AND (?1!='git' OR NOT EXISTS(SELECT 1 FROM jobs WHERE kind='git' AND owner=?2 AND state!='done'))",
                params![kind, owner, leaf, branch, QUEUE_LIMIT]).map_err(database)?;
            require(changed == 1)?;
            Ok(self.db.last_insert_rowid())
        }

        fn target(&self, id: i64) -> PathBuf {
            self.root.join(format!("cargo-{id}"))
        }

        pub fn prepare_cargo(&self) -> io::Result<CargoOutput> {
            let _worker = self.worker()?;
            let id = self.reserve("cargo", "", "", "")?;
            let target = self.target(id);
            DirBuilder::new().mode(0o700).create(&target)?;
            File::open(&self.root)?.sync_all()?;
            let live = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&target)?;
            live.try_lock_exclusive()?;
            let key = identity(&live.metadata()?);
            self.db
                .execute(
                    "UPDATE jobs SET state='active',identity=?2 WHERE id=?1",
                    params![id, key],
                )
                .map_err(database)?;
            Ok(CargoOutput { target, id, live })
        }

        pub fn complete_cargo(
            &self,
            output: CargoOutput,
            event: CleanupEvent,
        ) -> io::Result<CleanupReport> {
            require(output.target == self.target(output.id))?;
            let (state, reason) = match event {
                CleanupEvent::Success => ("pending", "success"),
                CleanupEvent::Failure => ("preserved", "failure_evidence"),
                CleanupEvent::Interrupted => ("preserved", "interruption_evidence"),
            };
            let changed = self.db.execute("UPDATE jobs SET state=?2,reason=?3 WHERE id=?1 AND kind='cargo' AND state='active'",
                params![output.id,state,reason]).map_err(database)?;
            require(changed == 1)?;
            drop(output.live);
            if state == "preserved" {
                let key: String = self
                    .db
                    .query_row(
                        "SELECT identity FROM jobs WHERE id=?1",
                        [output.id],
                        |row| row.get(0),
                    )
                    .map_err(database)?;
                let mut report = CleanupReport::kept(reason);
                if fs::symlink_metadata(&output.target).is_ok_and(|meta| identity(&meta) == key) {
                    report.preserved_bytes =
                        Inventory::scan(&output.target, false, Instant::now() + WINDOW)
                            .ok()
                            .map(|inventory| inventory.bytes);
                }
                return Ok(report);
            }
            match self.worker() {
                Ok(_worker) => self.clean_cargo(output.id),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    Ok(CleanupReport::kept("queued"))
                }
                Err(e) => Err(e),
            }
        }

        fn finish(&self, id: i64, report: CleanupReport) -> io::Result<CleanupReport> {
            self.db
                .execute(
                    "UPDATE jobs SET state=?2,bytes=?3,reason=?4 WHERE id=?1",
                    params![
                        id,
                        if report.removed == 1 {
                            "done"
                        } else {
                            "preserved"
                        },
                        i64::try_from(report.reclaimed_bytes).map_err(|_| denied())?,
                        report.reason
                    ],
                )
                .map_err(database)?;
            Ok(report)
        }

        fn clean_cargo(&self, id: i64) -> io::Result<CleanupReport> {
            let (state, key, bytes): (String, String, i64) = self
                .db
                .query_row(
                    "SELECT state,identity,bytes FROM jobs WHERE kind='cargo' AND id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(database)?;
            if state == "done" {
                return Ok(CleanupReport {
                    removed: 1,
                    preserved: 0,
                    reclaimed_bytes: u64::try_from(bytes).map_err(|_| denied())?,
                    preserved_bytes: Some(0),
                    reason: "cargo_reclaimed",
                });
            }
            if state != "pending" {
                return Ok(CleanupReport::kept("previously_dispatched"));
            }
            let deadline = Instant::now() + WINDOW;
            let root = self.target(id);
            if let Ok(token) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&root)
            {
                if token.try_lock_exclusive().is_err() {
                    return Ok(CleanupReport::kept("queued"));
                }
            }
            let inspection = (|| {
                require(identity(&fs::symlink_metadata(&root)?) == key)?;
                let inventory = Inventory::scan(&root, false, deadline)?;
                unused(&self.root, &root, deadline)?;
                require(inventory == Inventory::scan(&root, false, deadline)?)?;
                Ok::<_, io::Error>(inventory)
            })();
            let Ok(inventory) = inspection else {
                return self.finish(
                    id,
                    CleanupReport::kept("ownership_activity_or_limit_unconfirmed"),
                );
            };
            self.db
                .execute("UPDATE jobs SET state='removing' WHERE id=?1", [id])
                .map_err(database)?;
            let mut reclaimed = 0;
            let result = inventory.remove_cargo(&self.root, &root, deadline, &mut reclaimed);
            self.finish(
                id,
                if result.is_ok() {
                    CleanupReport {
                        removed: 1,
                        preserved: 0,
                        reclaimed_bytes: reclaimed,
                        preserved_bytes: Some(0),
                        reason: "cargo_reclaimed",
                    }
                } else {
                    CleanupReport {
                        removed: 0,
                        preserved: 1,
                        reclaimed_bytes: reclaimed,
                        preserved_bytes: None,
                        reason: "removal_interrupted",
                    }
                },
            )
        }

        pub fn create_worktree(
            &self,
            repo: &Path,
            branch: &str,
            destination: &Path,
        ) -> io::Result<()> {
            let _worker = self.worker()?;
            let deadline = Instant::now() + WINDOW;
            let repo = primary(repo, deadline)?;
            require(valid_feature(branch))?;
            let dest_parent = destination.parent().ok_or_else(denied)?.canonicalize()?;
            require(Some(dest_parent.as_path()) == repo.parent())?;
            let leaf = destination
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(denied)?;
            require(safe_label(leaf) && !Policy::default().protects(leaf))?;
            let dest = dest_parent.join(leaf);
            require(
                matches!(fs::symlink_metadata(&dest),Err(e) if e.kind()==io::ErrorKind::NotFound),
            )?;
            let head = revision(&repo, &format!("refs/heads/{branch}^{{commit}}"), deadline)?;
            tracked(&repo, &head, deadline)?;
            let id = self.reserve(
                "git",
                &identity(&fs::symlink_metadata(&repo)?),
                leaf,
                branch,
            )?;
            let creation = git_paths(
                &repo,
                &[
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("--"),
                    dest.as_os_str(),
                    OsStr::new(branch),
                ],
                deadline,
            );
            if let Err(error) = creation {
                if matches!(fs::symlink_metadata(&dest),Err(e) if e.kind()==io::ErrorKind::NotFound)
                {
                    self.db
                        .execute(
                            "UPDATE jobs SET state='done',reason='creation_rejected' WHERE id=?1",
                            [id],
                        )
                        .map_err(database)?;
                }
                return Err(error);
            }
            let key = identity(&fs::symlink_metadata(&dest)?);
            self.db
                .execute(
                    "UPDATE jobs SET state='active',identity=?2,head=?3 WHERE id=?1",
                    params![id, key, String::from_utf8(head).map_err(|_| denied())?],
                )
                .map_err(database)?;
            Ok(())
        }

        pub fn complete_git(&self, repo: &Path, feature: &str) -> io::Result<CleanupReport> {
            require(valid_feature(feature))?;
            let _worker = self.worker()?;
            let deadline = Instant::now() + WINDOW;
            let repo = primary(repo, deadline)?;
            let owner = identity(&fs::symlink_metadata(&repo)?);
            let job: Option<(i64,String,String,String,String)> = self.db.query_row(
                "SELECT id,leaf,identity,head,state FROM jobs WHERE kind='git' AND owner=?1 AND branch=?2 AND state!='done' LIMIT 1",
                params![owner,feature], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(database)?;
            let Some((id, leaf, key, base, state)) = job else {
                return Ok(CleanupReport {
                    removed: 0,
                    preserved: 0,
                    reclaimed_bytes: 0,
                    preserved_bytes: Some(0),
                    reason: "no_registered_worktree",
                });
            };
            if !matches!(state.as_str(), "active" | "retry") {
                return Ok(CleanupReport::kept("worktree_recovery_required"));
            }
            require(safe_label(&leaf))?;
            let dest = repo.parent().ok_or_else(denied)?.join(leaf);
            let inspect = (|| {
                require(
                    fs::symlink_metadata(&dest)?.is_dir()
                        && identity(&fs::symlink_metadata(&dest)?) == key,
                )?;
                require(!std::env::current_dir()?.canonicalize()?.starts_with(&dest))?;
                let head = git_text(&dest, &["rev-parse", "--verify", "HEAD"], deadline)?;
                let local = format!("refs/heads/{feature}");
                require(git_text(&dest, &["symbolic-ref", "HEAD"], deadline)? == local.as_bytes())?;
                require(head == revision(&repo, &format!("{local}^{{commit}}"), deadline)?)?;
                require(!base.is_empty())?;
                git_text(
                    &dest,
                    &["merge-base", "--is-ancestor", &base, "HEAD"],
                    deadline,
                )?;
                let tree = revision(&repo, &format!("{local}^{{tree}}"), deadline)?;
                for name in [
                    "refs/heads/main".to_owned(),
                    "refs/heads/develop".into(),
                    "refs/remotes/origin/main".into(),
                    "refs/remotes/origin/develop".into(),
                    format!("refs/remotes/origin/{feature}"),
                ] {
                    require(tree == revision(&repo, &format!("{name}^{{tree}}"), deadline)?)?;
                }
                // The remote feature must retain this exact commit after the checkout disappears.
                require(
                    head == revision(
                        &repo,
                        &format!("refs/remotes/origin/{feature}^{{commit}}"),
                        deadline,
                    )?,
                )?;
                registered_unlocked(&repo, &dest, &head, feature, deadline)?;
                let paths = tracked(&dest, b"HEAD", deadline)?;
                let inventory = Inventory::scan(&dest, true, deadline)?;
                require(inventory.entries.keys().all(|p| {
                    p.as_os_str().is_empty() || p == Path::new(".git") || paths.contains(p)
                }))?;
                let flags = git_text(&dest, &["ls-files", "-v", "-z"], deadline)?;
                require(
                    flags
                        .split(|b| *b == 0)
                        .filter(|s| !s.is_empty())
                        .all(|s| s.starts_with(b"H ")),
                )?;
                require(
                    git_text(
                        &dest,
                        &[
                            "status",
                            "--porcelain=v1",
                            "-z",
                            "--ignored",
                            "--untracked-files=all",
                            "--ignore-submodules=none",
                        ],
                        deadline,
                    )?
                    .is_empty(),
                )?;
                unused(&repo, &dest, deadline)?;
                require(inventory == Inventory::scan(&dest, true, deadline)?)?;
                registered_unlocked(&repo, &dest, &head, feature, deadline)?;
                Ok::<_, io::Error>(inventory.bytes)
            })();
            let Ok(bytes) = inspect else {
                self.db.execute("UPDATE jobs SET state='retry',reason='worktree_checks_unconfirmed' WHERE id=?1",[id]).map_err(database)?;
                return Ok(CleanupReport::kept("worktree_checks_unconfirmed"));
            };
            self.db
                .execute("UPDATE jobs SET state='removing' WHERE id=?1", [id])
                .map_err(database)?;
            let result = git_paths(
                &repo,
                &[
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    OsStr::new("--"),
                    dest.as_os_str(),
                ],
                deadline,
            );
            self.finish(id, if result.is_ok() && matches!(fs::symlink_metadata(&dest),Err(e) if e.kind()==io::ErrorKind::NotFound) {
                CleanupReport { removed:1,preserved:0,reclaimed_bytes:bytes,preserved_bytes:Some(0),reason:"worktree_reclaimed" }
            } else { CleanupReport::kept("worktree_remove_unconfirmed") })
        }
    }

    fn safe_label(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 100
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            && !s.contains("..")
    }
    fn valid_feature(s: &str) -> bool {
        let Some((date, label)) = s.strip_prefix("feature/").and_then(|s| s.split_once('-')) else {
            return false;
        };
        date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) && safe_label(label)
    }
    fn primary(repo: &Path, deadline: Instant) -> io::Result<PathBuf> {
        let repo = repo.canonicalize()?;
        require(fs::symlink_metadata(repo.join(".git"))?.is_dir())?;
        require(
            git_text(&repo, &["rev-parse", "--show-toplevel"], deadline)?
                == repo.as_os_str().as_bytes(),
        )?;
        Ok(repo)
    }
    fn revision(repo: &Path, spec: &str, deadline: Instant) -> io::Result<Vec<u8>> {
        let v = git_text(repo, &["rev-parse", "--verify", spec], deadline)?;
        require(matches!(v.len(), 40 | 64) && v.iter().all(u8::is_ascii_hexdigit))?;
        Ok(v)
    }
    fn tracked(repo: &Path, rev: &[u8], deadline: Instant) -> io::Result<BTreeSet<PathBuf>> {
        let rev = std::str::from_utf8(rev).map_err(|_| denied())?;
        let records = git_text(repo, &["ls-tree", "-r", "-l", "-z", rev], deadline)?;
        let policy = Policy::default();
        let mut bytes = 0u64;
        let mut paths = BTreeSet::new();
        for record in records.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let sep = record.iter().position(|b| *b == b'\t').ok_or_else(denied)?;
            let header = std::str::from_utf8(&record[..sep]).map_err(|_| denied())?;
            let fields: Vec<_> = header.split_ascii_whitespace().collect();
            require(
                fields.len() == 4
                    && matches!(fields[0], "100644" | "100755")
                    && fields[1] == "blob",
            )?;
            bytes = bytes
                .checked_add(fields[3].parse().map_err(|_| denied())?)
                .ok_or_else(denied)?;
            let path = Path::new(OsStr::from_bytes(&record[sep + 1..]));
            for component in path.components() {
                let Component::Normal(name) = component else {
                    return Err(denied());
                };
                require(!policy.protects(name.to_str().ok_or_else(denied)?))?;
            }
            paths.extend(
                path.ancestors()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf),
            );
            require(bytes <= BYTE_LIMIT && paths.len() < ENTRY_LIMIT && Instant::now() < deadline)?;
        }
        Ok(paths)
    }
    fn registered_unlocked(
        repo: &Path,
        dest: &Path,
        head: &[u8],
        feature: &str,
        deadline: Instant,
    ) -> io::Result<()> {
        let data = git_text(repo, &["worktree", "list", "--porcelain", "-z"], deadline)?;
        let mut records = data.split(|b| *b == 0);
        let branch = format!("branch refs/heads/{feature}");
        while let Some(path) = records.next() {
            let matching = path.strip_prefix(b"worktree ") == Some(dest.as_os_str().as_bytes());
            let mut commit_matches = false;
            let mut branch_matches = false;
            let mut unlocked = true;
            for field in records.by_ref().take_while(|v| !v.is_empty()) {
                commit_matches |= field.strip_prefix(b"HEAD ") == Some(head);
                branch_matches |= field == branch.as_bytes();
                unlocked &= !field.starts_with(b"locked") && !field.starts_with(b"prunable");
            }
            if matching {
                return require(commit_matches && branch_matches && unlocked);
            }
        }
        Err(denied())
    }

    fn git_text(repo: &Path, args: &[&str], deadline: Instant) -> io::Result<Vec<u8>> {
        let args: Vec<_> = args.iter().map(OsStr::new).collect();
        let mut data = git_paths(repo, &args, deadline)?;
        if data.last() == Some(&b'\n') {
            data.pop();
        }
        Ok(data)
    }
    fn git_paths(repo: &Path, args: &[&OsStr], deadline: Instant) -> io::Result<Vec<u8>> {
        let mut cmd = Command::new("/usr/bin/git");
        cmd.args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "submodule.recurse=false",
        ])
        .args(args);
        let (status, out, _) = bounded(cmd, repo, deadline)?;
        require(status == 0)?;
        Ok(out)
    }
    fn unused(cwd: &Path, path: &Path, deadline: Instant) -> io::Result<()> {
        let tool = ["/usr/sbin/lsof", "/usr/bin/lsof", "/bin/lsof"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
            .ok_or_else(denied)?;
        let mut cmd = Command::new(tool);
        cmd.args(["-nP", "-Fp", "+D"]).arg(path);
        let (status, out, err) = bounded(cmd, cwd, deadline)?;
        require(status == 1 && out.is_empty() && !err)
    }

    // Never expose diagnostic output. Read both pipes without blocking, cap the child group
    // and terminate only this invocation and its children on timeout or excess output.
    fn bounded(
        mut cmd: Command,
        cwd: &Path,
        deadline: Instant,
    ) -> io::Result<(i32, Vec<u8>, bool)> {
        require(Instant::now() < deadline)?;
        let deadline = deadline.min(Instant::now() + Duration::from_secs(6));
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        unsafe {
            cmd.pre_exec(|| {
                macro_rules! cap {
                    ($resource:expr,$value:expr) => {{
                        let mut r = libc::rlimit {
                            rlim_cur: 0,
                            rlim_max: 0,
                        };
                        if libc::getrlimit($resource, &mut r) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        let value = r.rlim_cur.min($value);
                        let limit = libc::rlimit {
                            rlim_cur: value,
                            rlim_max: value,
                        };
                        if libc::setrlimit($resource, &limit) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }};
                }
                cap!(libc::RLIMIT_CPU, 3);
                #[cfg(not(target_os = "macos"))]
                cap!(libc::RLIMIT_DATA, CHILD_MEMORY_LIMIT as libc::rlim_t);
                cap!(libc::RLIMIT_NOFILE, 128);
                cap!(libc::RLIMIT_CORE, 0);
                cap!(libc::RLIMIT_FSIZE, BYTE_LIMIT as libc::rlim_t);
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let pid = child.id() as i32;
        let mut stdout = child.stdout.take().ok_or_else(denied)?;
        let mut stderr = child.stderr.take().ok_or_else(denied)?;
        let result = (|| {
            for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                require(
                    flags >= 0
                        && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
                )?;
            }
            let mut out = Vec::new();
            let mut err = false;
            let mut err_size = 0;
            let mut buf = [0u8; 8192];
            #[cfg(target_os = "macos")]
            let mut next_memory_check = Instant::now();
            loop {
                require(Instant::now() < deadline)?;
                let mut advanced = false;
                let mut closed = 0;
                for (quiet, stream) in [
                    (false, &mut stdout as &mut dyn Read),
                    (true, &mut stderr as &mut dyn Read),
                ] {
                    match stream.read(&mut buf) {
                        Ok(0) => closed += 1,
                        Ok(n) => {
                            advanced = true;
                            if quiet {
                                err_size += n;
                                err = true;
                            } else {
                                out.extend_from_slice(&buf[..n]);
                            }
                            require(out.len() + err_size <= OUTPUT_LIMIT)?;
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e),
                    }
                }
                if closed == 2 {
                    if let Some(status) = child.try_wait()? {
                        return Ok((status.code().ok_or_else(denied)?, out, err));
                    }
                }
                #[cfg(target_os = "macos")]
                if Instant::now() >= next_memory_check {
                    require(child_resident_bytes(pid)? <= CHILD_MEMORY_LIMIT)?;
                    next_memory_check = Instant::now() + Duration::from_millis(50);
                }
                if !advanced {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        })();
        if result.is_err() {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn child_resident_bytes(pid: i32) -> io::Result<u64> {
        let mut data = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let count = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                data.as_mut_ptr().cast(),
                size,
            )
        };
        require(count == size)?;
        let info = unsafe { data.assume_init() };
        Ok(info.pti_resident_size)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Entry {
        dev: u64,
        ino: u64,
        dir: bool,
        bytes: u64,
        links: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    impl Entry {
        fn new(m: &Metadata) -> Self {
            Self {
                dev: m.dev() as u64,
                ino: m.ino(),
                dir: m.is_dir(),
                bytes: m.len(),
                links: m.nlink(),
                modified: (m.mtime(), m.mtime_nsec()),
                changed: (m.ctime(), m.ctime_nsec()),
            }
        }
        fn same_id(&self, meta: &Metadata) -> bool {
            self.dev == meta.dev() as u64 && self.ino == meta.ino() && self.dir == meta.is_dir()
        }
    }
    #[derive(PartialEq, Eq)]
    struct Inventory {
        entries: BTreeMap<PathBuf, Entry>,
        bytes: u64,
    }
    impl Inventory {
        fn scan(root: &Path, worktree: bool, deadline: Instant) -> io::Result<Self> {
            let meta = fs::symlink_metadata(root)?;
            require(meta.is_dir() && meta.uid() == unsafe { libc::geteuid() })?;
            let mut entries = BTreeMap::from([(PathBuf::new(), Entry::new(&meta))]);
            let mut dirs = vec![PathBuf::new()];
            let mut links: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
            let policy = Policy::default();
            let mut bytes = 0u64;
            while let Some(dir) = dirs.pop() {
                let parent = root.join(&dir);
                require(
                    Instant::now() < deadline
                        && entries[&dir].same_id(&fs::symlink_metadata(&parent)?),
                )?;
                for item in fs::read_dir(&parent)? {
                    require(entries.len() < ENTRY_LIMIT && Instant::now() < deadline)?;
                    let item = item?;
                    let name = item.file_name();
                    let path = dir.join(&name);
                    let m = fs::symlink_metadata(item.path())?;
                    if !worktree && dir.as_os_str().is_empty() {
                        let registered = match name.to_str() {
                            Some("debug" | "release" | "tmp") => m.is_dir(),
                            Some(
                                "CACHEDIR.TAG" | ".rustc_info.json" | ".rustdoc_fingerprint.json",
                            ) => m.is_file(),
                            _ => false,
                        };
                        require(registered)?;
                    }
                    let git_marker = worktree && path == Path::new(".git") && m.is_file();
                    require(git_marker || !policy.protects(name.to_str().ok_or_else(denied)?))?;
                    require(
                        m.dev() == meta.dev()
                            && m.uid() == meta.uid()
                            && (m.is_dir() || m.is_file())
                            && path.components().count() <= 32
                            && path.as_os_str().len() <= 4096,
                    )?;
                    if m.is_dir() {
                        dirs.push(path.clone());
                    } else {
                        let count = links
                            .entry((m.dev() as u64, m.ino()))
                            .or_insert((0, m.nlink()));
                        count.0 += 1;
                        if count.0 == 1 {
                            bytes = bytes.checked_add(m.len()).ok_or_else(denied)?;
                        }
                        require(bytes <= BYTE_LIMIT)?;
                    }
                    entries.insert(path, Entry::new(&m));
                }
            }
            require(links.values().all(|(found, total)| found == total))?;
            Ok(Self { entries, bytes })
        }

        // Resolve every parent through O_NOFOLLOW descriptors, validate the enrolled inode
        // at each hop, and unlink only entries previously inspected. Unexpected files therefore
        // make rmdir fail instead of being recursively swept up.
        fn remove_cargo(
            &self,
            queue: &Path,
            root: &Path,
            deadline: Instant,
            reclaimed: &mut u64,
        ) -> io::Result<()> {
            let parent = File::open(queue)?;
            let leaf = root.file_name().ok_or_else(denied)?;
            let root_fd = open_dir(&parent, leaf)?;
            require(self.entries[Path::new("")].same_id(&root_fd.metadata()?))?;
            for (path, entry) in self
                .entries
                .iter()
                .rev()
                .filter(|(p, _)| !p.as_os_str().is_empty())
            {
                require(Instant::now() < deadline)?;
                let mut fd = root_fd.try_clone()?;
                let mut relative = PathBuf::new();
                for name in path.parent().ok_or_else(denied)?.iter() {
                    relative.push(name);
                    fd = open_dir(&fd, name)?;
                    require(
                        self.entries
                            .get(&relative)
                            .is_some_and(|e| fd.metadata().is_ok_and(|m| e.same_id(&m))),
                    )?;
                }
                let name = path.file_name().ok_or_else(denied)?;
                let stat = stat_child(&fd, name)?;
                let mode = stat.st_mode & libc::S_IFMT;
                require(
                    stat.st_dev as u64 == entry.dev
                        && stat.st_ino as u64 == entry.ino
                        && if entry.dir {
                            mode == libc::S_IFDIR
                        } else {
                            mode == libc::S_IFREG && stat.st_size as u64 == entry.bytes
                        },
                )?;
                unlink(&fd, name, entry.dir)?;
                if !entry.dir && stat.st_nlink == 1 {
                    *reclaimed += entry.bytes;
                }
            }
            require(Instant::now() < deadline)?;
            let actual = stat_child(&parent, leaf)?;
            let expected = &self.entries[Path::new("")];
            require(actual.st_dev as u64 == expected.dev && actual.st_ino as u64 == expected.ino)?;
            unlink(&parent, leaf, true)?;
            parent.sync_all()
        }
    }
    fn cname(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| denied())
    }
    fn open_dir(parent: &File, name: &OsStr) -> io::Result<File> {
        let name = cname(name)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn stat_child(parent: &File, name: &OsStr) -> io::Result<libc::stat> {
        let name = cname(name)?;
        let mut stat = std::mem::MaybeUninit::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { stat.assume_init() })
        }
    }
    fn unlink(parent: &File, name: &OsStr, dir: bool) -> io::Result<()> {
        let name = cname(name)?;
        let flag = if dir { libc::AT_REMOVEDIR } else { 0 };
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flag) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn failure_and_interruption_keep_their_own_outputs() {
        let state = tempfile::tempdir().unwrap();
        let queue = CleanupQueue::open(state.path()).unwrap();
        for event in [CleanupEvent::Failure, CleanupEvent::Interrupted] {
            let job = queue.prepare_cargo().unwrap();
            let file = job.target.join("debug").join("fixture");
            fs::create_dir(file.parent().unwrap()).unwrap();
            fs::write(&file, b"built").unwrap();
            let report = queue.complete_cargo(job, event).unwrap();
            assert_eq!(
                (report.removed, report.preserved, report.preserved_bytes),
                (0, 1, Some(5))
            );
            assert!(file.exists());
        }
    }

    #[test]
    fn reopening_distinguishes_a_live_supervisor_from_an_abandoned_output() {
        let state = tempfile::tempdir().unwrap();
        let queue = CleanupQueue::open(state.path()).unwrap();
        let job = queue.prepare_cargo().unwrap();
        let target = job.target.clone();
        let journal =
            rusqlite::Connection::open(state.path().join("managed-cleanup/jobs.sqlite")).unwrap();
        let state_of_job = || {
            journal
                .query_row("SELECT state FROM jobs", [], |r| r.get::<_, String>(0))
                .unwrap()
        };
        drop(CleanupQueue::open(state.path()).unwrap());
        assert_eq!(state_of_job(), "active");
        drop(job);
        drop(CleanupQueue::open(state.path()).unwrap());
        assert_eq!(state_of_job(), "preserved");
        assert!(target.is_dir());
    }

    #[test]
    fn unknown_outputs_and_external_links_survive_success() {
        let state = tempfile::tempdir().unwrap();
        let queue = CleanupQueue::open(state.path()).unwrap();
        let unrelated = state.path().join("fixture");
        fs::write(&unrelated, b"retained").unwrap();
        for linked in [false, true] {
            let job = queue.prepare_cargo().unwrap();
            let unexpected = if linked {
                let dir = job.target.join("debug");
                fs::create_dir(&dir).unwrap();
                let path = dir.join("fixture");
                fs::hard_link(&unrelated, &path).unwrap();
                path
            } else {
                let path = job.target.join("unknown");
                fs::write(&path, b"retained").unwrap();
                path
            };
            let report = queue.complete_cargo(job, CleanupEvent::Success).unwrap();
            assert_eq!(
                (report.removed, report.preserved, report.reclaimed_bytes),
                (0, 1, 0)
            );
            assert!(unexpected.exists());
            assert!(unrelated.exists());
        }
    }
}
