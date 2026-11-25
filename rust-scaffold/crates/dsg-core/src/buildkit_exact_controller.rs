//! One socket-activated request inside the registered offline worker's keeper namespaces.
use crate::docker::QueryFailure;
use crate::docker_control_local::{
    admission::{
        bounded_read, compare_metadata, executable_hash, readable, valid_hash,
        write_request::{check_seqpacket, receive_account_packet},
    },
    check_alive, io_failure, open_protected,
};
mod operating;
pub use operating::serve_operating;

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::{fs::MetadataExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, QueryFailure>;

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    project: String,
    worker: String,
    record_id: String,
    generation: String,
    origin: String,
    metadata_digest: String,
    approval: String,
    intent: String,
    custody_sha256: String,
}

impl Scope {
    fn valid(&self) -> bool {
        [&self.worker, &self.record_id].iter().all(|id| {
            id.len() == 25
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        }) && [
            &self.project,
            &self.generation,
            &self.origin,
            &self.metadata_digest,
            &self.approval,
            &self.intent,
            &self.custody_sha256,
        ]
        .iter()
        .all(|hash| valid_hash(hash))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Approve,
    Apply,
    Receipt,
}

impl Operation {
    fn argument(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Apply => "apply",
            Self::Receipt => "receipt",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    operation: Operation,
    scope: Scope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u8,
    approver_uid: u32,
    executor_uid: u32,
    runtime_sha256: String,
    scope: Scope,
}

impl Policy {
    fn authorize(&self, uid: u32, request: &Request) -> Result<()> {
        let role = match request.operation {
            Operation::Approve => self.approver_uid,
            Operation::Apply | Operation::Receipt => self.executor_uid,
        };
        if self.version != 1
            || request.version != 1
            || self.approver_uid == 0
            || self.executor_uid == 0
            || self.approver_uid == self.executor_uid
            || uid != role
            || self.scope != request.scope
            || !self.scope.valid()
            || !valid_hash(&self.runtime_sha256)
        {
            return Err(QueryFailure::InvalidSelector);
        }
        Ok(())
    }
}

fn stage(registration: &str) -> Result<PathBuf> {
    if !(12..=64).contains(&registration.len())
        || !registration
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(PathBuf::from(format!(
        "/run/dsg-approval-{registration}/stage"
    )))
}

fn protected_file(path: &Path, mode: u32) -> Result<File> {
    let file = open_protected(path, true)?;
    let info = file.metadata().map_err(io_failure)?;
    if !info.is_file() || info.uid() != 0 || info.nlink() != 1 || info.mode() & 0o7777 != mode {
        return Err(QueryFailure::InvalidSelector);
    }
    Ok(file)
}

fn parse_envelope<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    // Serde also accepts positional arrays for structs. This protocol requires named
    // objects, while the typed second pass additionally rejects duplicate keys.
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| QueryFailure::Malformed)?;
    if !json.is_object() || !json.get("scope").is_some_and(serde_json::Value::is_object) {
        return Err(QueryFailure::Malformed);
    }
    serde_json::from_slice(bytes).map_err(|_| QueryFailure::Malformed)
}

/// Handle one accepted AF_UNIX SOCK_SEQPACKET connection supplied on stdin.
/// A local admission rejection returns a fixed response on the socket. On success
/// the hash-pinned native process writes its own response and supplies the exit status.
pub fn serve(registration: &str) -> u8 {
    let _ = dispatch(registration);
    if check_seqpacket(libc::STDIN_FILENO).is_ok() {
        let reply = b"{\"status\":\"preserved\",\"reason\":\"controller_admission\"}\n";
        unsafe {
            libc::send(
                libc::STDIN_FILENO,
                reply.as_ptr().cast(),
                reply.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            );
        }
    }
    5
}

