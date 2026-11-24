use crate::docker::{
    query_engine, valid_context_name, DockerOperation, DockerRead, InventoryIssue, InventoryStage,
    InventoryStatus, QueryFailure,
};
use serde::{Deserialize, Serialize};

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

fn buildx_flag(value: &str) -> Result<u64, QueryFailure> {
    match value {
        "true" => Ok(1),
        "false" => Ok(0),
        _ => Err(QueryFailure::Malformed),
    }
}

pub(crate) fn buildx_usage(response: &str) -> Result<BuildxUsage, QueryFailure> {
    if response.len() > MAX_BUILDX_BYTES {
        return Err(QueryFailure::TooLarge);
    }
    let mut usage = BuildxUsage::default();
    for (index, row) in response.lines().enumerate() {
        if index >= MAX_BUILDX_RECORDS {
            return Err(QueryFailure::TooLarge);
        }
        let (reclaimable, rest) = row.split_once('|').ok_or(QueryFailure::Malformed)?;
        let (shared, mutable) = rest.split_once('|').ok_or(QueryFailure::Malformed)?;
        usage.reclaimable += buildx_flag(reclaimable)?;
        usage.shared += buildx_flag(shared)?;
        usage.mutable += buildx_flag(mutable)?;
        usage.total += 1;
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
        for (wire, expected) in [
            (
                "true|true|false\ntrue|false|false\nfalse|false|true\r\n",
                [3, 2, 1, 1],
            ),
            ("", [0, 0, 0, 0]),
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
            assert!(result.issues.is_empty());
        }
    }

    #[test]
    fn buildx_discards_all_counts_for_malformed_limited_unavailable_or_changed_usage() {
        let too_many = "true|true|true\n".repeat(MAX_BUILDX_RECORDS + 1);
        let too_large = " ".repeat(MAX_BUILDX_BYTES + 1);
        for (wire, recheck, stage, failure) in [
            (
                Ok("true|false|true|raw marker"),
                None,
                InventoryStage::BuildCache,
                QueryFailure::Malformed,
            ),
            (
                Ok("true|False|true"),
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
                Ok("true|false|true"),
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
