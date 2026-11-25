use super::*;
use crate::docker_gc_apply::{Binding, DockerGcKind};
use crate::registry::protected_docker_gc_project_owner;
use std::collections::HashSet;

const MANIFEST_FILENAME: &str = "docker-backend-ownership.json";
const STORE_ID_FILENAME: &str = "docker-backend-store-id";
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_RESOURCES: usize = 128;

#[derive(Debug, Serialize)]
pub struct BackendOwnershipEvidence {
    pub controller: ControllerEvidence,
    pub engine: ControllerEvidence,
    pub store_namespace: String,
    pub project_id: i64,
    pub kind: DockerGcKind,
    pub resource_id: String,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendOwnershipFailure {
    AdmissionUnverified,
    ManifestUnverified,
    ProjectOwnershipUnverified,
    StoreBindingUnverified,
    EvidenceChanged,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipManifest {
    version: u8,
    admission_policy_sha256: String,
    controller_executable_sha256: String,
    engine_executable_sha256: String,
    store_directory: PathBuf,
    store_id: String,
    controller_store_fd: i32,
    resources: Vec<OwnedResource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedResource {
    project_id: i64,
    project_owner_sha256: String,
    kind: DockerGcKind,
    id: String,
}

impl OwnershipManifest {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes).map_err(|_| QueryFailure::Malformed)?;
        if value.version != 1
            || value.controller_store_fd < 3
            || !valid_hash(&value.admission_policy_sha256)
            || !valid_hash(&value.controller_executable_sha256)
            || !valid_hash(&value.engine_executable_sha256)
            || !valid_hash(&value.store_id)
            || value.resources.len() > MAX_RESOURCES
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let mut targets = HashSet::with_capacity(value.resources.len());
        for resource in &value.resources {
            if resource.project_id <= 0
                || !valid_hash(&resource.project_owner_sha256)
                || !resource.kind.valid_id(&resource.id)
                || !targets.insert((resource.kind, &resource.id))
            {
                return Err(QueryFailure::InvalidSelector);
            }
        }
        Ok(value)
    }

    fn matches_policy(&self, policy: &ProtectedPolicy<'_>) -> bool {
        self.admission_policy_sha256 == policy.digest
            && self.controller_executable_sha256 == policy.value.controller.executable_sha256
            && self.engine_executable_sha256 == policy.value.engine.executable_sha256
    }

    fn permits(&self, request: &WriteRequest, binding: &Binding, namespace: &str) -> bool {
        if binding.endpoint != namespace {
            return false;
        }
        match request {
            // The controller resolves the exact stored intent before looking up its receipt.
            // A retired assignment must not strand the last outstanding receipt in this store.
            WriteRequest::Recover { project_id, .. } => binding.project == *project_id,
            WriteRequest::Approve { project_id, .. } | WriteRequest::Apply { project_id, .. } => {
                binding.project == *project_id
                    && self.owns(*project_id, &binding.owner, binding.kind, &binding.id)
            }
        }
    }

    fn owns(&self, project: i64, owner: &str, kind: DockerGcKind, id: &str) -> bool {
        self.resources.iter().any(|resource| {
            resource.project_id == project
                && resource.project_owner_sha256 == owner
                && resource.kind == kind
                && resource.id == id
        })
    }
}

/// Observe a protected deployment and its exact project resource assignment. The controller's
/// held directory and the admitted engine socket are separate kernel observations. They provide
/// no capability to change resources or guarantee of exclusive engine-internal mutation.
pub fn verify_backend_ownership(
    policy_path: &Path,
    manifest_path: &Path,
    state_dir: &Path,
    project: i64,
    kind: DockerGcKind,
    id: &str,
) -> std::result::Result<BackendOwnershipEvidence, BackendOwnershipFailure> {
    use BackendOwnershipFailure::*;

    if project <= 0 || !kind.valid_id(id) {
        return Err(ProjectOwnershipUnverified);
    }
    let deadline = Instant::now() + ADMISSION_TIMEOUT;
    check_host_root().map_err(|_| AdmissionUnverified)?;
    let (manifest_file, manifest) = read_manifest(manifest_path).map_err(|_| ManifestUnverified)?;
    let owner = protected_docker_gc_project_owner(state_dir, project)
        .map_err(|_| ProjectOwnershipUnverified)?;
    if !manifest.owns(project, &owner, kind, id) {
        return Err(ProjectOwnershipUnverified);
    }

    let policy = ProtectedPolicy::open(policy_path).map_err(|_| AdmissionUnverified)?;
    if !manifest.matches_policy(&policy) {
        return Err(ManifestUnverified);
    }
    let peers = PinnedPeers::open(&policy.value, deadline).map_err(|_| AdmissionUnverified)?;
    let mut controller = peers
        .controller
        .connect()
        .map_err(|_| AdmissionUnverified)?;
    peers.engine.connect().map_err(|_| AdmissionUnverified)?;
    peers.approver.connect().map_err(|_| AdmissionUnverified)?;
    peers.executor.connect().map_err(|_| AdmissionUnverified)?;
    let store = PrivateStore::open(&manifest, policy.value.controller.pid)
        .map_err(|_| StoreBindingUnverified)?;
    exchange(&mut controller, deadline).map_err(|_| AdmissionUnverified)?;

    store.check_current().map_err(|_| EvidenceChanged)?;
    peers.check_current(deadline).map_err(|_| EvidenceChanged)?;
    policy.check_current().map_err(|_| EvidenceChanged)?;
    manifest_file.check_current().map_err(|_| EvidenceChanged)?;
    if protected_docker_gc_project_owner(state_dir, project).map_err(|_| EvidenceChanged)? != owner
    {
        return Err(EvidenceChanged);
    }
    remaining(deadline).map_err(|_| EvidenceChanged)?;
    Ok(BackendOwnershipEvidence {
        controller: peers.controller.evidence(b"controller", &policy.boot_id),
        engine: peers.engine.evidence(b"engine", &policy.boot_id),
        store_namespace: store.namespace,
        project_id: project,
        kind,
        resource_id: id.to_owned(),
    })
}

fn read_manifest(path: &Path) -> Result<(ProtectedRead, OwnershipManifest)> {
    if path.file_name() != Some(OsStr::new(MANIFEST_FILENAME)) {
        return Err(QueryFailure::InvalidSelector);
    }
    let file = ProtectedRead::open(path)?;
    let manifest = OwnershipManifest::parse(&file.read(MAX_MANIFEST_BYTES)?)?;
    Ok((file, manifest))
}

/// Pins the assignment and the store held by this controller while its live registry and
/// engine transaction are in use. Only the separate read-only probe opens an immutable DB.
pub(crate) struct PinnedBackendOwnership {
    file: ProtectedRead,
    manifest: OwnershipManifest,
    store: PrivateStore,
}

impl PinnedBackendOwnership {
    pub(super) fn open(policy: &ProtectedPolicy<'_>) -> Result<Self> {
        check_host_root()?;
        let path = policy.path.with_file_name(MANIFEST_FILENAME);
        let (file, manifest) = read_manifest(&path)?;
        if !manifest.matches_policy(policy) {
            return Err(QueryFailure::InvalidSelector);
        }
        let store = PrivateStore::open(&manifest, policy.value.controller.pid)?;
        let ownership = Self {
            file,
            manifest,
            store,
        };
        ownership.check_current()?;
        Ok(ownership)
    }

