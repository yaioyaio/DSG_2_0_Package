//! Native image GC for the pinned, dedicated Moby engine.
use super::dispatch::{ClaimedDockerGc, ExclusiveDockerGc, ExclusiveEngine, PreparedDockerGc};
use super::{
    valid_owner_fingerprint, Binding, ControlledDockerGc, DockerApplyGate, DockerGcKind, Effect,
    Intent, Observation, Receipt, Usage,
};
use crate::docker_control_local::ControllerEvidence;
use crate::docker_image_gc::{ImageGcDisposition, ImageGcPlan, ImageGcReason};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const VERSION: u8 = 1;
const MAX_FRAME_BYTES: usize = 4096;
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct MobyImageBackend<'a, F> {
    admission: &'a ControllerEvidence,
    namespace: &'a str,
    store_id: String,
    project: i64,
    connect: F,
}

impl<'a, F> MobyImageBackend<'a, F>
where
    F: Fn() -> Option<UnixStream>,
{
    /// The controller supplies a connector that verifies the kernel peer on every connection.
    pub(super) fn open(
        admission: &'a ControllerEvidence,
        namespace: &'a str,
        project: i64,
        connect: F,
    ) -> Option<Self> {
        if project <= 0
            || !valid_owner_fingerprint(namespace)
            || !valid_owner_fingerprint(&admission.endpoint)
            || !valid_owner_fingerprint(&admission.generation)
        {
            return None;
        }
        let reply = exchange(&mut connect()?, Operation::Probe)?;
        let store_id = reply.store_id.as_str();
        if !reply.is_acknowledgement(Status::Ready)
            || !valid_owner_fingerprint(store_id)
            || store_namespace(store_id) != namespace
        {
            return None;
        }
        Some(Self {
            admission,
            namespace,
            store_id: reply.store_id,
            project,
            connect,
        })
    }

    fn observation(&self, owner: &str, id: &str, snapshot: String) -> Observation {
        Observation {
            binding: Binding {
                project: self.project,
                owner: owner.to_owned(),
                kind: DockerGcKind::Image,
                id: id.to_owned(),
                endpoint: self.namespace.to_owned(),
                generation: self.admission.generation.clone(),
                snapshot: snapshot.clone(),
            },
            unreferenced: Some(true),
            usage: Usage::Image(ImageGcPlan {
                context: "dsg-dedicated-moby".to_owned(),
                engine_id: Some(self.admission.endpoint.clone()),
                image_id: id.to_owned(),
                disposition: ImageGcDisposition::Candidate,
                reason: ImageGcReason::Eligible,
                fingerprint: Some(snapshot),
                apply_gate: DockerApplyGate::Controlled,
            }),
        }
    }

    fn contains(&self, binding: &Binding) -> bool {
        binding.project == self.project
            && binding.endpoint == self.namespace
            && binding.kind == DockerGcKind::Image
            && binding.kind.valid_id(&binding.id)
    }
}

impl<F: Fn() -> Option<UnixStream>> ControlledDockerGc for MobyImageBackend<'_, F> {
    fn observe(&mut self, owner: &str, kind: DockerGcKind, id: &str) -> Option<Observation> {
        if kind != DockerGcKind::Image || !kind.valid_id(id) || !valid_owner_fingerprint(owner) {
            return None;
        }
        let reply = exchange(
            &mut (self.connect)()?,
            Operation::Observe {
                image_id: id,
                owner,
            },
        )?;
        let snapshot = reply.observed_snapshot(&self.store_id)?;
        Some(self.observation(owner, id, snapshot))
    }

    fn apply_if_current(&mut self, _intent: &Intent) -> Option<Receipt> {
        // Only the managed dispatcher can obtain the registry claim required to commit.
        None
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        if !self.contains(&intent.binding) {
            return None;
        }
        let id = intent.binding.id.as_str();
        let reply = exchange(
            &mut (self.connect)()?,
            Operation::Receipt {
                image_id: id,
                intent,
            },
        )?;
        if !reply.matches(&self.store_id, Status::Settled) || reply.snapshot.is_some() {
            return None;
        }
        let receipt = reply.receipt?;
        if receipt.intent != *intent || receipt.reclaimed_bytes != 0 {
            return None;
        }
        Some(Receipt {
            intent: receipt.intent,
            effect: Effect::Applied(0),
        })
    }
}

