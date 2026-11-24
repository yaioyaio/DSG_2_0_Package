pub mod cleanup;
pub mod docker;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod docker_buildkit_local;
pub mod docker_cache;
pub mod docker_command;
pub mod docker_image_gc;
pub mod filesystem;
pub mod gc;
pub mod policy;
pub mod registry;
pub mod reuse;
pub mod runner;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Warn,
    Deny,
    ApprovalRequired,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::ApprovalRequired => "approval_required",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Verdict {
    pub decision: Decision,
    pub reason_code: &'static str,
    pub message: &'static str,
    pub evidence_ids: Vec<i64>,
    pub suggested_action: &'static str,
}

impl Verdict {
    pub fn new(
        decision: Decision,
        reason_code: &'static str,
        message: &'static str,
        suggested_action: &'static str,
    ) -> Self {
        Self {
            decision,
            reason_code,
            message,
            evidence_ids: Vec::new(),
            suggested_action,
        }
    }
}

pub fn check_command(command: &str) -> Decision {
    evaluate_command(&command.split_whitespace().collect::<Vec<_>>()).decision
}

pub fn evaluate_command(argv: &[impl AsRef<str>]) -> Verdict {
    let Some(program) = argv.first() else {
        return Verdict::new(
            Decision::ApprovalRequired,
            "COMMAND_UNKNOWN",
            "실행할 명령을 확인할 수 없습니다",
            "inspect",
        );
    };
    let executable = program
        .as_ref()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let executable = executable.strip_suffix(".exe").unwrap_or(&executable);
    let args = argv[1..]
        .iter()
        .map(|arg| arg.as_ref().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    match (executable, args.as_slice()) {
        ("docker", ["volume", "prune" | "rm", ..]) => {
            return Verdict::new(
                Decision::Deny,
                "PROTECTED_VOLUME",
                "볼륨 삭제 요청입니다",
                "inspect",
            );
        }
        ("docker", ["compose", "down", rest @ ..] | ["system", "prune", rest @ ..])
            if rest
                .iter()
                .any(|flag| matches!(*flag, "--volumes" | "-v" | "-a" | "--all")) =>
        {
            return Verdict::new(
                Decision::Deny,
                "PROTECTED_VOLUME",
                "보호 가능한 Docker 자원 삭제 요청입니다",
                "inspect",
            );
        }
        ("rm", rest)
            if rest.iter().any(|flag| {
                *flag == "--recursive"
                    || (flag.starts_with('-') && !flag.starts_with("--") && flag.contains('r'))
            }) =>
        {
            return Verdict::new(
                Decision::Deny,
                "RECURSIVE_DELETE",
                "재귀 삭제 요청입니다",
                "inspect",
            );
        }
        ("docker", ["system" | "image" | "container" | "builder" | "network", "prune", ..]) => {
            return Verdict::new(
                Decision::ApprovalRequired,
                "NEEDS_APPROVAL",
                "정리 범위와 승인 여부를 확인하세요",
                "inspect",
            );
        }
        ("sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh", _) => {
            return Verdict::new(
                Decision::ApprovalRequired,
                "COMMAND_UNKNOWN",
                "간접 실행의 명령 범위를 확인하세요",
                "inspect",
            );
        }
        ("docker", [flag, ..] | ["compose", flag, ..]) if flag.starts_with('-') => {
            return Verdict::new(
                Decision::ApprovalRequired,
                "DOCKER_OPTIONS_UNKNOWN",
                "Docker 옵션을 포함한 명령의 범위를 확인하세요",
                "inspect",
            );
        }
        ("docker", ["build" | "run" | "create", ..] | ["compose", "up", ..])
        | ("cargo" | "pnpm", ["build" | "test", ..])
        | ("npm", ["run", "build", ..])
        | ("playwright", ["test", ..]) => {
            return Verdict::new(
                Decision::Warn,
                "MANAGED_EXECUTION_RECOMMENDED",
                "실행 전에 프로젝트 예산과 활성 작업을 확인하세요",
                "scan",
            );
        }
        _ => {}
    }
    Verdict::new(
        Decision::Allow,
        "NO_KNOWN_RISK",
        "등록된 위험 규칙에 해당하지 않습니다",
        "none",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_explicit_requests() {
        assert_eq!(check_command("docker volume prune"), Decision::Deny);
        assert_eq!(check_command("docker compose down -v"), Decision::Deny);
        assert_eq!(
            check_command("docker system prune"),
            Decision::ApprovalRequired
        );
        assert_eq!(
            check_command("docker compose -f fixture.yaml up"),
            Decision::ApprovalRequired
        );
        assert_eq!(check_command("docker build ."), Decision::Warn);
        assert_eq!(check_command("cargo --version"), Decision::Allow);
    }
}
