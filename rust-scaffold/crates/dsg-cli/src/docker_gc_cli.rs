use super::{open_registry, policy_digest, registry_error, Cli, CliError, Output};
use crate::execution::{project_id, scope_key};
use dsg_core::docker_command::DockerCli;
use dsg_core::docker_image_gc::{plan_image, revalidate_images, ImageGcPlan};
use dsg_core::policy::Policy;
use dsg_core::registry::Registry;
use serde_json::json;
use std::path::Path;

pub(super) fn observe(
    registry: &Registry,
    root: &Path,
    context: &str,
    engine: &str,
    image: &str,
) -> Result<ImageGcPlan, CliError> {
    let backend = DockerCli::new(context)
        .map_err(|_| CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요"))?;
    let ownership = scope_key(root);
    let active = registry
        .has_open_execution_lease()
        .map_err(registry_error)?;
    let plan = plan_image(&backend, engine, image, &ownership, active);
    if !active
        && registry
            .has_open_execution_lease()
            .map_err(registry_error)?
    {
        return Ok(plan_image(&backend, engine, image, &ownership, true));
    }
    Ok(plan)
}

pub(super) fn recheck(
    cli: &Cli,
    root: &Path,
    policy: &Policy,
    id: i64,
) -> Result<Output, CliError> {
    let registry = open_registry(cli)?;
    let project = project_id(&registry, root, policy)?;
    let digest = policy_digest(policy)?;
    let payload = registry
        .read_current_gc_plan(project, id, &digest)
        .map_err(|_| {
            CliError::new(
                "IMAGE_PLAN_STALE",
                "현재 정책의 유효한 이미지 계획을 확인하세요",
            )
        })?;
    let prior: ImageGcPlan = serde_json::from_value(payload["docker_images"].clone())
        .map_err(|_| CliError::new("IMAGE_PLAN_STALE", "저장된 이미지 계획을 확인하세요"))?;
    let engine = prior.engine_id.as_deref().ok_or_else(|| {
        CliError::new("IMAGE_PLAN_STALE", "저장된 이미지 계획의 엔진을 확인하세요")
    })?;
    let current = observe(&registry, root, &prior.context, engine, &prior.image_id)?;
    let verified = revalidate_images(&prior, &current);
    Ok(Output {
        command: "docker-image-recheck",
        data: json!({ "plan_id": id, "verified": verified, "image": current }),
        text: if verified {
            "이미지 계획 재확인"
        } else {
            "이미지 계획 변경 또는 미확인"
        }
        .to_owned(),
        exit: if verified { 0 } else { 5 },
    })
}
