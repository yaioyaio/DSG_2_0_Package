use super::{budget_status, check_budget, mcp_state::Reader, read_scan, Cli};
use dsg_core::{
    docker, docker_command::DockerCli, evaluate_command, filesystem, policy::Policy, Decision,
};
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = "2025-06-18";
const MAX_FRAME: usize = 64 * 1024;
const MAX_ARGC: usize = 128;
const MAX_ARG_BYTES: usize = 4096;
const MCP_DOCKER_RESOURCE_LIMIT: usize = 8;
const DASHBOARD_DOCKER_RESOURCE_LIMIT: usize = 64;
const EXECUTION_STATUS: &str = "dsg_get_execution_status";
const APPROVAL_STATUS: &str = "dsg_get_gc_approval_status";
const TOOLS: &[(&str, &str)] = &[
    ("dsg_get_health", "프로젝트 예산과 측정 상태를 조회합니다."),
    ("dsg_scan_project", "관리 경로의 사용량 합계를 조회합니다."),
    (
        "dsg_inspect_docker",
        "default Docker 컨텍스트를 제한적으로 조회합니다.",
    ),
    (
        "dsg_check_command",
        "실행 없이 인수 배열의 정책 판정을 확인합니다.",
    ),
    (
        "dsg_get_gc_plan",
        "계획을 저장하지 않고 정리 후보의 개수와 용량을 조회합니다.",
    ),
    (
        "dsg_get_policy",
        "세션 시작 시 적용된 정책 한도를 조회합니다.",
    ),
    (
        EXECUTION_STATUS,
        "이 프로젝트에 기록된 관리형 실행 상태를 조회합니다.",
    ),
    (
        APPROVAL_STATUS,
        "이 프로젝트의 정리 계획에 대해 별도로 발행된 승인 기록의 상태를 조회합니다.",
    ),
];

#[derive(PartialEq, Eq)]
enum Phase {
    New,
    Negotiated,
    Ready,
}

struct Server {
    project: Option<(PathBuf, Policy)>,
    state_reader: Option<Reader>,
    phase: Phase,
}

