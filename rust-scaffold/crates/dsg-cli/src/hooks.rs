use std::fmt;
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::process::ExitCode;

use dsg_core::{evaluate_command, Decision};
use serde::de::{value::MapAccessDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const INVALID_INPUT: &str = "HOOK_INPUT_INVALID";
const UNSUPPORTED_EVENT: &str = "HOOK_EVENT_UNSUPPORTED";
const UNKNOWN_COMMAND: &str = "HOOK_COMMAND_UNKNOWN";
const OUTPUT_FAILURE: &str = "HOOK_OUTPUT_FAILED";

#[derive(Clone, Copy, clap::ValueEnum)]
pub(super) enum Agent {
    Claude,
    Codex,
    Gemini,
}

#[derive(Deserialize)]
struct HookInput {
    hook_event_name: String,
    tool_name: String,
    #[serde(deserialize_with = "deserialize_object")]
    tool_input: ToolInput,
}

#[derive(Deserialize)]
struct ToolInput {
    command: String,
}

#[derive(Deserialize)]
struct HookObject(#[serde(deserialize_with = "deserialize_object")] HookInput);

// Serde structs also accept positional arrays. Hook requests require JSON objects.
fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Object<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a hook input object")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Object(PhantomData))
}

pub(super) fn run(agent: Agent) -> ExitCode {
    let response = respond(agent, io::stdin().lock());
    let mut output = io::stdout().lock();
    if serde_json::to_writer(&mut output, &response).is_ok()
        && output.write_all(b"\n").is_ok()
        && output.flush().is_ok()
    {
        ExitCode::SUCCESS
    } else {
        let _ = writeln!(io::stderr().lock(), "{OUTPUT_FAILURE}");
        ExitCode::from(2)
    }
}

fn respond(agent: Agent, input: impl Read) -> Value {
    let mut payload = Vec::new();
    if input
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut payload)
        .is_err()
        || payload.len() > MAX_INPUT_BYTES
    {
        return deny(agent, INVALID_INPUT);
    }

    match assess(agent, &payload) {
        Ok(()) => json!({}),
        Err(reason) => deny(agent, reason),
    }
}

fn assess(agent: Agent, payload: &[u8]) -> Result<(), &'static str> {
    let HookObject(input) = serde_json::from_slice(payload).map_err(|_| INVALID_INPUT)?;
    let expected = match agent {
        Agent::Claude | Agent::Codex => ("PreToolUse", "Bash"),
        Agent::Gemini => ("BeforeTool", "run_shell_command"),
    };
    if (input.hook_event_name.as_str(), input.tool_name.as_str()) != expected {
        return Err(UNSUPPORTED_EVENT);
    }

    let argv = literal_argv(&input.tool_input.command).ok_or(UNKNOWN_COMMAND)?;
    let verdict = evaluate_command(&argv);
    match verdict.decision {
        Decision::Allow | Decision::Warn => Ok(()),
        Decision::Deny | Decision::ApprovalRequired => Err(verdict.reason_code),
    }
}

fn literal_argv(command: &str) -> Option<Vec<String>> {
    // shlex splits words. Check separately for shell syntax that would change those words
    // or cause execution of an additional command.
    let mut quote = None;
    let mut bytes = command.bytes();
    while let Some(byte) = bytes.next() {
        if shell_control_byte(byte) {
            return None;
        }
        match (quote, byte) {
            (Some(open), close) if open == close => quote = None,
            (Some(b'\''), _) => {}
            (_, b'\\') => {
                if shell_control_byte(bytes.next()?) {
                    return None;
                }
            }
            (_, b'$' | b'`' | b'!') => return None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' | b'#') => return None,
            (None, b'{' | b'}' | b'[' | b']' | b'*' | b'?' | b'~') => return None,
            _ => {}
        }
    }

    let argv = shlex::split(command)?;
    let program = argv.first()?;
    let executable = program.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let executable = executable.strip_suffix(".exe").unwrap_or(&executable);
    if executable.is_empty()
        || program.contains('=')
        || matches!(
            executable,
            "." | "builtin"
                | "command"
                | "doas"
                | "env"
                | "eval"
                | "exec"
                | "nohup"
                | "source"
                | "sudo"
                | "time"
                | "xargs"
        )
    {
        return None;
    }
    Some(argv)
}

fn shell_control_byte(byte: u8) -> bool {
    byte.is_ascii_control() && byte != b'\t'
}

