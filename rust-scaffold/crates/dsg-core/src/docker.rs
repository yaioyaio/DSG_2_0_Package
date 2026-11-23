use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_RESOURCES: usize = 512;
const MAX_MOUNTS: usize = 128;
const MAX_DIGESTS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DockerOperation<'a> {
    Engine,
    Containers,
    Container(&'a str),
    Images,
    Image(&'a str),
    Volumes,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryFailure {
    #[error("Docker 조회 식별자가 올바르지 않습니다")]
    InvalidSelector,
    #[error("Docker 조회 결과를 확인할 수 없습니다")]
    Unavailable,
    #[error("Docker 조회 시간이 제한을 초과했습니다")]
    Timeout,
    #[error("Docker 조회 결과가 수집 제한을 초과했습니다")]
    TooLarge,
    #[error("Docker 조회 결과의 형식 또는 식별자가 올바르지 않습니다")]
    Malformed,
}

pub trait DockerRead {
    fn context(&self) -> &str;
    fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryStatus {
    Complete,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryStage {
    Context,
    Engine,
    EngineRecheck,
    Containers,
    Container,
    Images,
    Image,
    Volumes,
    ImageReferences,
    VolumeReferences,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct InventoryIssue {
    pub stage: InventoryStage,
    pub failure: QueryFailure,
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub context: String,
    pub engine_id: Option<String>,
    pub complete: bool,
    pub status: InventoryStatus,
    pub issues: Vec<InventoryIssue>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<String>,
    pub duplicates: Vec<DuplicateGroup>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DuplicateGroup {
    pub project: String,
    pub service: String,
    pub container_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Unhealthy,
    Starting,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ComposeIdentity {
    pub project: Option<String>,
    pub service: Option<String>,
    pub number: Option<u32>,
    pub config_hash: Option<String>,
    pub oneoff: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MountKind {
    Volume,
    Bind,
    Tmpfs,
    Npipe,
    Cluster,
    Image,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub kind: MountKind,
    pub volume: Option<String>,
    pub destination: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Container {
    pub id: String,
    pub image_id: Option<String>,
    pub state: ContainerState,
    pub health: Option<Health>,
    pub compose: Option<ComposeIdentity>,
    pub mounts: Vec<Mount>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub id: String,
    pub digests: Vec<String>,
    pub os: String,
    pub architecture: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContainerProjection {
    id: String,
    image_id: Option<String>,
    state: ContainerState,
    health: Option<String>,
    compose: Option<ComposeProjection>,
    mounts: Vec<Mount>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeProjection {
    project: Option<String>,
    service: Option<String>,
    number: Option<LabelValue>,
    config_hash: Option<String>,
    oneoff: Option<LabelValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LabelValue {
    Text(String),
    Number(u32),
    Flag(bool),
}

fn safe_name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn engine_id(value: &str) -> Option<String> {
    let legacy = value.len() == 59
        && value.split(':').count() == 12
        && value.split(':').all(|group| {
            group.len() == 4
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte))
        });
    (legacy || safe_name(value, 128)).then(|| value.to_owned())
}

fn hex_id(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn image_id(value: &str) -> Option<String> {
    hex_id(value.strip_prefix("sha256:").unwrap_or(value)).map(|id| format!("sha256:{id}"))
}

fn digest(value: &str) -> Option<String> {
    hex_id(value.strip_prefix("sha256:")?).map(|id| format!("sha256:{id}"))
}

fn volume_name(value: &str) -> Option<String> {
    safe_name(value, 255).then(|| value.to_owned())
}

fn compose_project(value: &str) -> Option<String> {
    (safe_name(value, 255)
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_uppercase() || byte == b'.'))
    .then(|| value.to_owned())
}

fn compose_service(value: &str) -> Option<String> {
    (value.len() <= 255
        && value.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then(|| value.to_owned())
}

fn optional_label<T>(
    value: Option<String>,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, QueryFailure> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| parse(value).ok_or(QueryFailure::Malformed))
        .transpose()
}

fn normalize_compose(raw: ComposeProjection) -> Result<Option<ComposeIdentity>, QueryFailure> {
    let project = optional_label(raw.project, compose_project)?;
    let service = optional_label(raw.service, compose_service)?;
    let number = match raw.number {
        None => None,
        Some(LabelValue::Text(value)) if value.is_empty() => None,
        Some(LabelValue::Text(value)) if value.bytes().all(|byte| byte.is_ascii_digit()) => {
            Some(value.parse().map_err(|_| QueryFailure::Malformed)?)
        }
        Some(LabelValue::Number(value)) => Some(value),
        _ => return Err(QueryFailure::Malformed),
    };
    if number == Some(0) {
        return Err(QueryFailure::Malformed);
    }
    let config_hash = optional_label(raw.config_hash, hex_id)?;
    let oneoff = match raw.oneoff {
        None => Some(false),
        Some(LabelValue::Text(value))
            if value.is_empty() || value.eq_ignore_ascii_case("false") =>
        {
            Some(false)
        }
        Some(LabelValue::Text(value)) if value.eq_ignore_ascii_case("true") => Some(true),
        Some(LabelValue::Flag(value)) => Some(value),
        _ => None,
    };
    if project.is_none()
        && service.is_none()
        && number.is_none()
        && config_hash.is_none()
        && oneoff == Some(false)
    {
        return Ok(None);
    }
    Ok(Some(ComposeIdentity {
        project,
        service,
        number,
        config_hash,
        oneoff,
    }))
}

fn valid_destination(value: &str) -> bool {
    if value.len() > 512 {
        return false;
    }
    let path = match value.as_bytes() {
        [b'/', rest @ ..] => rest,
        [drive, b':', b'\\' | b'/', rest @ ..] if drive.is_ascii_alphabetic() => rest,
        [b'\\', b'\\', b'.', b'\\', rest @ ..] => rest,
        _ => return false,
    };
    path.iter().all(|&byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b'\\' | b' ')
    }) && !path
        .split(|&byte| matches!(byte, b'/' | b'\\'))
        .any(|part| part == b"." || part == b"..")
}

fn sort_mounts(mounts: &mut [Mount]) -> Result<(), QueryFailure> {
    if mounts.len() > MAX_MOUNTS {
        return Err(QueryFailure::TooLarge);
    }
    if mounts.iter().any(|mount| {
        !valid_destination(&mount.destination)
            || match mount.kind {
                MountKind::Volume => !mount
                    .volume
                    .as_deref()
                    .is_some_and(|name| safe_name(name, 255)),
                _ => mount.volume.is_some(),
            }
    }) {
        return Err(QueryFailure::Malformed);
    }
    mounts.sort_unstable_by(|left, right| left.destination.cmp(&right.destination));
    if mounts
        .windows(2)
        .any(|pair| pair[0].destination == pair[1].destination)
    {
        return Err(QueryFailure::Malformed);
    }
    Ok(())
}

fn parse_container(raw: &str, id: &str) -> Result<Container, QueryFailure> {
    let mut raw: ContainerProjection =
        serde_json::from_str(raw).map_err(|_| QueryFailure::Malformed)?;
    if hex_id(&raw.id).as_deref() != Some(id) {
        return Err(QueryFailure::Malformed);
    }
    sort_mounts(&mut raw.mounts)?;
    let health = match raw.health.as_deref() {
        None | Some("" | "none") => None,
        Some("healthy") => Some(Health::Healthy),
        Some("unhealthy") => Some(Health::Unhealthy),
        Some("starting") => Some(Health::Starting),
        Some(_) => Some(Health::Unknown),
    };
    Ok(Container {
        id: id.to_owned(),
        image_id: optional_label(raw.image_id, image_id)?,
        state: raw.state,
        health,
        compose: raw.compose.map(normalize_compose).transpose()?.flatten(),
        mounts: raw.mounts,
    })
}

fn parse_image(raw: &str, id: &str) -> Result<Image, QueryFailure> {
    let mut image: Image = serde_json::from_str(raw).map_err(|_| QueryFailure::Malformed)?;
    if image_id(&image.id).as_deref() != Some(id)
        || !safe_name(&image.os, 32)
        || !safe_name(&image.architecture, 32)
    {
        return Err(QueryFailure::Malformed);
    }
    if image.digests.len() > MAX_DIGESTS {
        return Err(QueryFailure::TooLarge);
    }
    image.id = id.to_owned();
    for value in &mut image.digests {
        *value = digest(value).ok_or(QueryFailure::Malformed)?;
    }
    image.digests.sort_unstable();
    image.digests.dedup();
    Ok(image)
}

fn checked_query(
    query: &impl DockerRead,
    operation: DockerOperation<'_>,
) -> Result<String, QueryFailure> {
    let response = query.query(operation)?;
    if response.len() > MAX_RESPONSE_BYTES {
        return Err(QueryFailure::TooLarge);
    }
    Ok(response)
}

pub fn query_engine(query: &impl DockerRead) -> Result<String, QueryFailure> {
    let response = checked_query(query, DockerOperation::Engine)?;
    let mut lines = response.lines();
    let id = lines.next().and_then(engine_id);
    if lines.next().is_some() {
        return Err(QueryFailure::Malformed);
    }
    id.ok_or(QueryFailure::Malformed)
}

fn identifiers(
    query: &impl DockerRead,
    operation: DockerOperation<'_>,
    stage: InventoryStage,
    parse: fn(&str) -> Option<String>,
    remaining: &mut usize,
    inventory: &mut Inventory,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let Some(raw) = inventory.record(stage, checked_query(query, operation)) else {
        return ids;
    };
    for line in raw.lines() {
        let Some(id) = parse(line) else {
            inventory.issue(stage, QueryFailure::Malformed);
            continue;
        };
        if ids.contains(&id) {
            continue;
        }
        if ids.len() == *remaining {
            inventory.issue(stage, QueryFailure::TooLarge);
            break;
        }
        ids.insert(id);
    }
    *remaining -= ids.len();
    ids
}

pub fn inventory(query: &impl DockerRead, max_resources: usize) -> Inventory {
    let mut result = Inventory {
        context: String::new(),
        engine_id: None,
        complete: true,
        status: InventoryStatus::Complete,
        issues: Vec::new(),
        containers: Vec::new(),
        images: Vec::new(),
        volumes: Vec::new(),
        duplicates: Vec::new(),
    };
    if !safe_name(query.context(), 128) {
        result.issue(InventoryStage::Context, QueryFailure::InvalidSelector);
        return result;
    }
    result.context = query.context().to_owned();
    let Some(first_engine) = result.record(InventoryStage::Engine, query_engine(query)) else {
        return result;
    };
    let remaining = &mut max_resources.min(MAX_RESOURCES);
    for id in identifiers(
        query,
        DockerOperation::Containers,
        InventoryStage::Containers,
        hex_id,
        remaining,
        &mut result,
    ) {
        let detail = checked_query(query, DockerOperation::Container(&id))
            .and_then(|raw| parse_container(&raw, &id));
        if let Some(container) = result.record(InventoryStage::Container, detail) {
            result.containers.push(container);
        }
    }
    for id in identifiers(
        query,
        DockerOperation::Images,
        InventoryStage::Images,
        image_id,
        remaining,
        &mut result,
    ) {
        let detail = checked_query(query, DockerOperation::Image(&id))
            .and_then(|raw| parse_image(&raw, &id));
        if let Some(image) = result.record(InventoryStage::Image, detail) {
            result.images.push(image);
        }
    }
    result.volumes = identifiers(
        query,
        DockerOperation::Volumes,
        InventoryStage::Volumes,
        volume_name,
        remaining,
        &mut result,
    )
    .into_iter()
    .collect();
    match result.record(InventoryStage::EngineRecheck, query_engine(query)) {
        Some(last_engine) if last_engine == first_engine => result.engine_id = Some(first_engine),
        Some(_) => {
            result.issue(InventoryStage::EngineRecheck, QueryFailure::Malformed);
            result.containers.clear();
            result.images.clear();
            result.volumes.clear();
            return result;
        }
        None => return result,
    }
    result.check_references();
    result.duplicates = duplicate_groups(&result.containers);
    result
}

fn duplicate_groups(containers: &[Container]) -> Vec<DuplicateGroup> {
    let mut groups = BTreeMap::<(&str, &str), Vec<String>>::new();
    for container in containers {
        let Some(ComposeIdentity {
            project: Some(project),
            service: Some(service),
            oneoff: Some(false),
            ..
        }) = &container.compose
        else {
            continue;
        };
        groups
            .entry((project, service))
            .or_default()
            .push(container.id.clone());
    }
    groups
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|((project, service), container_ids)| DuplicateGroup {
            project: project.to_owned(),
            service: service.to_owned(),
            container_ids,
        })
        .collect()
}

pub struct ReuseRequest<'a> {
    pub engine_id: &'a str,
    pub project: &'a str,
    pub service: &'a str,
    pub number: u32,
    pub config_hash: &'a str,
    pub image_id: &'a str,
    pub image_digest: &'a str,
    pub mounts: &'a [Mount],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseStatus {
    Candidate,
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseReason {
    Verified,
    IncompleteInventory,
    EngineMismatch,
    MissingInstance,
    UnverifiedInstance,
    AmbiguousInstance,
    UnverifiedConfiguration,
    ConfigurationChanged,
    UnverifiedImage,
    ImageChanged,
    UnverifiedMount,
    MountChanged,
    UnverifiedHealth,
    NotRunning,
    NotHealthy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReuseAssessment {
    pub status: ReuseStatus,
    pub reason: ReuseReason,
    pub container_id: Option<String>,
}

impl ReuseAssessment {
    fn unknown(reason: ReuseReason) -> Self {
        Self {
            status: ReuseStatus::Unknown,
            reason,
            container_id: None,
        }
    }

    fn reject(reason: ReuseReason) -> Self {
        Self {
            status: ReuseStatus::Rejected,
            reason,
            container_id: None,
        }
    }
}

impl Inventory {
    fn issue(&mut self, stage: InventoryStage, failure: QueryFailure) {
        self.complete = false;
        self.status = InventoryStatus::Unknown;
        let issue = InventoryIssue { stage, failure };
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }

    fn record<T>(&mut self, stage: InventoryStage, result: Result<T, QueryFailure>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(failure) => {
                self.issue(stage, failure);
                None
            }
        }
    }

    fn check_references(&mut self) {
        if self.containers.iter().any(|container| {
            !container.image_id.as_deref().is_some_and(|id| {
                self.images
                    .binary_search_by(|image| image.id.as_str().cmp(id))
                    .is_ok()
            })
        }) {
            self.issue(InventoryStage::ImageReferences, QueryFailure::Unavailable);
        }
        if self.containers.iter().any(|container| {
            container.mounts.iter().any(|mount| {
                mount
                    .volume
                    .as_ref()
                    .is_some_and(|volume| self.volumes.binary_search(volume).is_err())
            })
        }) {
            self.issue(InventoryStage::VolumeReferences, QueryFailure::Unavailable);
        }
    }

    /// Assess the observed snapshot against a Compose instance's expected identity.
    pub fn assess_reuse(&self, request: &ReuseRequest<'_>) -> ReuseAssessment {
        use ReuseReason::*;

        let Some(engine) = engine_id(request.engine_id) else {
            return ReuseAssessment::unknown(IncompleteInventory);
        };
        if self.engine_id.as_ref().is_some_and(|id| id != &engine) {
            return ReuseAssessment::reject(EngineMismatch);
        }
        if !self.complete || self.engine_id.is_none() {
            return ReuseAssessment::unknown(IncompleteInventory);
        }
        if request.number == 0
            || compose_project(request.project).is_none()
            || compose_service(request.service).is_none()
        {
            return ReuseAssessment::unknown(UnverifiedInstance);
        }

        let mut matched = None;
        let mut uncertain = false;
        for container in &self.containers {
            let Some(compose) = &container.compose else {
                continue;
            };
            if compose.oneoff == Some(true)
                || compose
                    .project
                    .as_deref()
                    .is_some_and(|id| id != request.project)
                || compose
                    .service
                    .as_deref()
                    .is_some_and(|id| id != request.service)
                || compose
                    .number
                    .is_some_and(|number| number != request.number)
            {
                continue;
            }
            if compose.project.is_none()
                || compose.service.is_none()
                || compose.number.is_none()
                || compose.oneoff.is_none()
            {
                uncertain = true;
                continue;
            }
            if matched.replace((container, compose)).is_some() {
                return ReuseAssessment::unknown(AmbiguousInstance);
            }
        }
        if uncertain {
            return ReuseAssessment::unknown(UnverifiedInstance);
        }
        let Some((container, compose)) = matched else {
            return ReuseAssessment::reject(MissingInstance);
        };
        let (Some(current_hash), Some(wanted_hash)) =
            (&compose.config_hash, hex_id(request.config_hash))
        else {
            return ReuseAssessment::unknown(UnverifiedConfiguration);
        };
        if current_hash != &wanted_hash {
            return ReuseAssessment::reject(ConfigurationChanged);
        }

        let (Some(current_image), Some(wanted_image), Some(wanted_digest)) = (
            &container.image_id,
            image_id(request.image_id),
            digest(request.image_digest),
        ) else {
            return ReuseAssessment::unknown(UnverifiedImage);
        };
        if current_image != &wanted_image {
            return ReuseAssessment::reject(ImageChanged);
        }
        let Some(image) = self.images.iter().find(|image| &image.id == current_image) else {
            return ReuseAssessment::unknown(UnverifiedImage);
        };
        if image.digests.is_empty() {
            return ReuseAssessment::unknown(UnverifiedImage);
        }
        if !image.digests.contains(&wanted_digest) {
            return ReuseAssessment::reject(ImageChanged);
        }

        if request.mounts.len() > MAX_MOUNTS {
            return ReuseAssessment::unknown(UnverifiedMount);
        }
        let mut wanted_mounts = request.mounts.to_vec();
        if sort_mounts(&mut wanted_mounts).is_err() {
            return ReuseAssessment::unknown(UnverifiedMount);
        }
        if container.mounts != wanted_mounts {
            return ReuseAssessment::reject(MountChanged);
        }
        if container
            .mounts
            .iter()
            .any(|mount| mount.kind != MountKind::Volume)
        {
            return ReuseAssessment::unknown(UnverifiedMount);
        }

        match container.state {
            ContainerState::Running => {}
            ContainerState::Unknown => return ReuseAssessment::unknown(UnverifiedHealth),
            _ => return ReuseAssessment::reject(NotRunning),
        }
        match container.health {
            Some(Health::Healthy) => ReuseAssessment {
                status: ReuseStatus::Candidate,
                reason: Verified,
                container_id: Some(container.id.clone()),
            },
            Some(Health::Unhealthy | Health::Starting) => ReuseAssessment::reject(NotHealthy),
            Some(Health::Unknown) | None => ReuseAssessment::unknown(UnverifiedHealth),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::cell::Cell;

    const ENGINE: &str = "engine-fixture";
    const CONTAINER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SECOND_CONTAINER: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const IMAGE: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const DIGEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const HASH: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const CHANGED_HASH: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    struct Fixture {
        context: &'static str,
        last_engine: Result<&'static str, QueryFailure>,
        engine_calls: Cell<u32>,
        containers: BTreeMap<&'static str, Value>,
        containers_extra: &'static str,
        image: Result<Value, QueryFailure>,
        volumes: &'static str,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                context: "workstation",
                last_engine: Ok(ENGINE),
                engine_calls: Cell::new(0),
                containers: BTreeMap::from([(
                    CONTAINER,
                    json!({
                        "id": CONTAINER,
                        "image_id": IMAGE,
                        "state": "running",
                        "health": "healthy",
                        "compose": {"project": "shop", "service": "db", "number": "01", "config_hash": HASH, "oneoff": "False"},
                        "mounts": [{"kind": "volume", "volume": "shop_data", "destination": "/data", "read_only": false}]
                    }),
                )]),
                containers_extra: "",
                image: Ok(
                    json!({ "id": IMAGE, "digests": [DIGEST], "os": "linux", "architecture": "arm64" }),
                ),
                volumes: "shop_data\r\n",
            }
        }

        fn container(&mut self) -> &mut Value {
            self.containers.get_mut(CONTAINER).unwrap()
        }

        fn scan(&self) -> Inventory {
            self.engine_calls.set(0);
            inventory(self, 10)
        }
    }

    impl DockerRead for Fixture {
        fn context(&self) -> &str {
            self.context
        }

        fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
            match operation {
                DockerOperation::Engine => {
                    let calls = self.engine_calls.get();
                    self.engine_calls.set(calls + 1);
                    let id = if calls == 0 {
                        ENGINE
                    } else {
                        self.last_engine?
                    };
                    Ok(format!("{id}\r\n"))
                }
                DockerOperation::Containers => {
                    let mut ids = self
                        .containers
                        .keys()
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\r\n");
                    ids.push_str(self.containers_extra);
                    Ok(ids)
                }
                DockerOperation::Container(id) => self
                    .containers
                    .get(id)
                    .map(Value::to_string)
                    .ok_or(QueryFailure::Unavailable),
                DockerOperation::Images => Ok(format!(
                    "{IMAGE}\n{}\n",
                    IMAGE.strip_prefix("sha256:").unwrap().to_ascii_uppercase()
                )),
                DockerOperation::Image(IMAGE) => self
                    .image
                    .as_ref()
                    .map(Value::to_string)
                    .map_err(|error| *error),
                DockerOperation::Image(_) => Err(QueryFailure::InvalidSelector),
                DockerOperation::Volumes => Ok(self.volumes.to_owned()),
            }
        }
    }

    fn volume() -> Mount {
        Mount {
            kind: MountKind::Volume,
            volume: Some("shop_data".to_owned()),
            destination: "/data".to_owned(),
            read_only: false,
        }
    }

    fn request(mounts: &[Mount]) -> ReuseRequest<'_> {
        ReuseRequest {
            engine_id: ENGINE,
            project: "shop",
            service: "db",
            number: 1,
            config_hash: HASH,
            image_id: IMAGE,
            image_digest: DIGEST,
            mounts,
        }
    }

    #[test]
    fn proven_identity_yields_a_candidate() {
        let inventory = Fixture::new().scan();
        assert!(inventory.complete);
        assert_eq!(inventory.engine_id.as_deref(), Some(ENGINE));
        assert_eq!(inventory.images.len(), 1);
        let result = inventory.assess_reuse(&request(&[volume()]));
        assert_eq!(result.status, ReuseStatus::Candidate);
        assert_eq!(result.container_id.as_deref(), Some(CONTAINER));
    }

    #[test]
    fn rejects_changes_in_engine_project_service_instance_configuration_image_and_mount() {
        let inventory = Fixture::new().scan();
        let same = [volume()];
        let other_mount = [Mount {
            volume: Some("other_data".to_owned()),
            ..volume()
        }];
        let readonly_mount = [Mount {
            read_only: true,
            ..volume()
        }];
        for (wanted, expected) in [
            (
                ReuseRequest {
                    engine_id: "other-engine",
                    ..request(&same)
                },
                ReuseReason::EngineMismatch,
            ),
            (
                ReuseRequest {
                    project: "other",
                    ..request(&same)
                },
                ReuseReason::MissingInstance,
            ),
            (
                ReuseRequest {
                    service: "cache",
                    ..request(&same)
                },
                ReuseReason::MissingInstance,
            ),
            (
                ReuseRequest {
                    number: 2,
                    ..request(&same)
                },
                ReuseReason::MissingInstance,
            ),
            (
                ReuseRequest {
                    config_hash: CHANGED_HASH,
                    ..request(&same)
                },
                ReuseReason::ConfigurationChanged,
            ),
            (
                ReuseRequest {
                    image_id: DIGEST,
                    ..request(&same)
                },
                ReuseReason::ImageChanged,
            ),
            (
                ReuseRequest {
                    image_digest: IMAGE,
                    ..request(&same)
                },
                ReuseReason::ImageChanged,
            ),
            (request(&other_mount), ReuseReason::MountChanged),
            (request(&readonly_mount), ReuseReason::MountChanged),
        ] {
            let result = inventory.assess_reuse(&wanted);
            assert_eq!(
                (result.status, result.reason),
                (ReuseStatus::Rejected, expected)
            );
        }
    }

    #[test]
    fn missing_facts_and_unchecked_bind_sources_remain_unknown() {
        let named = [volume()];
        for (field, value, reason) in [
            ("health", Value::Null, ReuseReason::UnverifiedHealth),
            (
                "compose",
                json!({"project": "shop", "service": "db", "number": 1}),
                ReuseReason::UnverifiedConfiguration,
            ),
            (
                "compose",
                json!({"project": "shop", "service": "db", "number": "", "config_hash": HASH}),
                ReuseReason::UnverifiedInstance,
            ),
            (
                "compose",
                json!({"project": "shop", "service": "db", "number": 1, "config_hash": HASH, "oneoff": "invalid"}),
                ReuseReason::UnverifiedInstance,
            ),
        ] {
            let mut fixture = Fixture::new();
            fixture.container()[field] = value;
            assert_eq!(
                fixture.scan().assess_reuse(&request(&named)),
                ReuseAssessment::unknown(reason)
            );
        }

        let mut fixture = Fixture::new();
        fixture.image.as_mut().unwrap()["digests"] = json!([]);
        assert_eq!(
            fixture.scan().assess_reuse(&request(&named)),
            ReuseAssessment::unknown(ReuseReason::UnverifiedImage)
        );
        fixture.container()["mounts"] =
            json!([{"kind": "bind", "volume": null, "destination": "/data", "read_only": false}]);
        fixture.image.as_mut().unwrap()["digests"] = json!([DIGEST]);
        let bind = [Mount {
            kind: MountKind::Bind,
            volume: None,
            ..volume()
        }];
        assert_eq!(
            fixture.scan().assess_reuse(&request(&bind)),
            ReuseAssessment::unknown(ReuseReason::UnverifiedMount)
        );
    }

    #[test]
    fn requires_running_and_healthy() {
        for (field, value, reason) in [
            ("state", "exited", ReuseReason::NotRunning),
            ("health", "unhealthy", ReuseReason::NotHealthy),
            ("health", "starting", ReuseReason::NotHealthy),
        ] {
            let mut fixture = Fixture::new();
            fixture.container()[field] = json!(value);
            assert_eq!(
                fixture.scan().assess_reuse(&request(&[volume()])),
                ReuseAssessment::reject(reason)
            );
        }
    }

    #[test]
    fn same_service_group_explains_replicas_without_proving_reuse() {
        let mut fixture = Fixture::new();
        let mut replica = fixture.container().clone();
        replica["id"] = json!(SECOND_CONTAINER);
        replica["compose"]["number"] = json!(2);
        fixture.containers.insert(SECOND_CONTAINER, replica);
        let inventory = fixture.scan();
        assert_eq!(
            inventory.duplicates[0].container_ids,
            vec![CONTAINER, SECOND_CONTAINER]
        );
        assert_eq!(
            inventory.assess_reuse(&request(&[volume()])).status,
            ReuseStatus::Candidate
        );

        fixture.containers.get_mut(SECOND_CONTAINER).unwrap()["compose"]["number"] = json!(1);
        assert_eq!(
            fixture.scan().assess_reuse(&request(&[volume()])),
            ReuseAssessment::unknown(ReuseReason::AmbiguousInstance)
        );

        fixture.containers.get_mut(SECOND_CONTAINER).unwrap()["compose"]["oneoff"] = json!("True");
        fixture.container()["compose"]["oneoff"] = json!("True");
        let inventory = fixture.scan();
        assert!(inventory.duplicates.is_empty());
        assert_eq!(
            inventory.assess_reuse(&request(&[volume()])),
            ReuseAssessment::reject(ReuseReason::MissingInstance)
        );
    }

    #[test]
    fn partial_inventory_preserves_observations_and_blocks_reuse() {
        let mut fixture = Fixture::new();
        fixture.image = Err(QueryFailure::Timeout);
        let inventory = fixture.scan();
        assert_eq!(inventory.status, InventoryStatus::Unknown);
        assert_eq!(inventory.containers.len(), 1);
        assert_eq!(inventory.volumes, vec!["shop_data"]);
        assert_eq!(
            inventory.assess_reuse(&request(&[volume()])),
            ReuseAssessment::unknown(ReuseReason::IncompleteInventory)
        );
        assert!(inventory.issues.contains(&InventoryIssue {
            stage: InventoryStage::Image,
            failure: QueryFailure::Timeout
        }));

        let fixture = Fixture::new();
        let limited = super::inventory(&fixture, 1);
        assert!(!limited.complete);
        assert!(limited.images.is_empty());
        assert!(limited.volumes.is_empty());
        assert!(limited.issues.contains(&InventoryIssue {
            stage: InventoryStage::Images,
            failure: QueryFailure::TooLarge
        }));
    }

    #[test]
    fn malformed_selectors_mismatched_details_and_extra_projection_fields_do_not_escape() {
        let mut fixture = Fixture::new();
        fixture.containers_extra = "\r\n--invalid-selector\r\n";
        fixture.container()["id"] = json!(SECOND_CONTAINER);
        fixture.image.as_mut().unwrap()["digests"] =
            json!(["dummy-registry.invalid/repo@sha256:dddd"]);
        let inventory = fixture.scan();
        assert!(!inventory.complete);
        assert!(inventory.containers.is_empty());
        assert!(inventory.images.is_empty());
        let public = serde_json::to_string(&inventory).unwrap();
        assert!(!public.contains("dummy-registry"));
        assert!(!public.contains("invalid-selector"));

        let mut fixture = Fixture::new();
        fixture.container()["mounts"][0]["source"] = json!("fixture-extra-field");
        let inventory = fixture.scan();
        assert!(!inventory.complete);
        assert!(inventory.containers.is_empty());
        assert!(!serde_json::to_string(&inventory)
            .unwrap()
            .contains("fixture-extra-field"));
    }

    #[test]
    fn changing_engine_discards_mixed_resource_results() {
        let mut fixture = Fixture::new();
        fixture.last_engine = Ok("other-engine");
        let inventory = fixture.scan();
        assert_eq!(inventory.engine_id, None);
        assert_eq!(inventory.status, InventoryStatus::Unknown);
        assert!(inventory.containers.is_empty());
        assert!(inventory.images.is_empty());
        assert!(inventory.volumes.is_empty());
    }

    #[test]
    fn accepts_engine_id_shapes_and_linux_windows_destination_fixtures() {
        for id in [
            "c68196b3-0531-4e61-bf96-635618086137",
            "AAAA:BBBB:CCCC:DDDD:EEEE:FFFF:GGGG:HHHH:IIII:JJJJ:KKKK:LLLL",
        ] {
            assert_eq!(engine_id(id).as_deref(), Some(id));
        }
        for destination in [
            "/var/lib/app",
            r"C:\Program Files\app",
            "C:/data",
            r"\\.\pipe\app",
        ] {
            assert!(valid_destination(destination));
        }
        for unsafe_path in ["relative", "/data/../other", "//user@host/data"] {
            assert!(!valid_destination(unsafe_path));
        }
    }
}