    pub(crate) fn permits(&self, request: &WriteRequest, binding: &Binding) -> bool {
        self.manifest
            .permits(request, binding, &self.store.namespace)
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        self.file.check_current()?;
        self.store.check_current()
    }
}

fn check_host_root() -> Result<()> {
    let map = bounded_read(File::open("/proc/self/uid_map").map_err(io_failure)?, 4096)?;
    let map = std::str::from_utf8(&map).map_err(|_| QueryFailure::Malformed)?;
    // An unprivileged caller can acquire UID 0 in a new user namespace. Require the host map.
    if unsafe { libc::geteuid() } != 0 || !map.split_ascii_whitespace().eq(["0", "0", "4294967295"])
    {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(())
}

struct ProtectedRead {
    path: PathBuf,
    descriptor: File,
    metadata: fs::Metadata,
}

impl ProtectedRead {
    fn open(path: &Path) -> Result<Self> {
        let descriptor = open_protected(path, false)?;
        let info = descriptor.metadata().map_err(io_failure)?;
        if !info.is_file() || info.uid() != 0 || info.mode() & 0o7777 != 0o444 || info.nlink() != 1
        {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(Self {
            path: path.to_path_buf(),
            descriptor,
            metadata: info,
        })
    }

    fn read(&self, limit: u64) -> Result<Vec<u8>> {
        let bytes = bounded_read(readable(&self.descriptor)?, limit)?;
        compare_metadata(
            &self.metadata,
            &self.descriptor.metadata().map_err(io_failure)?,
        )?;
        Ok(bytes)
    }

    fn check_current(&self) -> Result<()> {
        compare_metadata(
            &self.metadata,
            &open_protected(&self.path, false)?
                .metadata()
                .map_err(io_failure)?,
        )
    }
}

struct PrivateStore {
    directory: PathBuf,
    held_path: PathBuf,
    descriptor: File,
    marker: ProtectedRead,
    namespace: String,
}

impl PrivateStore {
    fn open(manifest: &OwnershipManifest, controller_pid: i32) -> Result<Self> {
        let directory = manifest.store_directory.as_path();
        let held_path = PathBuf::from(format!(
            "/proc/{controller_pid}/fd/{}",
            manifest.controller_store_fd
        ));
        let descriptor = open_protected(directory, false)?;
        if !private_directory(&descriptor.metadata().map_err(io_failure)?) {
            return Err(QueryFailure::InvalidSelector);
        }
        let marker_path = directory.join(STORE_ID_FILENAME);
        let marker = ProtectedRead::open(&marker_path)?;
        let namespace = store_namespace(&marker.read(65)?, &manifest.store_id)?;
        let store = Self {
            directory: directory.to_owned(),
            held_path,
            descriptor,
            marker,
            namespace,
        };
        store.check_current()?;
        Ok(store)
    }

    fn check_current(&self) -> Result<()> {
        let current = open_protected(&self.directory, false)?
            .metadata()
            .map_err(io_failure)?;
        let directory = self.descriptor.metadata().map_err(io_failure)?;
        let held = fs::metadata(&self.held_path).map_err(io_failure)?;
        if !private_directory(&current)
            || !same_inode(&current, &directory)
            || !same_inode(&current, &held)
        {
            return Err(QueryFailure::InvalidSelector);
        }
        self.marker.check_current()
    }
}

fn private_directory(info: &fs::Metadata) -> bool {
    info.is_dir() && info.uid() == 0 && info.mode() & 0o7777 == 0o700
}

fn store_namespace(bytes: &[u8], expected_id: &str) -> Result<String> {
    if !valid_hash(expected_id) || bytes.strip_suffix(b"\n") != Some(expected_id.as_bytes()) {
        return Err(QueryFailure::InvalidSelector);
    }
    let mut namespace = Sha256::new();
    namespace.update(b"dsg-docker-backend-store-v1\0");
    namespace.update(expected_id.as_bytes());
    Ok(format!("{:x}", namespace.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn manifest() -> Value {
        json!({
            "version": 1,
            "admission_policy_sha256": "f".repeat(64),
            "controller_executable_sha256": "b".repeat(64),
            "engine_executable_sha256": "c".repeat(64),
            "store_directory": "/run/docker-fixture/store",
            "store_id": "a".repeat(64),
            "controller_store_fd": 7,
            "resources": [{
                "project_id": 1,
                "project_owner_sha256": "d".repeat(64),
                "kind": "image",
                "id": format!("sha256:{}", "e".repeat(64))
            }]
        })
    }

    fn parse(value: &Value) -> Result<OwnershipManifest> {
        OwnershipManifest::parse(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn assignment_must_match_entire_project_and_target() {
        let ownership = parse(&manifest()).unwrap();
        let image = &ownership.resources[0].id;
        let owner = &"d".repeat(64);
        assert!(ownership.owns(1, owner, DockerGcKind::Image, image));
        assert!(!ownership.owns(2, owner, DockerGcKind::Image, image));
        assert!(!ownership.owns(1, &"a".repeat(64), DockerGcKind::Image, image));
        assert!(!ownership.owns(1, owner, DockerGcKind::BuildCache, image));
        assert!(!ownership.owns(1, owner, DockerGcKind::Image, &image[..16]));
    }

    #[test]
    fn empty_assignment_allows_only_receipt_lookup_for_the_same_project_and_store() {
        let mut value = manifest();
        let original = parse(&value).unwrap();
        let resource = &original.resources[0];
        let store_id = value["store_id"].as_str().unwrap();
        let namespace = store_namespace(format!("{store_id}\n").as_bytes(), store_id).unwrap();
        let binding = Binding {
            project: resource.project_id,
            owner: resource.project_owner_sha256.clone(),
            kind: resource.kind,
            id: resource.id.clone(),
            endpoint: namespace.clone(),
            generation: "1".repeat(64),
            snapshot: "2".repeat(64),
        };
        value["resources"] = json!([]);
        let empty = parse(&value).unwrap();
        let project_id = binding.project;
        let recover = WriteRequest::Recover {
            project_id,
            intent_id: "3".repeat(64),
        };
        assert!(empty.permits(&recover, &binding, &namespace));
        assert!(!empty.permits(&recover, &binding, &"4".repeat(64)));
        let other_project = WriteRequest::Recover {
            project_id: project_id + 1,
            intent_id: "3".repeat(64),
        };
        assert!(!empty.permits(&other_project, &binding, &namespace));
        for request in [
            WriteRequest::Approve {
                project_id,
                plan_id: 1,
            },
            WriteRequest::Apply {
                project_id,
                plan_id: 1,
            },
        ] {
            assert!(!empty.permits(&request, &binding, &namespace));
        }
    }

    #[test]
    fn ownership_rejects_conflicting_assignments_short_ids_and_capability_claims() {
        let mut conflict = manifest();
        let mut resource = conflict["resources"][0].clone();
        resource["project_id"] = json!(2);
        conflict["resources"].as_array_mut().unwrap().push(resource);
        assert_eq!(parse(&conflict).err(), Some(QueryFailure::InvalidSelector));

        let mut short = manifest();
        short["resources"][0]["id"] = json!("sha256:ea");
        assert_eq!(parse(&short).err(), Some(QueryFailure::InvalidSelector));

        let mut mutation_claim = manifest();
        mutation_claim["exclusive_mutation"] = json!(true);
        assert_eq!(parse(&mutation_claim).err(), Some(QueryFailure::Malformed));
    }

    #[test]
    fn stable_namespace_is_bound_to_the_expected_persistent_marker() {
        let store_id = "a".repeat(64);
        let marker = format!("{store_id}\n");
        assert_eq!(
            store_namespace(marker.as_bytes(), &store_id).unwrap(),
            "6f43915d23eac6e701938e3a5d777321c19dbc56426190b07af0fa006f13a08f"
        );
        assert_eq!(
            store_namespace(marker.as_bytes(), &"b".repeat(64)),
            Err(QueryFailure::InvalidSelector)
        );
        assert_eq!(
            store_namespace(store_id.as_bytes(), &store_id),
            Err(QueryFailure::InvalidSelector)
        );
    }
}