impl<F: Fn() -> Option<UnixStream>> ExclusiveDockerGc for MobyImageBackend<'_, F> {
    fn engine(&self) -> Option<ExclusiveEngine<'_>> {
        Some(ExclusiveEngine {
            admission: self.admission,
            namespace: self.namespace,
        })
    }

    fn prepare(&mut self, intent: &Intent) -> Option<Box<dyn PreparedDockerGc + '_>> {
        let binding = &intent.binding;
        if !self.contains(binding) || binding.generation != self.admission.generation {
            return None;
        }
        let mut stream = (self.connect)()?;
        let reply = exchange(
            &mut stream,
            Operation::Prepare {
                image_id: &binding.id,
                intent,
            },
        )?;
        let snapshot = reply.observed_snapshot(&self.store_id)?;
        let observed = self.observation(&binding.owner, &binding.id, snapshot);
        if observed.binding != *binding {
            return None;
        }
        Some(Box::new(PreparedImage {
            stream,
            store_id: &self.store_id,
            intent: intent.clone(),
            observed,
        }))
    }
}

struct PreparedImage<'a> {
    stream: UnixStream,
    store_id: &'a str,
    intent: Intent,
    observed: Observation,
}

impl PreparedDockerGc for PreparedImage<'_> {
    fn observed(&self) -> &Observation {
        &self.observed
    }

    fn commit(mut self: Box<Self>, claim: ClaimedDockerGc<'_>) -> bool {
        let intent = claim.intent();
        if self.intent != *intent {
            return false;
        }
        exchange(
            &mut self.stream,
            Operation::Commit {
                image_id: &intent.binding.id,
                intent,
            },
        )
        .is_some_and(|reply| {
            reply.store_id == self.store_id && reply.is_acknowledgement(Status::Committed)
        })
    }
}

fn store_namespace(store_id: &str) -> String {
    let mut namespace = Sha256::new();
    namespace.update(b"dsg-docker-backend-store-v1\0");
    namespace.update(store_id.as_bytes());
    format!("{:x}", namespace.finalize())
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation<'a> {
    Probe,
    Observe {
        image_id: &'a str,
        owner: &'a str,
    },
    Prepare {
        image_id: &'a str,
        intent: &'a Intent,
    },
    Commit {
        image_id: &'a str,
        intent: &'a Intent,
    },
    Receipt {
        image_id: &'a str,
        intent: &'a Intent,
    },
}

#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    #[serde(flatten)]
    operation: Operation<'a>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ready,
    Preserved,
    Committed,
    Settled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    status: Status,
    store_id: String,
    #[serde(default, deserialize_with = "present")]
    snapshot: Option<String>,
    #[serde(default, deserialize_with = "present")]
    receipt: Option<AuditReceipt>,
}

impl Reply {
    fn matches(&self, store_id: &str, status: Status) -> bool {
        self.version == VERSION && self.store_id == store_id && self.status == status
    }

    fn is_acknowledgement(&self, status: Status) -> bool {
        self.version == VERSION
            && self.status == status
            && self.snapshot.is_none()
            && self.receipt.is_none()
    }

    fn observed_snapshot(self, store_id: &str) -> Option<String> {
        if !self.matches(store_id, Status::Ready) || self.receipt.is_some() {
            return None;
        }
        self.snapshot
            .filter(|snapshot| valid_owner_fingerprint(snapshot))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditReceipt {
    intent: Intent,
    reclaimed_bytes: u64,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn exchange(stream: &mut UnixStream, operation: Operation<'_>) -> Option<Reply> {
    let deadline = Instant::now() + EXCHANGE_TIMEOUT;
    let mut frame = serde_json::to_vec(&Request {
        version: VERSION,
        operation,
    })
    .ok()?;
    if frame.len() >= MAX_FRAME_BYTES {
        return None;
    }
    frame.push(b'\n');
    let mut pending = frame.as_slice();
    while !pending.is_empty() {
        stream.set_write_timeout(Some(time_left(deadline)?)).ok()?;
        match stream.write(pending) {
            Ok(0) => return None,
            Ok(count) => pending = &pending[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => return None,
        }
    }

    let mut frame = [0; MAX_FRAME_BYTES];
    let mut used = 0;
    while used < frame.len() {
        stream.set_read_timeout(Some(time_left(deadline)?)).ok()?;
        match stream.read(&mut frame[used..]) {
            Ok(0) => return None,
            Ok(count) => {
                let end = used + count;
                if let Some(newline) = frame[used..end].iter().position(|byte| *byte == b'\n') {
                    time_left(deadline)?;
                    let newline = used + newline;
                    return (newline + 1 == end)
                        .then(|| serde_json::from_slice(&frame[..newline]).ok())
                        .flatten();
                }
                used = end;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => return None,
        }
    }
    None
}

fn time_left(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(Instant::now())?;
    (!remaining.is_zero()).then_some(remaining)
}
