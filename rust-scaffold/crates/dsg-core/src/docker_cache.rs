use crate::docker::{
    query_engine, valid_context_name, DockerOperation, DockerRead, InventoryIssue, InventoryStage,
    InventoryStatus, QueryFailure,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_SUMMARY_BYTES: usize = 128;
const MAX_BUILDX_BYTES: usize = 64 * 1024;
const MAX_BUILDX_RECORDS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheDisposition {
    Preserve,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheReason {
    UnknownOwnership,
    IncompleteInventory,
}

#[derive(Debug, Serialize)]
pub struct BuildCacheInventory {
    pub context: String,
    pub engine_id: Option<String>,
    pub status: InventoryStatus,
    pub total_count: Option<u64>,
    pub active_count: Option<u64>,
    pub disposition: BuildCacheDisposition,
    pub reason: BuildCacheReason,
    pub issues: Vec<InventoryIssue>,
}

pub trait BuildxRead: DockerRead {
    fn query_buildx_usage(&self) -> Result<String, QueryFailure>;
}

#[derive(Debug, Serialize)]
pub struct BuildxCacheInventory {
    pub context: String,
    pub engine_id: Option<String>,
    pub status: InventoryStatus,
    pub total_count: Option<u64>,
    pub reclaimable_count: Option<u64>,
    pub shared_count: Option<u64>,
    pub mutable_count: Option<u64>,
    pub candidate_count: u64,
    pub preserved_by_reason: Option<CachePreservation>,
    pub disposition: BuildCacheDisposition,
    pub reason: BuildCacheReason,
    pub issues: Vec<InventoryIssue>,
}

#[derive(Default, Serialize)]
pub struct BuildxUsage {
    pub total: u64,
    reclaimable: u64,
    shared: u64,
    mutable: u64,
    candidate_count: u64,
    preserved_by_reason: CachePreservation,
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
pub struct CachePreservation {
    active: u64,
    shared: u64,
    mutable: u64,
    unknown_ownership: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CachePreserveReason {
    Active,
    Shared,
    Mutable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CacheRecordState {
    pub(crate) reclaimable: bool,
    pub(crate) shared: bool,
    pub(crate) mutable: bool,
}

impl CacheRecordState {
    /// Evaluate usage alone. A missing blocker carries no ownership or apply authority.
    pub(crate) fn reclaim_blocker(self) -> Option<CachePreserveReason> {
        if !self.reclaimable {
            Some(CachePreserveReason::Active)
        } else if self.shared {
            Some(CachePreserveReason::Shared)
        } else if self.mutable {
            Some(CachePreserveReason::Mutable)
        } else {
            None
        }
    }
}

impl BuildxUsage {
    fn record_external(&mut self, state: CacheRecordState) {
        self.total += 1;
        self.reclaimable += u64::from(state.reclaimable);
        self.shared += u64::from(state.shared);
        self.mutable += u64::from(state.mutable);

        // Standard builder observations carry no project ownership evidence.
        match state.reclaim_blocker() {
            Some(CachePreserveReason::Active) => self.preserved_by_reason.active += 1,
            Some(CachePreserveReason::Shared) => self.preserved_by_reason.shared += 1,
            Some(CachePreserveReason::Mutable) => self.preserved_by_reason.mutable += 1,
            None => self.preserved_by_reason.unknown_ownership += 1,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageSummary<'a> {
    total: &'a str,
    active: &'a str,
}

fn count(value: &str) -> Result<u64, QueryFailure> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(QueryFailure::Malformed);
    }
    value.parse().map_err(|_| QueryFailure::Malformed)
}

fn query_usage(query: &impl DockerRead) -> Result<(u64, u64), QueryFailure> {
    let response = query.query(DockerOperation::BuildCache)?;
    if response.len() > MAX_SUMMARY_BYTES {
        return Err(QueryFailure::TooLarge);
    }
    // The Docker formatter can emit blank rows for other resource types.
    if !response.trim_start().starts_with('{') {
        return Err(QueryFailure::Malformed);
    }
    let summary: UsageSummary<'_> =
        serde_json::from_str(&response).map_err(|_| QueryFailure::Malformed)?;
    let total = count(summary.total)?;
    let active = count(summary.active)?;
    if active > total {
        return Err(QueryFailure::Malformed);
    }
    Ok((total, active))
}

fn engine_usage(query: &impl DockerRead) -> Result<(String, u64, u64), InventoryIssue> {
    let engine = query_engine(query).map_err(|failure| InventoryIssue {
        stage: InventoryStage::Engine,
        failure,
    })?;
    let (total, active) = query_usage(query).map_err(|failure| InventoryIssue {
        stage: InventoryStage::BuildCache,
        failure,
    })?;
    recheck_engine(query, &engine)?;
    Ok((engine, total, active))
}

fn recheck_engine(query: &impl DockerRead, engine: &str) -> Result<(), InventoryIssue> {
    let issue = |failure| InventoryIssue {
        stage: InventoryStage::EngineRecheck,
        failure,
    };
    if query_engine(query).map_err(issue)? != engine {
        return Err(issue(QueryFailure::Malformed));
    }
    Ok(())
}

/// Observe only the selected engine's embedded builder cache summary.
pub fn build_cache_inventory(query: &impl DockerRead) -> BuildCacheInventory {
    let mut inventory = BuildCacheInventory {
        context: String::new(),
        engine_id: None,
        status: InventoryStatus::Unknown,
        total_count: None,
        active_count: None,
        disposition: BuildCacheDisposition::Preserve,
        reason: BuildCacheReason::IncompleteInventory,
        issues: Vec::new(),
    };
    let context = query.context();
    if !valid_context_name(context) {
        inventory.issues.push(InventoryIssue {
            stage: InventoryStage::Context,
            failure: QueryFailure::InvalidSelector,
        });
        return inventory;
    }
    inventory.context = context.to_owned();
    match engine_usage(query) {
        Ok((engine, total, active)) => {
            inventory.engine_id = Some(engine);
            inventory.status = InventoryStatus::Complete;
            inventory.total_count = Some(total);
            inventory.active_count = Some(active);
            inventory.reason = BuildCacheReason::UnknownOwnership;
        }
        Err(issue) => inventory.issues.push(issue),
    }
    inventory
}

fn buildx_flag(value: &str) -> Result<bool, QueryFailure> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(QueryFailure::Malformed),
    }
}

fn cache_record_id(value: &str) -> Result<&str, QueryFailure> {
    let id = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(QueryFailure::Malformed)?;
    // Both formatters quote IDs so an invalid ID cannot introduce another row.
    // Treat accepted IDs as opaque and use them only to check uniqueness.
    if !valid_cache_record_id(id) {
        return Err(QueryFailure::Malformed);
    }
    Ok(id)
}

pub(crate) fn valid_cache_record_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

pub(crate) fn buildx_usage(response: &str) -> Result<BuildxUsage, QueryFailure> {
    if response.len() > MAX_BUILDX_BYTES {
        return Err(QueryFailure::TooLarge);
    }
    if !response.is_empty() && !response.ends_with('\n') {
        return Err(QueryFailure::Malformed);
    }
    let mut usage = BuildxUsage::default();
    let mut ids = HashSet::new();
    for (index, row) in response.lines().enumerate() {
        if index >= MAX_BUILDX_RECORDS {
            return Err(QueryFailure::TooLarge);
        }
        let (id, rest) = row.split_once('|').ok_or(QueryFailure::Malformed)?;
        if !ids.insert(cache_record_id(id)?) {
            return Err(QueryFailure::Malformed);
        }
        let (reclaimable, rest) = rest.split_once('|').ok_or(QueryFailure::Malformed)?;
        let (shared, mutable) = rest.split_once('|').ok_or(QueryFailure::Malformed)?;
        usage.record_external(CacheRecordState {
            reclaimable: buildx_flag(reclaimable)?,
            shared: buildx_flag(shared)?,
            mutable: buildx_flag(mutable)?,
        });
    }
    Ok(usage)
}

fn engine_buildx_usage(query: &impl BuildxRead) -> Result<(String, BuildxUsage), InventoryIssue> {
    let engine = query_engine(query).map_err(|failure| InventoryIssue {
        stage: InventoryStage::Engine,
        failure,
    })?;
    let usage = query
        .query_buildx_usage()
        .and_then(|response| buildx_usage(&response))
        .map_err(|failure| InventoryIssue {
            stage: InventoryStage::BuildCache,
            failure,
        })?;
    recheck_engine(query, &engine)?;
    Ok((engine, usage))
}

/// Observe records from the selected context's engine-provided Buildx builder.
pub fn buildx_cache_inventory(query: &impl BuildxRead) -> BuildxCacheInventory {
    let mut inventory = BuildxCacheInventory {
        context: String::new(),
        engine_id: None,
        status: InventoryStatus::Unknown,
        total_count: None,
        reclaimable_count: None,
        shared_count: None,
        mutable_count: None,
        candidate_count: 0,
        preserved_by_reason: None,
        disposition: BuildCacheDisposition::Preserve,
        reason: BuildCacheReason::IncompleteInventory,
        issues: Vec::new(),
    };
    let context = query.context();
    if !valid_context_name(context) {
        inventory.issues.push(InventoryIssue {
            stage: InventoryStage::Context,
            failure: QueryFailure::InvalidSelector,
        });
        return inventory;
    }
    inventory.context = context.to_owned();
    match engine_buildx_usage(query) {
        Ok((engine, usage)) => {
            inventory.engine_id = Some(engine);
            inventory.status = InventoryStatus::Complete;
            inventory.total_count = Some(usage.total);
            inventory.reclaimable_count = Some(usage.reclaimable);
            inventory.shared_count = Some(usage.shared);
            inventory.mutable_count = Some(usage.mutable);
            inventory.candidate_count = usage.candidate_count;
            inventory.preserved_by_reason = Some(usage.preserved_by_reason);
            inventory.reason = BuildCacheReason::UnknownOwnership;
        }
        Err(issue) => inventory.issues.push(issue),
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Fixture<'a> {
        context: &'a str,
        calls: RefCell<VecDeque<(DockerOperation<'a>, Result<&'a str, QueryFailure>)>>,
    }

    impl DockerRead for Fixture<'_> {
        fn context(&self) -> &str {
            self.context
        }

        fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
            let (expected, result) = self
                .calls
                .borrow_mut()
                .pop_front()
                .expect("unexpected query");
            assert_eq!(expected, operation);
            result.map(str::to_owned)
        }
    }

    fn read(calls: &[(DockerOperation<'_>, Result<&str, QueryFailure>)]) -> BuildCacheInventory {
        let fixture = Fixture {
            context: "isolated-test",
            calls: RefCell::new(calls.iter().copied().collect()),
        };
        let inventory = build_cache_inventory(&fixture);
        assert!(fixture.calls.borrow().is_empty());
        inventory
    }

    struct BuildxFixture<'a> {
        engine: Fixture<'a>,
        usage: Result<&'a str, QueryFailure>,
        calls_before_usage: usize,
    }

    impl DockerRead for BuildxFixture<'_> {
        fn context(&self) -> &str {
            self.engine.context()
        }

        fn query(&self, operation: DockerOperation<'_>) -> Result<String, QueryFailure> {
            self.engine.query(operation)
        }
    }

    impl BuildxRead for BuildxFixture<'_> {
        fn query_buildx_usage(&self) -> Result<String, QueryFailure> {
            assert_eq!(self.engine.calls.borrow().len(), self.calls_before_usage);
            self.usage.map(str::to_owned)
        }
    }

    fn read_buildx(
        usage: Result<&str, QueryFailure>,
        recheck: Option<Result<&str, QueryFailure>>,
    ) -> BuildxCacheInventory {
        let mut calls = VecDeque::from([(DockerOperation::Engine, Ok("engine-test"))]);
        if let Some(recheck) = recheck {
            calls.push_back((DockerOperation::Engine, recheck));
        }
        let fixture = BuildxFixture {
            engine: Fixture {
                context: "isolated-test",
                calls: RefCell::new(calls),
            },
            usage,
            calls_before_usage: usize::from(recheck.is_some()),
        };
        let inventory = buildx_cache_inventory(&fixture);
        assert!(fixture.engine.calls.borrow().is_empty());
        inventory
    }

    #[test]
    fn buildx_counts_flags_and_preserves_all_records_including_empty_usage() {
        for (wire, expected, preserved) in [
            (
                concat!(
                    "\"record-1\"|false|true|true\n",
                    "\"record-2\"|true|true|true\n",
                    "\"record-3\"|true|false|true\r\n",
                    "\"record-4\"|true|false|false\n",
                ),
                [4, 3, 2, 3],
                CachePreservation {
                    active: 1,
                    shared: 1,
                    mutable: 1,
                    unknown_ownership: 1,
                },
            ),
            ("", [0, 0, 0, 0], CachePreservation::default()),
        ] {
            let result = read_buildx(Ok(wire), Some(Ok("engine-test")));
            assert_eq!(result.engine_id.as_deref(), Some("engine-test"));
            assert_eq!(result.status, InventoryStatus::Complete);
            assert_eq!(
                [
                    result.total_count,
                    result.reclaimable_count,
                    result.shared_count,
                    result.mutable_count
                ],
                expected.map(Some)
            );
            assert_eq!(result.disposition, BuildCacheDisposition::Preserve);
            assert_eq!(result.reason, BuildCacheReason::UnknownOwnership);
            assert_eq!(result.candidate_count, 0);
            assert_eq!(result.preserved_by_reason.as_ref(), Some(&preserved));
            assert!(result.issues.is_empty());

            let counts = serde_json::to_value(buildx_usage(wire).unwrap()).unwrap();
            for (key, count) in ["total", "reclaimable", "shared", "mutable"]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(counts[key], count);
            }
            assert_eq!(counts["candidate_count"], 0);
            assert_eq!(counts["preserved_by_reason"], serde_json::json!(preserved));
            assert!(!serde_json::to_string(&result).unwrap().contains("record-1"));
        }
    }

    #[test]
    fn buildx_discards_all_counts_for_malformed_limited_unavailable_or_changed_usage() {
        let too_many = "\"id\"|true|true|true\n".repeat(MAX_BUILDX_RECORDS + 1);
        let too_large = " ".repeat(MAX_BUILDX_BYTES + 1);
        for (wire, recheck, stage, failure) in [
            (
                Ok("\"id\"|true|false|true|raw marker\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\"id\"|true|False|true\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\"id\"|true|false|true"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\"id\"|true|false|true\n\"id\"|true|false|false\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\"id\\nforeign\"|true|false|false\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\"\"|true|false|false\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("\n"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok(&too_many),
                None,
                InventoryStage::BuildCache,
                QueryFailure::TooLarge,
            ),
            (
                Ok(&too_large),
                None,
                InventoryStage::BuildCache,
                QueryFailure::TooLarge,
            ),
            (
                Err(QueryFailure::Unavailable),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Unavailable,
            ),
            (
                Err(QueryFailure::Timeout),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Timeout,
            ),
            (
                Ok("\"id\"|true|false|true\n"),
                Some(Ok("engine-changed")),
                InventoryStage::EngineRecheck,
                QueryFailure::Malformed,
            ),
        ] {
            let result = read_buildx(wire, recheck);
            assert_eq!(result.status, InventoryStatus::Unknown);
            assert_eq!(result.engine_id, None);
            assert_eq!(
                [
                    result.total_count,
                    result.reclaimable_count,
                    result.shared_count,
                    result.mutable_count
                ],
                [None; 4]
            );
            assert_eq!(result.disposition, BuildCacheDisposition::Preserve);
            assert_eq!(result.reason, BuildCacheReason::IncompleteInventory);
            assert_eq!(result.candidate_count, 0);
            assert_eq!(result.preserved_by_reason, None);
            assert_eq!(result.issues, [InventoryIssue { stage, failure }]);
            assert!(!serde_json::to_string(&result)
                .unwrap()
                .contains("raw marker"));
        }
    }

    #[test]
    fn only_reports_usage_and_preserves_unowned_cache_including_an_empty_summary() {
        for (wire, total, active) in [
            ("\n\n{\"total\":\"208\",\"active\":\"8\"}\n", 208, 8),
            (r#"{"total":"0","active":"0"}"#, 0, 0),
        ] {
            let inventory = read(&[
                (DockerOperation::Engine, Ok("engine-test")),
                (DockerOperation::BuildCache, Ok(wire)),
                (DockerOperation::Engine, Ok("engine-test")),
            ]);
            assert_eq!(inventory.engine_id.as_deref(), Some("engine-test"));
            assert_eq!(inventory.status, InventoryStatus::Complete);
            assert_eq!(inventory.total_count, Some(total));
            assert_eq!(inventory.active_count, Some(active));
            assert_eq!(inventory.disposition, BuildCacheDisposition::Preserve);
            assert_eq!(inventory.reason, BuildCacheReason::UnknownOwnership);
            assert!(inventory.issues.is_empty());
        }
    }

    #[test]
    fn rejects_missing_duplicate_or_malformed_summaries_and_raw_output() {
        let oversized = " ".repeat(MAX_SUMMARY_BYTES + 1);
        for (wire, expected) in [
            ("", QueryFailure::Malformed),
            (r#"["1","0"]"#, QueryFailure::Malformed),
            (r#"{"total":"1","active":"2"}"#, QueryFailure::Malformed),
            (r#"{"total":"1","active":"+1"}"#, QueryFailure::Malformed),
            (r#"{"total":"1"}"#, QueryFailure::Malformed),
            (
                r#"{"total":"1","active":"0","description":"raw marker"}"#,
                QueryFailure::Malformed,
            ),
            (
                r#"{"total":"1","active":"0","total":"2"}"#,
                QueryFailure::Malformed,
            ),
            (
                r#"{"total":"1","active":"0"} {"total":"2","active":"0"}"#,
                QueryFailure::Malformed,
            ),
            (&oversized, QueryFailure::TooLarge),
        ] {
            let inventory = read(&[
                (DockerOperation::Engine, Ok("engine-test")),
                (DockerOperation::BuildCache, Ok(wire)),
            ]);
            assert_eq!(inventory.status, InventoryStatus::Unknown);
            assert_eq!(inventory.total_count, None);
            assert_eq!(inventory.active_count, None);
            assert_eq!(inventory.disposition, BuildCacheDisposition::Preserve);
            assert_eq!(inventory.reason, BuildCacheReason::IncompleteInventory);
            assert_eq!(
                inventory.issues,
                [InventoryIssue {
                    stage: InventoryStage::BuildCache,
                    failure: expected
                }]
            );
            assert!(!serde_json::to_string(&inventory)
                .unwrap()
                .contains("raw marker"));
        }
    }

    #[test]
    fn retains_no_counts_when_engine_identity_changes_or_cannot_be_confirmed() {
        for (engine_recheck, expected) in [
            (Ok("engine-other"), QueryFailure::Malformed),
            (Err(QueryFailure::Timeout), QueryFailure::Timeout),
        ] {
            let inventory = read(&[
                (DockerOperation::Engine, Ok("engine-test")),
                (
                    DockerOperation::BuildCache,
                    Ok(r#"{"total":"9","active":"3"}"#),
                ),
                (DockerOperation::Engine, engine_recheck),
            ]);
            assert_eq!(inventory.status, InventoryStatus::Unknown);
            assert_eq!(inventory.engine_id, None);
            assert_eq!(inventory.total_count, None);
            assert_eq!(inventory.active_count, None);
            assert_eq!(
                inventory.issues,
                [InventoryIssue {
                    stage: InventoryStage::EngineRecheck,
                    failure: expected
                }]
            );
        }
    }

    #[test]
    fn unavailable_usage_does_not_look_like_an_empty_cache() {
        let inventory = read(&[
            (DockerOperation::Engine, Ok("engine-test")),
            (DockerOperation::BuildCache, Err(QueryFailure::Unavailable)),
        ]);
        assert_eq!(inventory.status, InventoryStatus::Unknown);
        assert_eq!(inventory.total_count, None);
        assert_eq!(inventory.active_count, None);
        assert_eq!(inventory.disposition, BuildCacheDisposition::Preserve);
        assert_eq!(
            inventory.issues,
            [InventoryIssue {
                stage: InventoryStage::BuildCache,
                failure: QueryFailure::Unavailable
            }]
        );
    }
}