fn deny(agent: Agent, reason: &'static str) -> Value {
    match agent {
        Agent::Claude | Agent::Codex => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        }),
        Agent::Gemini => json!({"decision": "deny", "reason": reason}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(agent: Agent, command: &str) -> Value {
        let (event, tool) = match agent {
            Agent::Claude | Agent::Codex => ("PreToolUse", "Bash"),
            Agent::Gemini => ("BeforeTool", "run_shell_command"),
        };
        json!({
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": {"command": command, "other": true},
            "transport_metadata": "ignored"
        })
    }

    fn command_response(agent: Agent, command: &str) -> Value {
        respond(agent, input(agent, command).to_string().as_bytes())
    }

    #[test]
    fn uses_agent_specific_denial_shapes() {
        for agent in [Agent::Claude, Agent::Codex] {
            assert_eq!(
                command_response(agent, "docker volume prune"),
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": "PROTECTED_VOLUME"
                    }
                })
            );
        }
        assert_eq!(
            command_response(Agent::Gemini, "docker volume prune"),
            json!({"decision": "deny", "reason": "PROTECTED_VOLUME"})
        );
    }

    #[test]
    fn preserves_agent_permissions_on_allow_and_warn() {
        for agent in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            for command in ["git status --short", "docker build .", "printf '%s' 'a;b'"] {
                assert_eq!(command_response(agent, command), json!({}));
            }
        }
    }

    #[test]
    fn passes_literal_arguments_to_core_policy() {
        assert_eq!(
            command_response(Agent::Claude, "docker 'vol'ume prune"),
            deny(Agent::Claude, "PROTECTED_VOLUME")
        );
        assert_eq!(
            command_response(Agent::Codex, "docker system prune"),
            deny(Agent::Codex, "NEEDS_APPROVAL")
        );
        assert_eq!(
            command_response(Agent::Gemini, "'rm' -r 'fixture dir'"),
            deny(Agent::Gemini, "RECURSIVE_DELETE")
        );
    }

    #[test]
    fn declines_commands_outside_the_literal_subset() {
        for command in [
            "",
            "'' echo",
            "echo 'unterminated",
            "echo a\ntrue",
            "echo a; true",
            "echo a && true",
            "echo a | true",
            "echo a > fixture",
            "echo \"$(true)\"",
            "echo *",
            "echo a # fixture",
            "FLAG=fixture true",
            "env true",
            "command true",
        ] {
            assert_eq!(
                command_response(Agent::Gemini, command),
                deny(Agent::Gemini, UNKNOWN_COMMAND)
            );
        }
        assert_eq!(
            command_response(Agent::Gemini, "sh -c 'true'"),
            deny(Agent::Gemini, "COMMAND_UNKNOWN")
        );
    }

    #[test]
    fn declines_unexpected_event_or_tool_name() {
        let mut request = input(Agent::Codex, "git status");
        request["tool_name"] = json!("exec_command");
        assert_eq!(
            respond(Agent::Codex, request.to_string().as_bytes()),
            deny(Agent::Codex, UNSUPPORTED_EVENT)
        );
        let request = input(Agent::Gemini, "git status");
        assert_eq!(
            respond(Agent::Claude, request.to_string().as_bytes()),
            deny(Agent::Claude, UNSUPPORTED_EVENT)
        );
    }

    #[test]
    fn denies_invalid_or_oversized_input_without_echo() {
        let request = input(Agent::Gemini, "true").to_string();
        let mut payload = request.as_bytes().to_vec();
        payload.resize(MAX_INPUT_BYTES, b' ');
        assert_eq!(respond(Agent::Gemini, payload.as_slice()), json!({}));
        payload.push(b' ');
        let expected = deny(Agent::Gemini, INVALID_INPUT);
        assert_eq!(respond(Agent::Gemini, payload.as_slice()), expected);
        let invalid: &[&[u8]] = &[
            b"{\"fixture_marker\":",
            b"\xff",
            b"{}",
            br#"["BeforeTool","run_shell_command",{"command":"true"}]"#,
            br#"{"hook_event_name":"BeforeTool","tool_name":"run_shell_command","tool_input":["true"]}"#,
            br#"{"hook_event_name":"BeforeTool","tool_name":"run_shell_command","tool_input":{"command":[]}}"#,
            br#"{"hook_event_name":"BeforeTool","tool_name":"run_shell_command","tool_input":{"command":"true","command":"false"}}"#,
        ];
        for payload in invalid {
            assert_eq!(respond(Agent::Gemini, *payload), expected);
        }
    }
}
