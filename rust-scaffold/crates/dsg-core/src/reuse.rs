use crate::docker::{
    inventory, DockerRead, ReuseAssessment, ReuseReason, ReuseRequest, ReuseStatus,
};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReuseVerificationReason {
    Verified,
    InitialUnverified { assessment_reason: ReuseReason },
    FinalUnverified,
    CandidateChanged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReuseVerification {
    pub verified: bool,
    pub reason: ReuseVerificationReason,
    pub assessment: ReuseAssessment,
}

/// Verify that two bounded read-only observations select the same Compose instance.
/// Both observations share the caller's transport and its deadline.
/// Docker resources can change between observations or after this call returns.
pub fn verify_reuse(
    query: &impl DockerRead,
    max_resources: usize,
    expected: &ReuseRequest<'_>,
) -> ReuseVerification {
    let initial = inventory(query, max_resources).assess_reuse(expected);
    let assessment = inventory(query, max_resources).assess_reuse(expected);
    let reason = match (candidate_id(&initial), candidate_id(&assessment)) {
        (None, _) => ReuseVerificationReason::InitialUnverified {
            assessment_reason: initial.reason,
        },
        (Some(_), None) => ReuseVerificationReason::FinalUnverified,
        (Some(first), Some(last)) if first != last => ReuseVerificationReason::CandidateChanged,
        (Some(_), Some(_)) => ReuseVerificationReason::Verified,
    };
    ReuseVerification {
        verified: reason == ReuseVerificationReason::Verified,
        reason,
        assessment,
    }
}

fn candidate_id(assessment: &ReuseAssessment) -> Option<&str> {
    match assessment {
        ReuseAssessment {
            status: ReuseStatus::Candidate,
            container_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{DockerOperation, QueryFailure};
    use serde_json::json;
    use std::cell::Cell;

    const ENGINE: &str = "fixture-engine";
    const FIRST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SECOND: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const IMAGE: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const DIGEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const HASH: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    #[derive(Clone, Copy)]
    struct Observation {
        ids: &'static [&'static str],
        health: &'static str,
        image_available: bool,
    }

    const HEALTHY: Observation = Observation {
        ids: &[FIRST],
        health: "healthy",
        image_available: true,
    };

    struct Fixture {
        observations: [Observation; 2],
        scans: Cell<usize>,
    }

    impl Fixture {
        fn new(observations: [Observation; 2]) -> Self {
            Self {
                observations,
                scans: Cell::new(0),
            }
        }
    }

    impl DockerRead for Fixture {
        fn context(&self) -> &str {
            "fixture"
        }

        fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
            match operation {
                DockerOperation::Engine => Ok(ENGINE.to_owned()),
                DockerOperation::Containers => {
                    let scans = self.scans.get();
                    self.scans.set(scans + 1);
                    Ok(self.observations[scans].ids.join("\n"))
                }
                DockerOperation::Container(id) => {
                    let observed = self.observations[self.scans.get() - 1];
                    assert!(observed.ids.contains(&id));
                    Ok(json!({
                        "id": id,
                        "image_id": IMAGE,
                        "state": "running",
                        "health": observed.health,
                        "compose": {
                            "project": "shop", "service": "db", "number": 1,
                            "config_hash": HASH, "oneoff": "False"
                        },
                        "mounts": [],
                    })
                    .to_string())
                }
                DockerOperation::Images => Ok(IMAGE.to_owned()),
                DockerOperation::Image(id) => {
                    assert_eq!(id, IMAGE);
                    if !self.observations[self.scans.get() - 1].image_available {
                        return Err(QueryFailure::Timeout);
                    }
                    Ok(json!({
                        "id": IMAGE, "digests": [DIGEST], "os": "linux", "architecture": "arm64"
                    })
                    .to_string())
                }
                DockerOperation::Volumes => Ok(String::new()),
                DockerOperation::BuildCache | DockerOperation::ImageGcEvidence { .. } => {
                    Err(QueryFailure::Unavailable)
                }
            }
        }
    }

    fn request() -> ReuseRequest<'static> {
        ReuseRequest {
            engine_id: ENGINE,
            project: "shop",
            service: "db",
            number: 1,
            config_hash: HASH,
            image_id: IMAGE,
            image_digest: DIGEST,
            mounts: &[],
        }
    }

    #[test]
    fn verifies_the_same_single_candidate_across_two_reads() {
        let fixture = Fixture::new([HEALTHY, HEALTHY]);
        let result = verify_reuse(&fixture, 3, &request());
        assert_eq!(fixture.scans.get(), 2);
        assert!(result.verified);
        assert_eq!(result.reason, ReuseVerificationReason::Verified);
        assert_eq!(result.assessment.container_id.as_deref(), Some(FIRST));
    }

    #[test]
    fn refuses_a_replacement_even_when_each_observation_has_a_candidate() {
        let replaced = Observation {
            ids: &[SECOND],
            ..HEALTHY
        };
        let result = verify_reuse(&Fixture::new([HEALTHY, replaced]), 3, &request());
        assert!(!result.verified);
        assert_eq!(result.reason, ReuseVerificationReason::CandidateChanged);
        assert_eq!(result.assessment.status, ReuseStatus::Candidate);
        assert_eq!(result.assessment.container_id.as_deref(), Some(SECOND));
    }

    #[test]
    fn refuses_when_the_last_observation_is_unhealthy_or_ambiguous() {
        let unhealthy = Observation {
            health: "unhealthy",
            ..HEALTHY
        };
        let ambiguous = Observation {
            ids: &[FIRST, SECOND],
            ..HEALTHY
        };
        for (last, assessment_reason) in [
            (unhealthy, ReuseReason::NotHealthy),
            (ambiguous, ReuseReason::AmbiguousInstance),
        ] {
            let result = verify_reuse(&Fixture::new([HEALTHY, last]), 3, &request());
            assert!(!result.verified);
            assert_eq!(result.reason, ReuseVerificationReason::FinalUnverified);
            assert_eq!(result.assessment.reason, assessment_reason);
        }
    }

    #[test]
    fn last_candidate_does_not_override_an_initial_incomplete_read() {
        let timeout = Observation {
            image_available: false,
            ..HEALTHY
        };
        let result = verify_reuse(&Fixture::new([timeout, HEALTHY]), 3, &request());
        assert!(!result.verified);
        assert_eq!(
            result.reason,
            ReuseVerificationReason::InitialUnverified {
                assessment_reason: ReuseReason::IncompleteInventory,
            }
        );
        assert_eq!(result.assessment.status, ReuseStatus::Candidate);
    }
}
