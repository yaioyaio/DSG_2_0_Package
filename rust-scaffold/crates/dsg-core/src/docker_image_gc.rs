use crate::docker::{
    engine_id, image_id, inventory, query_engine, valid_context_name, ContainerState,
    DockerOperation, DockerRead, MountKind,
};
use crate::docker_gc_apply::{DockerApplyGate, DockerGcService};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_EVIDENCE_BYTES: usize = 512;
const MAX_PLAN_RESOURCES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageGcDisposition {
    Candidate,
    Preserve,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageGcReason {
    Eligible,
    InvalidSelector,
    EngineUnverified,
    LeaseActive,
    InventoryIncomplete,
    ImageMissing,
    ReferencesUnverified,
    ContainerReference,
    NamedReference,
    OwnershipUnverified,
    EvidenceUnverified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGcPlan {
    pub context: String,
    pub engine_id: Option<String>,
    pub image_id: String,
    pub disposition: ImageGcDisposition,
    pub reason: ImageGcReason,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub apply_gate: DockerApplyGate,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImageEvidence {
    id: String,
    owner_matches: Option<bool>,
    tag_count: u64,
    digest_count: u64,
}

pub(crate) fn valid_owner_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn valid_full_image_id(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(valid_owner_fingerprint)
}

impl ImageGcPlan {
    fn preserved(mut self, reason: ImageGcReason) -> Self {
        self.reason = reason;
        self
    }
}

/// Evaluate one image explicitly selected by a registered project.
pub fn plan_image(
    query: &impl DockerRead,
    expected_engine: &str,
    expected_image: &str,
    owner_fingerprint: &str,
    has_open_lease: bool,
) -> ImageGcPlan {
    use ImageGcReason::*;

    let context = query.context();
    let mut plan = ImageGcPlan {
        context: valid_context_name(context)
            .then_some(context)
            .unwrap_or_default()
            .to_owned(),
        engine_id: None,
        image_id: valid_full_image_id(expected_image)
            .then_some(expected_image)
            .unwrap_or_default()
            .to_owned(),
        disposition: ImageGcDisposition::Preserve,
        reason: InvalidSelector,
        fingerprint: None,
        apply_gate: DockerGcService::external().apply_gate(),
    };
    if plan.context.is_empty()
        || plan.image_id.is_empty()
        || engine_id(expected_engine).is_none()
        || !valid_owner_fingerprint(owner_fingerprint)
    {
        return plan;
    }
    if has_open_lease {
        return plan.preserved(LeaseActive);
    }
    if query_engine(query).as_deref() != Ok(expected_engine) {
        return plan.preserved(EngineUnverified);
    }
    let snapshot = inventory(query, MAX_PLAN_RESOURCES);
    if snapshot.engine_id.as_deref() != Some(expected_engine) {
        return plan.preserved(EngineUnverified);
    }
    plan.engine_id = snapshot.engine_id;
    if !snapshot.complete {
        return plan.preserved(InventoryIncomplete);
    }
    let Some(image) = snapshot
        .images
        .iter()
        .find(|image| image.id == expected_image)
    else {
        return plan.preserved(ImageMissing);
    };
    if snapshot.containers.iter().any(|container| {
        container.state == ContainerState::Unknown
            || container
                .mounts
                .iter()
                .any(|mount| mount.kind == MountKind::Image)
    }) {
        return plan.preserved(ReferencesUnverified);
    }
    if snapshot
        .containers
        .iter()
        .any(|container| container.image_id.as_deref() == Some(expected_image))
    {
        return plan.preserved(ContainerReference);
    }
    if !image.digests.is_empty() {
        return plan.preserved(NamedReference);
    }

    let evidence = query_evidence(query, expected_image, owner_fingerprint);
    if query_engine(query).as_deref() != Ok(expected_engine) {
        plan.engine_id = None;
        return plan.preserved(EngineUnverified);
    }
    let Some(evidence) = evidence else {
        return plan.preserved(EvidenceUnverified);
    };
    if evidence.tag_count != 0 || evidence.digest_count != 0 {
        return plan.preserved(NamedReference);
    }
    if evidence.owner_matches != Some(true) {
        return plan.preserved(OwnershipUnverified);
    }

    let Ok(snapshot) = serde_json::to_vec(&(
        "dsg-image-gc-v1",
        context,
        expected_engine,
        owner_fingerprint,
        image,
        evidence,
    )) else {
        return plan.preserved(EvidenceUnverified);
    };
    plan.fingerprint = Some(format!("{:x}", Sha256::digest(snapshot)));
    plan.disposition = ImageGcDisposition::Candidate;
    plan.reason = Eligible;
    plan
}

fn query_evidence(
    query: &impl DockerRead,
    expected_image: &str,
    owner_fingerprint: &str,
) -> Option<ImageEvidence> {
    let response = query
        .query(DockerOperation::ImageGcEvidence {
            id: expected_image,
            owner_fingerprint,
        })
        .ok()?;
    if response.len() > MAX_EVIDENCE_BYTES || !response.trim_start().starts_with('{') {
        return None;
    }
    let mut evidence: ImageEvidence = serde_json::from_str(&response).ok()?;
    evidence.id = image_id(&evidence.id)?;
    (evidence.id == expected_image).then_some(evidence)
}

/// Compare a stored candidate with its current read-only evaluation.
pub fn revalidate_images(stored: &ImageGcPlan, current: &ImageGcPlan) -> bool {
    stored.disposition == ImageGcDisposition::Candidate
        && stored.reason == ImageGcReason::Eligible
        && stored
            .fingerprint
            .as_deref()
            .is_some_and(valid_owner_fingerprint)
        && stored == current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::QueryFailure;
    use serde_json::{json, Value};
    use std::cell::Cell;

    const ENGINE: &str = "isolated-engine";
    const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_IMAGE: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CONTAINER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const OWNER: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    struct Fixture {
        evidence: Value,
        container: Option<Value>,
        engine_reads: Cell<u8>,
        change_engine_after_evidence: bool,
        incomplete: bool,
        architecture: &'static str,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                evidence: json!({
                    "id": IMAGE, "owner_matches": true, "tag_count": 0, "digest_count": 0,
                }),
                container: None,
                engine_reads: Cell::new(0),
                change_engine_after_evidence: false,
                incomplete: false,
                architecture: "arm64",
            }
        }

        fn plan(&self) -> ImageGcPlan {
            plan_image(self, ENGINE, IMAGE, OWNER, false)
        }

        fn with_container(state: &str, image: &str, mounts: Value) -> Self {
            Self {
                container: Some(json!({
                    "id": CONTAINER, "image_id": image, "state": state, "health": null,
                    "compose": null, "mounts": mounts,
                })),
                ..Self::new()
            }
        }
    }

    impl DockerRead for Fixture {
        fn context(&self) -> &str {
            "isolated-context"
        }

        fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
            match operation {
                DockerOperation::Engine => {
                    let reads = self.engine_reads.get() + 1;
                    self.engine_reads.set(reads);
                    Ok(if reads == 4 && self.change_engine_after_evidence {
                        "different-engine"
                    } else {
                        ENGINE
                    }
                    .to_owned())
                }
                DockerOperation::Containers => Ok(if self.container.is_some() {
                    CONTAINER.to_owned()
                } else {
                    String::new()
                }),
                DockerOperation::Container(CONTAINER) => self
                    .container
                    .as_ref()
                    .map(Value::to_string)
                    .ok_or(QueryFailure::Unavailable),
                DockerOperation::Images => Ok(format!("{IMAGE}\n{OTHER_IMAGE}")),
                DockerOperation::Image(id) => Ok(json!({
                    "id": id, "digests": [], "os": "linux", "architecture": self.architecture,
                })
                .to_string()),
                DockerOperation::Volumes if !self.incomplete => Ok(String::new()),
                DockerOperation::ImageGcEvidence {
                    id: IMAGE,
                    owner_fingerprint: OWNER,
                } => Ok(self.evidence.to_string()),
                _ => Err(QueryFailure::Unavailable),
            }
        }
    }

    fn assert_preserved(plan: &ImageGcPlan, reason: ImageGcReason) {
        assert_eq!(plan.disposition, ImageGcDisposition::Preserve);
        assert_eq!(plan.reason, reason);
        assert_eq!(plan.fingerprint, None);
    }

    #[test]
    fn revalidation_requires_the_same_engine_scope_and_image_snapshot() {
        let saved = Fixture::new().plan();
        assert_eq!(saved.disposition, ImageGcDisposition::Candidate);
        assert!(revalidate_images(&saved, &Fixture::new().plan()));
        let changed_image = Fixture {
            architecture: "amd64",
            ..Fixture::new()
        }
        .plan();
        assert_eq!(changed_image.disposition, ImageGcDisposition::Candidate);
        assert!(!revalidate_images(&saved, &changed_image));
        let changed_engine = Fixture {
            change_engine_after_evidence: true,
            ..Fixture::new()
        }
        .plan();
        assert_preserved(&changed_engine, ImageGcReason::EngineUnverified);
        assert_eq!(changed_engine.engine_id, None);
        assert!(!revalidate_images(&saved, &changed_engine));
    }

    #[test]
    fn preserves_all_referenced_or_unverified_images() {
        for state in ["running", "exited"] {
            let referenced = Fixture::with_container(state, IMAGE, json!([])).plan();
            assert_preserved(&referenced, ImageGcReason::ContainerReference);
        }
        let image_mount = Fixture::with_container(
            "exited",
            OTHER_IMAGE,
            json!([{
                "kind": "image", "volume": null, "destination": "/assets", "read_only": true,
            }]),
        )
        .plan();
        assert_preserved(&image_mount, ImageGcReason::ReferencesUnverified);
        let missing = Fixture {
            incomplete: true,
            ..Fixture::new()
        }
        .plan();
        assert_preserved(&missing, ImageGcReason::InventoryIncomplete);
    }

    #[test]
    fn preserves_images_with_named_references_or_without_ownership() {
        let saved = Fixture::new().plan();
        for field in ["tag_count", "digest_count", "owner_matches", "description"] {
            let mut fixture = Fixture::new();
            let reason = match field {
                "description" => {
                    fixture.evidence[field] = json!("fixture marker omitted from reports");
                    ImageGcReason::EvidenceUnverified
                }
                "owner_matches" => {
                    fixture.evidence[field] = json!(false);
                    ImageGcReason::OwnershipUnverified
                }
                _ => {
                    fixture.evidence[field] = json!(1);
                    ImageGcReason::NamedReference
                }
            };
            let current = fixture.plan();
            assert_preserved(&current, reason);
            assert!(!revalidate_images(&saved, &current));
            assert!(!serde_json::to_string(&current)
                .unwrap()
                .contains("fixture marker"));
        }
    }

    #[test]
    fn rejects_invalid_selectors_and_preserves_open_leases_before_docker_queries() {
        let fixture = Fixture::new();
        let invalid = plan_image(&fixture, ENGINE, "alias:latest", OWNER, false);
        assert_preserved(&invalid, ImageGcReason::InvalidSelector);
        assert!(invalid.image_id.is_empty());
        let active = plan_image(&fixture, ENGINE, IMAGE, OWNER, true);
        assert_preserved(&active, ImageGcReason::LeaseActive);
        assert_eq!(fixture.engine_reads.get(), 0);
    }
}