fn dispatch(registration: &str) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 || unsafe { libc::getuid() } != 0 {
        return Err(QueryFailure::InvalidSelector);
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let stage = stage(registration)?;
    let mut payload = [0u8; 4096];
    let (len, uid, sender) = receive_account_packet(libc::STDIN_FILENO, &mut payload, deadline)?;
    let request: Request = parse_envelope(&payload[..len])?;

    let path = stage.join("controller-policy.json");
    let descriptor = protected_file(&path, 0o400)?;
    let identity = descriptor.metadata().map_err(io_failure)?;
    let bytes = bounded_read(readable(&descriptor)?, 4096)?;
    let policy: Policy = parse_envelope(&bytes)?;
    policy.authorize(uid, &request)?;

    let executable = protected_file(&stage.join("dsg-buildkit-exact"), 0o500)?;
    let info = executable.metadata().map_err(io_failure)?;
    if executable_hash(&executable, &info, deadline)? != policy.runtime_sha256 {
        return Err(QueryFailure::InvalidSelector);
    }
    // Only the registered selectors enter the native operator, on a separate private pipe.
    // Its output goes directly to this authenticated connection, including read-only receipts.
    let (input, mut writer) = io::pipe().map_err(io_failure)?;
    let body = serde_json::to_vec(&policy.scope).map_err(|_| QueryFailure::Malformed)?;
    writer.write_all(&body).map_err(io_failure)?;
    drop(writer);
    let reply = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map_err(io_failure)?;
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .arg0("dsg-buildkit-exact")
        .args([request.operation.argument(), registration])
        .env_clear()
        .current_dir("/")
        .stdin(input)
        .stdout(reply)
        .stderr(Stdio::null());
    // Keep all launcher descriptors private. The selected O_PATH inode stays available
    // through pathname resolution and is closed by the successful exec.
    unsafe {
        command.pre_exec(|| {
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    compare_metadata(
        &identity,
        &protected_file(&path, 0o400)?
            .metadata()
            .map_err(io_failure)?,
    )?;
    compare_metadata(&info, &executable.metadata().map_err(io_failure)?)?;
    check_alive(&sender)?;
    Err(io_failure(command.exec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_must_hold_the_registered_role_and_confirm_every_selector() {
        let hash = "a".repeat(64);
        let scope = json!({"project":hash,"worker":"w".repeat(25),"record_id":"r".repeat(25),
            "generation":hash,"origin":hash,"metadata_digest":hash,"approval":hash,"intent":hash,"custody_sha256":hash});
        let policy: Policy =
            serde_json::from_value(json!({"version":1,"approver_uid":101,"executor_uid":102,
            "scope":scope,"runtime_sha256":hash}))
            .unwrap();
        for (op, allowed, denied) in [
            ("approve", 101, 102),
            ("apply", 102, 101),
            ("receipt", 102, 101),
        ] {
            let packet = json!({"version":1,"operation":op,"scope":scope});
            let bytes = serde_json::to_vec(&packet).unwrap();
            let request = parse_envelope(&bytes).unwrap();
            assert!(policy.authorize(allowed, &request).is_ok());
            let positional = serde_json::to_vec(&json!([1, op, scope])).unwrap();
            assert!(parse_envelope::<Request>(&positional).is_err());
            let duplicate = String::from_utf8(bytes)
                .unwrap()
                .replacen('{', "{\"version\":1,", 1);
            assert!(parse_envelope::<Request>(duplicate.as_bytes()).is_err());
            let mut positional_scope = packet.clone();
            positional_scope["scope"] = json!([
                hash,
                "w".repeat(25),
                "r".repeat(25),
                hash,
                hash,
                hash,
                hash,
                hash,
                hash
            ]);
            assert!(
                parse_envelope::<Request>(&serde_json::to_vec(&positional_scope).unwrap()).is_err()
            );
            for uid in [0, denied, 103] {
                assert!(policy.authorize(uid, &request).is_err());
            }
            for name in scope.as_object().unwrap().keys() {
                let mut wrong = packet.clone();
                wrong["scope"][name] = json!("b".repeat(scope[name].as_str().unwrap().len()));
                assert!(policy
                    .authorize(allowed, &serde_json::from_value(wrong).unwrap())
                    .is_err());
            }
            let mut extended = packet;
            extended["uid"] = json!(allowed);
            assert!(serde_json::from_value::<Request>(extended).is_err());
        }
        for id in [
            "",
            "ABCDEF123456",
            "../123456789abc",
            "aaaaa",
            &"0".repeat(65),
        ] {
            assert!(stage(id).is_err());
        }
        assert!(stage("123456789abc").is_ok());
    }
}