pub(super) fn run(cli: &Cli) -> ExitCode {
    let project = filesystem::canonical_root(&cli.root).ok().and_then(|root| {
        Policy::load(
            &root,
            cli.global_policy.as_deref(),
            cli.session_policy.as_deref(),
        )
        .ok()
        .map(|policy| (root, policy))
    });
    let server = Server {
        project,
        state_reader: Reader::new(cli),
        phase: Phase::New,
    };
    match serve(server, io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn serve(mut server: Server, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes = (&mut input)
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if bytes == 0 {
            return Ok(());
        }
        let response = if bytes > MAX_FRAME {
            Some(error(&Value::Null, -32600, "Message too large"))
        } else {
            match serde_json::from_slice(&line) {
                Ok(message) => server.handle(&message),
                Err(_) => Some(error(&Value::Null, -32700, "Parse error")),
            }
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
        if bytes > MAX_FRAME {
            return Ok(());
        }
    }
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

impl Server {
    fn handle(&mut self, message: &Value) -> Option<Value> {
        let method = message["method"].as_str();
        if message["jsonrpc"] != "2.0" || method.is_none() {
            return Some(error(&Value::Null, -32600, "Invalid request"));
        }
        let params = &message["params"];
        let Some(id) = message.get("id") else {
            if method == Some("notifications/initialized")
                && self.phase == Phase::Negotiated
                && (message.get("params").is_none() || params.is_object())
            {
                self.phase = Phase::Ready;
            }
            return None;
        };
        if !(id.is_i64() || id.is_u64() || id.is_string()) {
            return Some(error(&Value::Null, -32600, "Invalid request"));
        }
        if message.get("params").is_some() && !params.is_object() {
            return Some(error(id, -32602, "Invalid parameters"));
        }
        let result = match method? {
            "ping" => json!({}),
            "initialize" if self.phase == Phase::New => {
                if !params["protocolVersion"].is_string()
                    || !params["capabilities"].is_object()
                    || !params["clientInfo"]["name"].is_string()
                    || !params["clientInfo"]["version"].is_string()
                {
                    return Some(error(id, -32602, "Invalid initialization parameters"));
                }
                if self.project.is_none() {
                    return Some(error(id, -32001, "Project policy unavailable"));
                }
                self.phase = Phase::Negotiated;
                json!({"protocolVersion":VERSION,"capabilities":{"tools":{}},
                    "serverInfo":{"name":"dsg","version":env!("CARGO_PKG_VERSION")}})
            }
            "initialize" => return Some(error(id, -32600, "Already initialized")),
            _ if self.phase != Phase::Ready => return Some(error(id, -32002, "Not initialized")),
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return Some(error(id, -32602, "Invalid cursor"));
                }
                json!({"tools": TOOLS.iter().map(|(name, description)| tool_spec(name, description)).collect::<Vec<_>>()})
            }
            "tools/call" => {
                let Some((root, policy)) = &self.project else {
                    return Some(error(id, -32001, "Project policy unavailable"));
                };
                let observation = if matches!(
                    params["name"].as_str(),
                    Some(EXECUTION_STATUS | APPROVAL_STATUS)
                ) {
                    call_state(self.state_reader.as_ref(), root, params)
                } else {
                    call(root, policy, params)
                };
                match observation {
                    Ok(data) => {
                        json!({"content":[{"type":"text","text":data.to_string()}],"isError":false})
                    }
                    Err(CallError::Invalid) => {
                        return Some(error(id, -32602, "Invalid tool or arguments"))
                    }
                    Err(CallError::Unavailable) => {
                        json!({"content":[{"type":"text","text":"Observation unavailable"}],"isError":true})
                    }
                }
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

fn tool_spec(name: &str, description: &str) -> Value {
    let mut schema = json!({"type":"object","properties":{},"additionalProperties":false});
    if name == "dsg_check_command" {
        schema["properties"]["argv"] = json!({"type":"array","minItems":1,"maxItems":MAX_ARGC,
            "items":{"type":"string","maxLength":MAX_ARG_BYTES}});
        schema["required"] = json!(["argv"]);
    } else if name == APPROVAL_STATUS {
        schema["properties"]["plan_id"] = json!({"type":"integer","minimum":1,"maximum":i64::MAX});
        schema["required"] = json!(["plan_id"]);
    }
    json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":true}})
}

enum CallError {
    Invalid,
    Unavailable,
}

fn call_state(reader: Option<&Reader>, root: &Path, params: &Value) -> Result<Value, CallError> {
    let plan_id = match params["name"].as_str() {
        Some(EXECUTION_STATUS) => match params.get("arguments") {
            None => None,
            Some(args) if args.as_object().is_some_and(serde_json::Map::is_empty) => None,
            _ => return Err(CallError::Invalid),
        },
        Some(APPROVAL_STATUS) => {
            let args = params["arguments"].as_object().ok_or(CallError::Invalid)?;
            if args.len() != 1 {
                return Err(CallError::Invalid);
            }
            Some(
                args.get("plan_id")
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0)
                    .ok_or(CallError::Invalid)?,
            )
        }
        _ => return Err(CallError::Invalid),
    };
    let observation = reader
        .and_then(|reader| reader.observe(root, plan_id))
        .ok_or(CallError::Unavailable)?;
    serde_json::to_value(observation).map_err(|_| CallError::Unavailable)
}

fn call(root: &Path, policy: &Policy, params: &Value) -> Result<Value, CallError> {
    let name = params["name"].as_str().ok_or(CallError::Invalid)?;
    let empty = serde_json::Map::new();
    let args = match params.get("arguments") {
        None => &empty,
        Some(value) => value.as_object().ok_or(CallError::Invalid)?,
    };
    if name == "dsg_check_command" {
        let argv = args
            .get("argv")
            .and_then(Value::as_array)
            .ok_or(CallError::Invalid)?;
        if args.len() != 1 || argv.is_empty() || argv.len() > MAX_ARGC {
            return Err(CallError::Invalid);
        }
        let argv: Vec<&str> = argv
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.len() <= MAX_ARG_BYTES && !s.contains('\0'))
                    .ok_or(CallError::Invalid)
            })
            .collect::<Result<_, _>>()?;
        if argv[0].trim().is_empty() {
            return Err(CallError::Invalid);
        }
        let mut verdict = evaluate_command(&argv);
        if verdict.decision == Decision::Warn {
            let report = read_scan(root, policy).map_err(|_| CallError::Unavailable)?;
            verdict = check_budget(policy, &report).unwrap_or(verdict);
        }
        return Ok(json!(verdict));
    }
    if !args.is_empty() {
        return Err(CallError::Invalid);
    }
    match name {
        "dsg_get_health" | "dsg_scan_project" => {
            let scan = read_scan(root, policy).map_err(|_| CallError::Unavailable)?;
            let (status, _, percent) = budget_status(policy, &scan);
            Ok(
                json!({"complete":scan.complete,"total_bytes":scan.total_bytes,
                "disk_free_bytes":scan.disk_free_bytes,"budget_status":status,"project_usage_percent":percent,
                "observed_at_unix":scan.observed_at_unix}),
            )
        }
        "dsg_get_gc_plan" => {
            let plan = filesystem::plan(root, policy).map_err(|_| CallError::Unavailable)?;
            let bytes = plan
                .candidates
                .iter()
                .fold(0u64, |n, c| n.saturating_add(c.bytes));
            Ok(
                json!({"dry_run":plan.dry_run,"candidate_count":plan.candidates.len(),
                "candidate_bytes":bytes,"excluded_count":plan.excluded.len()}),
            )
        }
        "dsg_get_policy" => Ok(
            json!({"version":policy.version,"project_budget_gb":policy.project_budget_gb,
            "host_min_free_gb":policy.host_min_free_gb,"warning_percent":policy.warning_percent,
            "critical_percent":policy.critical_percent,"max_scan_files":policy.max_scan_files,
            "max_candidates":policy.max_candidates,"gc_min_age_hours":policy.gc_min_age_hours}),
        ),
        "dsg_inspect_docker" => docker_summary("default", MCP_DOCKER_RESOURCE_LIMIT),
        _ => Err(CallError::Invalid),
    }
}

fn docker_summary(context: &str, max_resources: usize) -> Result<Value, CallError> {
    let backend = DockerCli::new(context).map_err(|_| CallError::Unavailable)?;
    let report = docker::inventory(&backend, max_resources);
    Ok(json!({"status":report.status,"complete":report.complete,
        "container_count":report.containers.len(), "image_count":report.images.len(),
        "volume_count":report.volumes.len(),"duplicate_groups":report.duplicates.len()}))
}

pub(super) fn overview(root: &Path, policy: &Policy, docker_context: Option<&str>) -> Value {
    let observe = |name| call(root, policy, &json!({"name":name})).ok();
    json!({
        "health": observe("dsg_get_health"),
        "policy": observe("dsg_get_policy"),
        "cleanup": observe("dsg_get_gc_plan"),
        "docker": docker_context.and_then(|context| {
            docker_summary(context, DASHBOARD_DOCKER_RESOURCE_LIMIT).ok()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_requires_handshake_and_exposes_only_scoped_tools() {
        let project = tempfile::tempdir().unwrap();
        let mut server = Server {
            project: Some((project.path().into(), Policy::default())),
            state_reader: None,
            phase: Phase::New,
        };
        let request = |method: &str, params: Value| json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        assert!(server.handle(&request("tools/list", json!({}))).unwrap()["error"].is_object());
        let init = request(
            "initialize",
            json!({"protocolVersion":"different","clientInfo":{"name":"test","version":"1"},"capabilities":{}}),
        );
        assert_eq!(
            server.handle(&init).unwrap()["result"]["protocolVersion"],
            VERSION
        );
        assert!(server.handle(&request("tools/list", json!({}))).unwrap()["error"].is_object());
        assert!(server
            .handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .is_none());
        let list = server.handle(&request("tools/list", json!({}))).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool["name"] == APPROVAL_STATUS)
                .unwrap()["inputSchema"]["required"],
            json!(["plan_id"])
        );
        assert!(tools
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true));
        for params in [
            json!({"name":"dsg_gc_apply"}),
            json!({"name":"dsg_gc_approve"}),
            json!({"name":"dsg_gc_recover"}),
            json!({"name":"dsg_exec"}),
            json!({"name":"dsg_scan_project","arguments":{"root":"/"}}),
            json!({"name":"dsg_check_command","arguments":{"argv":[1]}}),
            json!({"name":EXECUTION_STATUS,"arguments":{"project_id":2}}),
            json!({"name":APPROVAL_STATUS,"arguments":{"plan_id":0}}),
            json!({"name":APPROVAL_STATUS,"arguments":{"plan_id":3,"approval":"fixture"}}),
        ] {
            assert_eq!(
                server.handle(&request("tools/call", params)).unwrap()["error"]["code"],
                -32602
            );
        }
        for params in [
            json!({"name":EXECUTION_STATUS}),
            json!({"name":APPROVAL_STATUS,"arguments":{"plan_id":3}}),
        ] {
            let response = server.handle(&request("tools/call", params)).unwrap();
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(
                response["result"]["content"][0]["text"],
                "Observation unavailable"
            );
        }
        for tool in [
            "dsg_get_health",
            "dsg_scan_project",
            "dsg_get_gc_plan",
            "dsg_get_policy",
            "dsg_check_command",
        ] {
            let arguments = if tool == "dsg_check_command" {
                json!({"argv":["git","status"]})
            } else {
                json!({})
            };
            let res = server
                .handle(&request(
                    "tools/call",
                    json!({"name":tool,"arguments":arguments}),
                ))
                .unwrap();
            assert_eq!(res["result"]["isError"], false);
        }
        assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 0);
    }

    #[test]
    fn oversized_frame_ends_session_without_echoing_input() {
        let mut output = Vec::new();
        serve(
            Server {
                project: None,
                state_reader: None,
                phase: Phase::New,
            },
            vec![b'x'; MAX_FRAME + 2].as_slice(),
            &mut output,
        )
        .unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert!(output.len() < 256);
    }
}
