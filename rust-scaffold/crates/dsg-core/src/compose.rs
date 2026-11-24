use crate::docker::{digest, hex_id, sort_mounts, DockerRead, Mount, MountKind, ReuseRequest};
use crate::reuse::{verify_reuse_with_platform, ReuseVerification};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_PROJECTION_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeUnknown {
    InvalidProjection,
    InputTooLarge,
    InputUnavailable,
    BaselineMismatch,
    BaselineUnverified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeAction {
    Rebuild,
    Recreate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ComposeAssessment {
    Same,
    Changed { action: ComposeAction },
    Unknown { reason: ComposeUnknown },
}

#[derive(Debug, Serialize)]
pub struct ComposeVerification {
    pub verified: bool,
    pub baseline: Option<ReuseVerification>,
    pub compose: ComposeAssessment,
}

impl ComposeVerification {
    pub fn unknown(reason: ComposeUnknown) -> Self {
        Self {
            verified: false,
            baseline: None,
            compose: ComposeAssessment::Unknown { reason },
        }
    }
}

pub struct ComposeComparison<'a> {
    expected: &'a ReuseRequest<'a>,
    platform: Platform,
    assessment: ComposeAssessment,
}

impl<'a> ComposeComparison<'a> {
    /// Check an operator-provided baseline projection against the legacy Docker expectation.
    /// The operator must supply the service projection that corresponds to the observed label.
    pub fn prepare(input: &[u8], expected: &'a ReuseRequest<'a>) -> Result<Self, ComposeUnknown> {
        if input.len() > MAX_PROJECTION_BYTES {
            return Err(ComposeUnknown::InputTooLarge);
        }
        let Projection {
            baseline_config_hash,
            mut baseline,
            mut desired,
        } = serde_json::from_slice(input).map_err(|_| ComposeUnknown::InvalidProjection)?;
        let baseline_hash =
            hex_id(&baseline_config_hash).ok_or(ComposeUnknown::InvalidProjection)?;
        let baseline_digest = baseline
            .normalize()
            .ok_or(ComposeUnknown::InvalidProjection)?;
        desired
            .normalize()
            .ok_or(ComposeUnknown::InvalidProjection)?;

        let mut expected_mounts = expected.mounts.to_vec();
        if hex_id(expected.config_hash).as_ref() != Some(&baseline_hash)
            || digest(expected.image_digest).as_ref() != Some(&baseline_digest)
            || sort_mounts(&mut expected_mounts).is_err()
            || baseline.mounts() != expected_mounts
        {
            return Err(ComposeUnknown::BaselineMismatch);
        }

        Ok(Self {
            expected,
            platform: baseline.service.platform,
            assessment: compare(&baseline, &desired),
        })
    }

    pub fn verify(self, query: &impl DockerRead, max_resources: usize) -> ComposeVerification {
        let baseline = verify_reuse_with_platform(
            query,
            max_resources,
            self.expected,
            Some(self.platform.os_arch()),
        );
        let compose = if baseline.verified {
            self.assessment
        } else {
            ComposeAssessment::Unknown {
                reason: ComposeUnknown::BaselineUnverified,
            }
        };
        ComposeVerification {
            verified: compose == ComposeAssessment::Same,
            baseline: Some(baseline),
            compose,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    baseline_config_hash: String,
    baseline: Snapshot,
    desired: Snapshot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    service: Service,
    #[serde(deserialize_with = "required_nullable_digest")]
    build_inputs_sha256: Option<String>,
}

fn required_nullable_digest<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Service {
    image: String,
    platform: Platform,
    init: bool,
    read_only: bool,
    volumes: Vec<VolumeMount>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
enum Platform {
    #[serde(rename = "linux/amd64")]
    LinuxAmd64,
    #[serde(rename = "linux/arm64")]
    LinuxArm64,
}

impl Platform {
    fn os_arch(self) -> (&'static str, &'static str) {
        match self {
            Self::LinuxAmd64 => ("linux", "amd64"),
            Self::LinuxArm64 => ("linux", "arm64"),
        }
    }
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct VolumeMount {
    #[serde(rename = "type")]
    _kind: VolumeType,
    source: String,
    target: String,
    read_only: bool,
    volume: VolumeOptions,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum VolumeType {
    Volume,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct VolumeOptions {
    nocopy: bool,
}

impl Snapshot {
    fn normalize(&mut self) -> Option<String> {
        let (reference, checksum) = self.service.image.split_once('@')?;
        valid_image_reference(reference).then_some(())?;
        let image_digest = digest(checksum)?;
        self.service.image = format!("{reference}@{image_digest}");
        if let Some(build_inputs) = &mut self.build_inputs_sha256 {
            *build_inputs = hex_id(build_inputs)?;
        }
        if !self.service.volumes.iter().all(|volume| {
            volume.target.strip_prefix('/').is_some_and(|target| {
                target.split('/').all(|component| !component.is_empty())
                    && !target.contains(['\\', ' '])
            })
        }) {
            return None;
        }
        sort_mounts(&mut self.mounts()).ok()?;
        self.service
            .volumes
            .sort_unstable_by(|left, right| left.target.cmp(&right.target));
        Some(image_digest)
    }

    fn mounts(&self) -> Vec<Mount> {
        self.service
            .volumes
            .iter()
            .map(|volume| Mount {
                kind: MountKind::Volume,
                volume: Some(volume.source.clone()),
                destination: volume.target.clone(),
                read_only: volume.read_only,
            })
            .collect()
    }
}

fn valid_image_reference(reference: &str) -> bool {
    fn valid_component(value: &str, separators: &[char]) -> bool {
        value.split(separators).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    }
    let Some((registry, repository)) = reference.split_once('/') else {
        return false;
    };
    reference.len() <= 255
        && registry.contains('.')
        && registry
            .split('.')
            .all(|label| valid_component(label, &['-']))
        && repository
            .split('/')
            .all(|part| valid_component(part, &['.', '_', '-']))
}

fn compare(baseline: &Snapshot, desired: &Snapshot) -> ComposeAssessment {
    if desired.build_inputs_sha256.is_some()
        && (desired.build_inputs_sha256 != baseline.build_inputs_sha256
            || desired.service.platform != baseline.service.platform)
    {
        ComposeAssessment::Changed {
            action: ComposeAction::Rebuild,
        }
    } else if desired.service != baseline.service
        || desired.build_inputs_sha256 != baseline.build_inputs_sha256
    {
        ComposeAssessment::Changed {
            action: ComposeAction::Recreate,
        }
    } else {
        ComposeAssessment::Same
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CHANGED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DIGEST: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn request(mounts: &[Mount]) -> ReuseRequest<'_> {
        ReuseRequest {
            engine_id: "fixture-engine",
            project: "fixture",
            service: "app",
            number: 1,
            config_hash: HASH,
            image_id: DIGEST,
            image_digest: DIGEST,
            mounts,
        }
    }

    fn volume(source: &str, target: &str) -> Value {
        json!({
            "type": "volume", "source": source, "target": target,
            "read_only": true, "volume": {"nocopy": true}
        })
    }

    fn projection(volumes: Vec<Value>) -> Value {
        let baseline = json!({
            "service": {
                "image": format!("docker.io/library/alpine@{DIGEST}"),
                "platform": "linux/arm64", "init": false, "read_only": false,
                "volumes": volumes,
            },
            "build_inputs_sha256": HASH,
        });
        json!({ "baseline_config_hash": HASH, "baseline": baseline, "desired": baseline })
    }

    #[test]
    fn compose_projection_normalizes_volume_order_and_hash_case() {
        let mut input = projection(vec![volume("data-b", "/b"), volume("data-a", "/a")]);
        input["baseline_config_hash"] = json!(HASH.to_ascii_uppercase());
        input["desired"]["service"]["image"] = json!(format!(
            "docker.io/library/alpine@sha256:{}",
            DIGEST[7..].to_ascii_uppercase()
        ));
        input["desired"]["service"]["volumes"]
            .as_array_mut()
            .unwrap()
            .reverse();
        input["desired"]["build_inputs_sha256"] = json!(HASH.to_ascii_uppercase());
        let mounts = [("data-b", "/b"), ("data-a", "/a")].map(|(volume, destination)| Mount {
            kind: MountKind::Volume,
            volume: Some(volume.to_owned()),
            destination: destination.to_owned(),
            read_only: true,
        });
        let expected = request(&mounts);
        let same = ComposeComparison::prepare(input.to_string().as_bytes(), &expected).unwrap();
        assert_eq!(same.assessment, ComposeAssessment::Same);

        input["desired"]["service"]["volumes"][0]["volume"]["nocopy"] = json!(false);
        let changed = ComposeComparison::prepare(input.to_string().as_bytes(), &expected).unwrap();
        assert_eq!(
            changed.assessment,
            ComposeAssessment::Changed {
                action: ComposeAction::Recreate
            }
        );

        input["baseline"]["service"]["volumes"][0]["source"] = json!("data-changed");
        assert_eq!(
            ComposeComparison::prepare(input.to_string().as_bytes(), &expected).err(),
            Some(ComposeUnknown::BaselineMismatch)
        );
    }

    #[test]
    fn compose_projection_distinguishes_runtime_changes_from_build_inputs() {
        let expected = request(&[]);
        for (key, value, action) in [
            (
                "build_inputs_sha256",
                json!(CHANGED),
                ComposeAction::Rebuild,
            ),
            ("build_inputs_sha256", Value::Null, ComposeAction::Recreate),
        ] {
            let mut input = projection(vec![]);
            input["desired"][key] = value;
            let changed =
                ComposeComparison::prepare(input.to_string().as_bytes(), &expected).unwrap();
            assert_eq!(changed.assessment, ComposeAssessment::Changed { action });
        }

        for (baseline_build, action) in [
            (json!(HASH), ComposeAction::Rebuild),
            (Value::Null, ComposeAction::Recreate),
        ] {
            let mut input = projection(vec![]);
            input["baseline"]["build_inputs_sha256"] = baseline_build.clone();
            input["desired"]["build_inputs_sha256"] = baseline_build;
            input["desired"]["service"]["platform"] = json!("linux/amd64");
            let changed =
                ComposeComparison::prepare(input.to_string().as_bytes(), &expected).unwrap();
            assert_eq!(changed.assessment, ComposeAssessment::Changed { action });
        }
    }

    #[test]
    fn compose_projection_keeps_unrecognized_or_incomplete_inputs_unknown() {
        let expected = request(&[]);
        let invalid = |input: &Value| {
            assert_eq!(
                ComposeComparison::prepare(input.to_string().as_bytes(), &expected).err(),
                Some(ComposeUnknown::InvalidProjection)
            );
        };
        for (field, value) in [
            ("unknown", json!(true)),
            ("init", json!("false")),
            ("image", json!("docker.io/library/alpine@${SYNTHETIC}")),
            ("image", json!("docker.io/library/alpine:latest")),
            ("platform", json!("linux/arm/v7")),
            ("volumes", json!([volume("fixture", "/data/../path")])),
            (
                "volumes",
                json!([volume("a", "/data"), volume("b", "/data")]),
            ),
        ] {
            let mut input = projection(vec![]);
            input["desired"]["service"][field] = value;
            invalid(&input);
        }
        let mut missing = projection(vec![]);
        missing["desired"]
            .as_object_mut()
            .unwrap()
            .remove("build_inputs_sha256");
        invalid(&missing);
        let mut duplicate = projection(vec![]).to_string();
        duplicate = duplicate.replace("\"init\":false", "\"init\":false,\"init\":true");
        assert_eq!(
            ComposeComparison::prepare(duplicate.as_bytes(), &expected).err(),
            Some(ComposeUnknown::InvalidProjection)
        );
        assert_eq!(
            ComposeComparison::prepare(&vec![b' '; MAX_PROJECTION_BYTES + 1], &expected).err(),
            Some(ComposeUnknown::InputTooLarge)
        );
    }
}
