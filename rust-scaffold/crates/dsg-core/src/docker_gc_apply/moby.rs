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
const EXT4_BLOCK_BYTES: u64 = 4096;

pub(super) struct MobyImageBackend<'a, F> {
    admission: &'a ControllerEvidence,
    namespace: &'a str,
    store_id: String,
    project: i64,
    connect: F,
    estimated_exclusive_layer_bytes: Option<u64>,
    settled_allocation: Option<(String, ImageAllocation)>,
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
            estimated_exclusive_layer_bytes: None,
            settled_allocation: None,
        })
    }

    pub(super) fn estimated_exclusive_layer_bytes(&self) -> Option<u64> {
        self.estimated_exclusive_layer_bytes
    }

    pub(super) fn settled_allocation(&self) -> Option<(&str, &ImageAllocation)> {
        self.settled_allocation
            .as_ref()
            .map(|(intent_id, allocation)| (intent_id.as_str(), allocation))
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
        self.estimated_exclusive_layer_bytes = None;
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
        let (snapshot, estimate) = reply.observed_image(&self.store_id)?;
        self.estimated_exclusive_layer_bytes = estimate;
        Some(self.observation(owner, id, snapshot))
    }

    fn apply_if_current(&mut self, _intent: &Intent) -> Option<Receipt> {
        // Only the managed dispatcher can obtain the registry claim required to commit.
        None
    }

    fn receipt(&mut self, intent: &Intent) -> Option<Receipt> {
        self.settled_allocation = None;
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
        let receipt = reply.settled_receipt(&self.store_id, intent)?;
        self.settled_allocation = receipt
            .allocation
            .map(|allocation| (receipt.intent.operation.clone(), allocation));
        Some(Receipt {
            intent: receipt.intent,
            effect: Effect::Applied(receipt.reclaimed_bytes),
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
        let (snapshot, _) = reply.observed_image(&self.store_id)?;
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
    estimated_exclusive_layer_bytes: Option<u64>,
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
            && self.estimated_exclusive_layer_bytes.is_none()
            && self.receipt.is_none()
    }

    fn observed_image(self, store_id: &str) -> Option<(String, Option<u64>)> {
        if !self.matches(store_id, Status::Ready) || self.receipt.is_some() {
            return None;
        }
        let snapshot = self
            .snapshot
            .filter(|snapshot| valid_owner_fingerprint(snapshot))?;
        Some((snapshot, self.estimated_exclusive_layer_bytes))
    }

    fn settled_receipt(self, store_id: &str, intent: &Intent) -> Option<AuditReceipt> {
        if !self.matches(store_id, Status::Settled)
            || self.snapshot.is_some()
            || self.estimated_exclusive_layer_bytes.is_some()
        {
            return None;
        }
        self.receipt.filter(|receipt| receipt.valid_for(intent))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditReceipt {
    intent: Intent,
    reclaimed_bytes: u64,
    #[serde(default, deserialize_with = "present")]
    allocation: Option<ImageAllocation>,
}

impl AuditReceipt {
    fn valid_for(&self, intent: &Intent) -> bool {
        self.intent == *intent
            && match &self.allocation {
                Some(allocation) => allocation.valid_for(self.reclaimed_bytes),
                None => self.reclaimed_bytes == 0,
            }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageAllocation {
    scope: AllocationScope,
    block_bytes: u64,
    estimated_exclusive_layer_bytes: u64,
    ext4_before_bytes: u64,
    ext4_after_bytes: u64,
    ext4_net_reclaimed_bytes: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AllocationScope {
    DedicatedExt4,
}

impl ImageAllocation {
    fn valid_for(&self, reclaimed_bytes: u64) -> bool {
        self.block_bytes == EXT4_BLOCK_BYTES
            && self.ext4_before_bytes % EXT4_BLOCK_BYTES == 0
            && self.ext4_after_bytes % EXT4_BLOCK_BYTES == 0
            && i128::from(self.ext4_before_bytes) - i128::from(self.ext4_after_bytes)
                == i128::from(self.ext4_net_reclaimed_bytes)
            && u64::try_from(self.ext4_net_reclaimed_bytes).unwrap_or(0) == reclaimed_bytes
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const STORE: &str = "exact-store";

    fn intent() -> Intent {
        Intent {
            operation: "a".repeat(64),
            policy: "b".repeat(64),
            binding: Binding {
                project: 1,
                owner: "c".repeat(64),
                kind: DockerGcKind::Image,
                id: format!("sha256:{}", "d".repeat(64)),
                endpoint: "e".repeat(64),
                generation: "f".repeat(64),
                snapshot: "0".repeat(64),
            },
        }
    }

    fn receipt_reply(intent: &Intent, bytes: u64) -> Value {
        json!({"version":1,"status":"settled","store_id":STORE,
            "receipt":{"intent":intent,"reclaimed_bytes":bytes}})
    }

    fn settle(value: Value, intent: &Intent) -> Option<AuditReceipt> {
        serde_json::from_value::<Reply>(value)
            .ok()?
            .settled_receipt(STORE, intent)
    }

    fn allocation(before: u64, after: u64, net: i64) -> Value {
        json!({"scope":"dedicated_ext4","block_bytes":4096,
            "estimated_exclusive_layer_bytes":123,"ext4_before_bytes":before,
            "ext4_after_bytes":after,"ext4_net_reclaimed_bytes":net})
    }

    #[test]
    fn ready_estimate_is_optional_and_requires_snapshot() {
        let snapshot = intent().binding.snapshot;
        let mut ready = json!({"version":1,"status":"ready","store_id":STORE});
        let probe = serde_json::from_value::<Reply>(ready.clone()).unwrap();
        assert!(probe.is_acknowledgement(Status::Ready));
        ready["estimated_exclusive_layer_bytes"] = json!(0);
        let estimate_without_snapshot = serde_json::from_value::<Reply>(ready.clone()).unwrap();
        assert!(!estimate_without_snapshot.is_acknowledgement(Status::Ready));
        assert!(estimate_without_snapshot.observed_image(STORE).is_none());
        ready["snapshot"] = json!(&snapshot);
        assert_eq!(
            serde_json::from_value::<Reply>(ready.clone())
                .unwrap()
                .observed_image(STORE),
            Some((snapshot.clone(), Some(0)))
        );
        ready
            .as_object_mut()
            .unwrap()
            .remove("estimated_exclusive_layer_bytes");
        assert_eq!(
            serde_json::from_value::<Reply>(ready)
                .unwrap()
                .observed_image(STORE),
            Some((snapshot, None))
        );
    }

    #[test]
    fn receipt_accepts_legacy_zero_and_requires_exact_measured_intent() {
        let intent = intent();
        let legacy = settle(receipt_reply(&intent, 0), &intent).unwrap();
        assert!(legacy.allocation.is_none());
        assert!(settle(receipt_reply(&intent, 1), &intent).is_none());

        let mut measured = receipt_reply(&intent, 4096);
        measured["receipt"]["allocation"] = allocation(8192, 4096, 4096);
        assert_eq!(
            settle(measured.clone(), &intent).unwrap().reclaimed_bytes,
            4096
        );
        let mut different = intent.clone();
        different.operation = "1".repeat(64);
        assert!(settle(measured.clone(), &different).is_none());
        different = intent.clone();
        different.binding.snapshot = "2".repeat(64);
        assert!(settle(measured.clone(), &different).is_none());
        measured["store_id"] = json!("different");
        assert!(settle(measured, &intent).is_none());
    }

    #[test]
    fn receipt_preserves_signed_allocation_and_rejects_malformed_evidence() {
        let intent = intent();
        let mut measured = receipt_reply(&intent, 0);
        measured["receipt"]["allocation"] = allocation(4096, 8192, -4096);
        let verified = settle(measured.clone(), &intent)
            .unwrap()
            .allocation
            .unwrap();
        assert_eq!(verified.ext4_net_reclaimed_bytes, -4096);
        assert_eq!(
            serde_json::to_value(verified).unwrap(),
            measured["receipt"]["allocation"]
        );

        let mut invalid = [
            allocation(4096, 8192, 4096),
            allocation(4097, 8193, -4096),
            allocation(1 << 63, 0, i64::MAX),
            allocation(8192, 4096, 4096),
            allocation(4096, 8192, -4096),
            allocation(4096, 8192, -4096),
            allocation(4096, 8192, -4096),
            Value::Null,
        ];
        invalid[4]["scope"] = json!("external");
        invalid[5]["block_bytes"] = json!(512);
        invalid[6]["unexpected"] = json!(true);
        for bad in invalid {
            measured["receipt"]["allocation"] = bad;
            assert!(settle(measured.clone(), &intent).is_none());
        }
        measured["receipt"]["allocation"] = allocation(4096, 8192, -4096);
        measured["estimated_exclusive_layer_bytes"] = json!(123);
        assert!(settle(measured, &intent).is_none());
    }
}
