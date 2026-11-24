use clap::{Parser, Subcommand};
use dsg_core::docker;
use dsg_core::docker_cache::{build_cache_inventory, buildx_cache_inventory};
use dsg_core::docker_command::{discover_contexts, DockerCli};
use dsg_core::filesystem::{self, ScanReport};
use dsg_core::policy::{self, Policy, PolicyError, POLICY_VERSION, PROJECT_CONFIG};
use dsg_core::registry::{AuditAction, AuditOutcome, Registry};
use dsg_core::{evaluate_command, Decision, Verdict};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod cleanup_cli;
mod dashboard;
mod docker_gc_cli;
mod execution;
mod hook_install;
mod hooks;
mod mcp;
mod mcp_state;

#[derive(Parser)]
#[command(
    name = "dsg",
    version,
    about = "프로젝트 사용량과 정리 후보를 확인합니다"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    global_policy: Option<PathBuf>,
    #[arg(long, global = true)]
    session_policy: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 프로젝트 설정을 만들어 로컬 상태에 등록합니다.
    Init {
        #[arg(long)]
        project_id: Option<String>,
    },
    /// 유효 정책과 측정 상태를 확인합니다.
    Doctor,
    /// 등록된 대상의 논리 사용량과 관측 완전성을 확인합니다.
    Scan,
    /// 입력 명령의 알려진 위험과 예산을 평가합니다.
    Check {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// 파일을 변경하지 않고 정리 후보의 계획을 기록합니다.
    GcPlan {
        #[arg(long, requires_all = ["docker_engine", "docker_image"])]
        docker_context: Option<String>,
        #[arg(long, requires = "docker_context")]
        docker_engine: Option<String>,
        #[arg(long, requires = "docker_context")]
        docker_image: Option<String>,
    },
    /// 저장한 Docker 이미지 계획을 같은 엔진에서 재확인합니다.
    DockerImageRecheck {
        #[arg(long)]
        plan: i64,
    },
    /// 소유권을 확인한 생성 폴더를 정리 대상으로 등록합니다.
    GcOwn {
        #[arg(long)]
        candidate: String,
        #[arg(long, required = true)]
        confirm_generated: bool,
    },
    /// 계획의 지정 후보에만 일회용 적용 승인을 발급합니다.
    GcApprove {
        #[arg(long)]
        plan: i64,
        #[arg(long)]
        candidate: String,
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// 표준 입력의 승인으로 지정 후보를 다시 검증하고 적용합니다.
    GcApply {
        #[arg(long)]
        plan: i64,
        #[arg(long, required = true)]
        approval_stdin: bool,
    },
    /// 적용 도중 격리한 산출물의 원래 경로 복원을 시도합니다.
    GcRecover {
        #[arg(long)]
        plan: i64,
    },
    /// 관리 대상 보조 Git 워크트리를 생성합니다.
    WorktreeCreate {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        destination: PathBuf,
    },
    /// 검증한 Git 완료 이벤트의 보조 워크트리를 확인합니다.
    CleanupGit {
        #[arg(long)]
        feature: String,
    },
    /// 로컬 Docker 구성에 등록된 컨텍스트 이름을 조회합니다.
    DockerContexts {
        #[arg(long, default_value_t = docker::MAX_CONTEXTS, value_parser = clap::value_parser!(u16).range(1..=i64::from(docker::MAX_CONTEXTS)))]
        max_contexts: u16,
    },
    /// 선택한 엔진의 빌드 캐시를 읽기 전용으로 집계합니다.
    DockerBuildCache {
        #[arg(long)]
        context: String,
    },
    /// 지정한 엔진의 기본 Buildx 빌더 캐시 상태를 조회합니다.
    DockerBuildxCache {
        #[arg(long)]
        context: String,
    },
    /// 운영자가 지정한 Linux ARM64 로컬 BuildKit 데몬의 캐시 상태를 조회합니다.
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    BuildkitLocalCache {
        /// 권한 0700인 전용 디렉터리 안의 절대 Unix 소켓 경로
        #[arg(long)]
        socket: PathBuf,
        /// 같은 PID 네임스페이스에 있는 데몬의 프로세스 ID
        #[arg(long, value_parser = clap::value_parser!(i32).range(1..))]
        expected_pid: i32,
        /// 공식 BuildKit v0.35.1 Linux ARM64 buildctl 실행 파일의 절대 경로
        #[arg(long)]
        buildctl: PathBuf,
    },
    /// 지정된 Docker 컨텍스트의 리소스와 중복 후보를 조회합니다.
    DockerScan {
        #[arg(long)]
        context: String,
        #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u16).range(1..=64))]
        max_resources: u16,
    },
    /// 디스크 정책으로 확인한 명령을 시간 제한과 lease 안에서 실행합니다.
    Exec {
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=120))]
        timeout: u16,
        #[arg(long)]
        auto_clean_cargo: bool,
        /// 등록된 빌드·테스트 Provider의 정형 작업을 실행합니다.
        #[arg(long)]
        provider: Option<String>,
        /// 검증된 Playwright 성공 trace의 보존 기간을 초 단위로 지정합니다.
        #[arg(long, requires = "provider", value_parser = clap::value_parser!(u64).range(1..=604_800))]
        trace_ttl_seconds: Option<u64>,
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// 기대 구성과 일치하는 실행 중 Compose 컨테이너를 확인합니다.
    DockerReuse {
        #[arg(long)]
        context: String,
        #[arg(long)]
        expectation_json: String,
        /// 기준 및 희망 서비스의 비민감 설정 투영 JSON을 표준 입력으로 받습니다.
        #[arg(long)]
        compose_projection_stdin: bool,
    },
    /// AI CLI에서 전달한 도구 사용 이벤트를 평가합니다.
    Hook {
        #[arg(long, value_enum)]
        agent: hooks::Agent,
    },
    /// 프로젝트 범위의 DSG 훅을 관리합니다.
    Agents {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// 표준 입출력에서 읽기 전용 MCP 서버를 실행합니다.
    Mcp,
    #[command(hide = true)]
    McpStateRead {
        #[arg(long)]
        plan_id: Option<i64>,
    },
    /// 프로젝트 관측값을 로컬 브라우저에서 조회합니다.
    Dashboard {
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u16).range(1..=3600))]
        lifetime: u16,
        #[arg(long)]
        docker_context: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    Install {
        #[arg(long, value_enum)]
        agent: hooks::Agent,
    },
    Uninstall {
        #[arg(long, value_enum)]
        agent: hooks::Agent,
    },
}

struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<PolicyError> for CliError {
    fn from(error: PolicyError) -> Self {
        let code = match error {
            PolicyError::AlreadyExists => "CONFIG_EXISTS",
            _ => "CONFIG_INVALID",
        };
        Self::new(code, error.to_string())
    }
}

struct Output {
    command: &'static str,
    data: Value,
    text: String,
    exit: u8,
}

impl Output {
    fn success(command: &'static str, data: Value, text: String) -> Self {
        Self {
            command,
            data,
            text,
            exit: 0,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        Command::Hook { agent } => return hooks::run(*agent),
        Command::Mcp => return mcp::run(&cli),
        Command::McpStateRead { plan_id } => return mcp_state::run(&cli, *plan_id),
        Command::Dashboard {
            port,
            lifetime,
            docker_context,
        } => return dashboard::run(&cli, *port, *lifetime, docker_context.as_deref()),
        _ => {}
    }
    match run(&cli) {
        Ok(output) => {
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": 1, "command": output.command, "result": output.data})
                );
            } else {
                println!("{}", output.text);
            }
            ExitCode::from(output.exit)
        }
        Err(error) => {
            if cli.json {
                eprintln!(
                    "{}",
                    json!({"schema_version": 1, "error": {"code": error.code, "message": error.message}})
                );
            } else {
                eprintln!("{} — {}", error.code, error.message);
            }
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<Output, CliError> {
    if let Command::DockerContexts { max_contexts } = &cli.command {
        let inventory = discover_contexts(*max_contexts);
        let (text, exit) = match &inventory.contexts {
            Some(names) => (names.join("\n"), 0),
            None => ("Docker 컨텍스트 목록 미확인".to_owned(), 5),
        };
        return Ok(Output {
            command: "docker-contexts",
            data: json!(inventory),
            text,
            exit,
        });
    }
    if let Command::DockerBuildCache { context } = &cli.command {
        let backend = DockerCli::new(context)
            .map_err(|_| CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요"))?;
        let inventory = build_cache_inventory(&backend);
        let (text, exit) = match (inventory.total_count, inventory.active_count) {
            (Some(total), Some(active)) => (
                format!("빌드 캐시 {total}개 · 활성 {active}개 · 소유권 미확인으로 보존"),
                0,
            ),
            _ => ("Docker 빌드 캐시 집계 미확인 · 보존".to_owned(), 5),
        };
        return Ok(Output {
            command: "docker-build-cache",
            data: json!(inventory),
            text,
            exit,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    if let Command::BuildkitLocalCache {
        socket,
        expected_pid,
        buildctl,
    } = &cli.command
    {
        let result = dsg_core::docker_buildkit_local::observe(socket, *expected_pid, buildctl);
        let (data, text, exit) = match result {
            Ok(usage) => {
                let text = format!(
                    "로컬 BuildKit 캐시 {}개 · 소유권 미확인으로 보존",
                    usage.total
                );
                (
                    json!({"status":"complete", "counts":usage, "disposition":"preserve", "reason":"unknown_ownership"}),
                    text,
                    0,
                )
            }
            Err(failure) => (
                json!({"status":"unknown", "counts":null, "disposition":"preserve", "reason":"incomplete_inventory", "failure":failure}),
                "로컬 BuildKit 캐시 집계 미확인 · 보존".to_owned(),
                5,
            ),
        };
        return Ok(Output {
            command: "buildkit-local-cache",
            data,
            text,
            exit,
        });
    }
    if let Command::DockerBuildxCache { context } = &cli.command {
        let backend = DockerCli::new(context)
            .map_err(|_| CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요"))?;
        let inventory = buildx_cache_inventory(&backend);
        let (text, exit) = match (
            inventory.total_count,
            inventory.reclaimable_count,
            inventory.shared_count,
            inventory.mutable_count,
        ) {
            (Some(total), Some(reclaimable), Some(shared), Some(mutable)) => (
                format!("Buildx 캐시 {total}개 · 회수 가능 표시 {reclaimable}개 · 공유 {shared}개 · 변경 가능 {mutable}개 · 보존"),
                0,
            ),
            _ => ("Buildx 캐시 집계 미확인 · 보존".to_owned(), 5),
        };
        return Ok(Output {
            command: "docker-buildx-cache",
            data: json!(inventory),
            text,
            exit,
        });
    }
    let root = filesystem::canonical_root(&cli.root)
        .map_err(|_| CliError::new("PROJECT_UNKNOWN", "프로젝트 디렉터리에 접근할 수 없습니다"))?;
    if let Command::Agents { action } = &cli.command {
        return configure_agent(&root, action);
    }
    let settings = Policy::load(
        &root,
        cli.global_policy.as_deref(),
        cli.session_policy.as_deref(),
    )?;
    match &cli.command {
        Command::Doctor => {
            let report = read_scan(&root, &settings)?;
            let (status, label, percentage) = budget_status(&settings, &report);
            Ok(Output::success(
                "doctor",
                json!({"project_id": settings.project_id, "policy": settings, "disk_free_bytes": report.disk_free_bytes, "scan_complete": report.complete, "observed_at_unix": report.observed_at_unix, "budget_status": status, "project_usage_percent": percentage, "mode": "observe"}),
                format!(
                    "{} · 조회 모드\n예산 {label} · {}",
                    settings.project_id,
                    text_scan(&report)
                ),
            ))
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        Command::BuildkitLocalCache { .. } => unreachable!(),
        Command::Hook { .. }
        | Command::Agents { .. }
        | Command::Mcp
        | Command::McpStateRead { .. }
        | Command::DockerContexts { .. }
        | Command::DockerBuildCache { .. }
        | Command::DockerBuildxCache { .. }
        | Command::Dashboard { .. } => unreachable!(),
        Command::Scan => {
            let report = read_scan(&root, &settings)?;
            let (status, label, percentage) = budget_status(&settings, &report);
            Ok(Output::success(
                "scan",
                json!({"project_id": settings.project_id, "scan": report, "budget_status": status, "project_usage_percent": percentage}),
                format!("예산 {label} · {}", text_scan(&report)),
            ))
        }
        Command::Init { project_id } => {
            let label = project_id.as_deref().unwrap_or_else(|| {
                root.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("local-project")
            });
            let mut registry = open_registry(cli)?;
            policy::init_project(&root, label)?;
            let project = registry.register(&root, label, POLICY_VERSION).map_err(|_| {
                CliError::new("REGISTRY_UNAVAILABLE", "설정은 생성됐습니다. 상태 저장소를 확인한 뒤 정리 계획을 생성하면 등록을 다시 시도합니다")
            })?;
            Ok(Output::success(
                "init",
                json!({"project": project, "config": PROJECT_CONFIG}),
                format!("{} 등록 완료 · {}", label, PROJECT_CONFIG),
            ))
        }
        Command::Check { command } => {
            let mut verdict = evaluate_command(command);
            if verdict.decision == Decision::Warn {
                verdict = check_budget(&settings, &read_scan(&root, &settings)?).unwrap_or(verdict);
            }
            let mut registry = open_registry(cli)?;
            let project = registry.find_project(&root).map_err(registry_error)?;
            let outcome = match verdict.decision {
                Decision::Allow => AuditOutcome::Allow,
                Decision::Warn => AuditOutcome::Warn,
                Decision::Deny => AuditOutcome::Deny,
                Decision::ApprovalRequired => AuditOutcome::ApprovalRequired,
            };
            registry
                .record_event(
                    project.map(|item| item.id),
                    AuditAction::PolicyCheck,
                    outcome,
                    verdict.reason_code,
                )
                .map_err(registry_error)?;
            let exit = match verdict.decision {
                Decision::Deny => 3,
                Decision::ApprovalRequired => 4,
                _ => 0,
            };
            let text = format!(
                "{} · {}\n다음 단계 {}",
                verdict.decision.as_str(),
                verdict.message,
                verdict.suggested_action
            );
            Ok(Output {
                command: "check",
                data: json!(verdict),
                text,
                exit,
            })
        }
        Command::GcPlan {
            docker_context,
            docker_engine,
            docker_image,
        } => {
            let mut registry = open_registry(cli)?;
            let project = registry
                .register(&root, &settings.project_id, settings.version)
                .map_err(registry_error)?;
            let plan = filesystem::plan(&root, &settings)
                .map_err(|_| CliError::new("SCAN_FAILED", "정리 후보 확인에 실패했습니다"))?;
            let mut payload = json!(plan);
            if let (Some(context), Some(engine), Some(image)) =
                (docker_context, docker_engine, docker_image)
            {
                payload["docker_images"] = json!(docker_gc_cli::observe(
                    &registry, &root, context, engine, image
                )?);
            }
            let policy_digest = policy_digest(&settings)?;
            let record = registry
                .save_plan(
                    project.id,
                    &policy_digest,
                    &payload,
                    Duration::from_secs(3600),
                )
                .map_err(registry_error)?;
            let mut text = format!(
                "정리 미리보기 #{} · 후보 {}개 · 제외 {}개",
                record.plan_id,
                plan.candidates.len(),
                plan.excluded.len()
            );
            for candidate in &plan.candidates {
                text.push_str(&format!(
                    "\n- {} · {} B",
                    candidate.relative_path, candidate.bytes
                ));
            }
            Ok(Output::success(
                "gc-plan",
                json!({"record": record, "plan": payload}),
                text,
            ))
        }
        Command::DockerImageRecheck { plan } => {
            docker_gc_cli::recheck(cli, &root, &settings, *plan)
        }
        Command::DockerScan {
            context,
            max_resources,
        } => {
            let backend = DockerCli::new(context).map_err(|_| {
                CliError::new("CONFIG_INVALID", "Docker 컨텍스트 이름을 확인하세요")
            })?;
            let inventory = docker::inventory(&backend, usize::from(*max_resources));
            let state = if inventory.complete {
                "측정 완료"
            } else {
                "일부 또는 전체 미확인"
            };
            let text = format!(
                "Docker {context} · {state}\n컨테이너 {}개 · 이미지 {}개 · 볼륨 {}개 · 중복 후보 {}묶음",
                inventory.containers.len(),
                inventory.images.len(),
                inventory.volumes.len(),
                inventory.duplicates.len(),
            );
            let exit = if inventory.engine_id.is_some() { 0 } else { 5 };
            Ok(Output {
                command: "docker-scan",
                data: json!(inventory),
                text,
                exit,
            })
        }
        Command::Exec {
            timeout,
            auto_clean_cargo,
            provider,
            trace_ttl_seconds,
            command,
        } => execution::exec(
            cli,
            &root,
            &settings,
            *timeout,
            *auto_clean_cargo,
            provider.as_deref(),
            *trace_ttl_seconds,
            command,
        ),
        Command::DockerReuse {
            context,
            expectation_json,
            compose_projection_stdin,
        } => execution::reuse(
            cli,
            &root,
            &settings,
            context,
            expectation_json,
            *compose_projection_stdin,
        ),
        Command::GcOwn {
            candidate,
            confirm_generated: true,
        } => cleanup_cli::own(cli, &root, &settings, candidate),
        Command::GcApprove {
            plan,
            candidate,
            confirm: true,
        } => cleanup_cli::approve(cli, &root, &settings, *plan, candidate),
        Command::GcApply {
            plan,
            approval_stdin: true,
        } => cleanup_cli::apply(cli, &root, &settings, *plan, false),
        Command::GcRecover { plan } => cleanup_cli::apply(cli, &root, &settings, *plan, true),
        Command::WorktreeCreate {
            branch,
            destination,
        } => cleanup_cli::create_worktree(cli, &root, branch, destination),
        Command::CleanupGit { feature } => cleanup_cli::complete_git(cli, &root, feature),
        Command::GcOwn { .. } | Command::GcApprove { .. } | Command::GcApply { .. } => Err(
            CliError::new("NEEDS_APPROVAL", "지정한 정리 요청을 명시적으로 승인하세요"),
        ),
    }
}

fn configure_agent(root: &Path, action: &AgentAction) -> Result<Output, CliError> {
    let (agent, install) = match action {
        AgentAction::Install { agent } => (*agent, true),
        AgentAction::Uninstall { agent } => (*agent, false),
    };
    let config_error = || {
        CliError::new(
            "AGENT_CONFIG_UNAVAILABLE",
            "프로젝트 훅 설정 파일과 실행 경로를 확인하세요",
        )
    };
    if std::env::var_os("HOME")
        .and_then(|home| std::fs::canonicalize(home).ok())
        .is_some_and(|home| home == root)
    {
        return Err(config_error());
    }
    let executable = std::env::current_exe().map_err(|_| config_error())?;
    let changed =
        hook_install::configure(agent, root, &executable, install).map_err(|_| config_error())?;
    let text = if changed {
        "프로젝트 훅 설정을 갱신했습니다"
    } else {
        "프로젝트 훅 설정 변경이 없습니다"
    };
    Ok(Output::success(
        "agents",
        json!({"changed": changed, "installed": install}),
        text.to_owned(),
    ))
}

fn open_registry(cli: &Cli) -> Result<Registry, CliError> {
    Registry::open(&local_state_dir(cli)?).map_err(registry_error)
}

fn local_state_dir(cli: &Cli) -> Result<PathBuf, CliError> {
    cli.state_dir
        .clone()
        .or_else(|| std::env::var_os("DSG_STATE_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os(if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "HOME"
            })
            .map(|home| PathBuf::from(home).join(".dsg"))
        })
        .ok_or_else(|| {
            CliError::new(
                "REGISTRY_UNAVAILABLE",
                "--state-dir로 로컬 상태 위치를 지정하세요",
            )
        })
}

fn registry_error(_: dsg_core::registry::RegistryError) -> CliError {
    CliError::new("REGISTRY_UNAVAILABLE", "로컬 상태 저장소를 확인하세요")
}

fn policy_digest(policy: &Policy) -> Result<String, CliError> {
    let bytes = serde_json::to_vec(policy)
        .map_err(|_| CliError::new("CONFIG_INVALID", "정책 스냅샷을 만들 수 없습니다"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn read_scan(root: &Path, settings: &Policy) -> Result<ScanReport, CliError> {
    filesystem::scan(root, settings)
        .map_err(|_| CliError::new("SCAN_FAILED", "프로젝트 사용량을 확인할 수 없습니다"))
}

fn text_scan(report: &ScanReport) -> String {
    let free = report
        .disk_free_bytes
        .map(|bytes| format!("{bytes} B"))
        .unwrap_or_else(|| "미확인".to_owned());
    let state = if report.complete {
        "측정 완료"
    } else {
        "일부 미확인"
    };
    let mut text = format!(
        "{state} · 관리 경로 사용량 {} B · 호스트 여유 {free}",
        report.total_bytes
    );
    for entry in &report.entries {
        let quality = if entry.complete {
            ""
        } else {
            " · 일부 미확인"
        };
        text.push_str(&format!("\n- {} · {} B{quality}", entry.path, entry.bytes));
    }
    text
}

fn check_budget(settings: &Policy, report: &ScanReport) -> Option<Verdict> {
    if report
        .disk_free_bytes
        .is_some_and(|bytes| bytes < settings.host_min_free_gb.saturating_mul(1 << 30))
        || report.total_bytes > settings.project_budget_gb.saturating_mul(1 << 30)
    {
        return Some(Verdict::new(
            Decision::Deny,
            "INSUFFICIENT_SPACE",
            "호스트 여유 또는 프로젝트 예산이 부족합니다",
            "scan",
        ));
    }
    if report.disk_free_bytes.is_none() || !report.complete {
        return Some(Verdict::new(
            Decision::ApprovalRequired,
            "MEASUREMENT_INCOMPLETE",
            "실행 전 디스크 예산을 확인할 수 없습니다",
            "scan",
        ));
    }
    None
}

fn budget_status(
    settings: &Policy,
    report: &ScanReport,
) -> (&'static str, &'static str, Option<u64>) {
    let budget = (settings.project_budget_gb as u128) << 30;
    let percentage = if report.complete && budget != 0 {
        u64::try_from(u128::from(report.total_bytes) * 100 / budget).ok()
    } else {
        None
    };
    if report
        .disk_free_bytes
        .is_some_and(|bytes| bytes < settings.host_min_free_gb.saturating_mul(1 << 30))
        || u128::from(report.total_bytes) > budget
        || percentage.is_some_and(|value| value >= u64::from(settings.critical_percent))
    {
        return ("critical", "위험", percentage);
    }
    if !report.complete || report.disk_free_bytes.is_none() {
        return ("unknown", "미확인", None);
    }
    if percentage.is_some_and(|value| value >= u64::from(settings.warning_percent)) {
        return ("warning", "주의", percentage);
    }
    ("normal", "정상", percentage)
}
