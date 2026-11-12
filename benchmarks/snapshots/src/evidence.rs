//! Raw observations are authoritative; stored aggregates are recomputed and checked.
use crate::{
    profiles::{Case, Workload},
    statistics::Observation,
    util::{require, Result},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Timing {
    pub(crate) seconds: f64,
    pub(crate) peak_rss_kib: u64,
    pub(crate) git_steps_ms: Vec<u64>,
    pub(crate) cpu: Option<Cpu>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cpu {
    pub(crate) user_seconds: Option<f64>,
    pub(crate) system_seconds: Option<f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Storage {
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
}
#[derive(Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub(crate) measurements: BTreeMap<String, Vec<Timing>>,
    pub(crate) checks: BTreeMap<String, bool>,
    pub(crate) object_growth: Option<Storage>,
    pub(crate) batch_seconds: Option<f64>,
}
impl Evidence {
    pub(crate) fn record(&mut self, name: &str, timing: serde_json::Value) -> Result<()> {
        self.measurements
            .entry(name.into())
            .or_default()
            .push(serde_json::from_value(timing)?);
        Ok(())
    }
    pub(crate) fn checked(&mut self, name: &str) {
        self.checks.insert(name.into(), true);
    }
}
pub(crate) fn observation(case: &Case, evidence: Evidence) -> Result<Observation> {
    let (operations, checks): (Vec<(&str, usize)>, &[&str]) = match case.workload {
        Workload::SnapshotSave { history_depth, .. } => (
            vec![
                ("first_save", 1),
                ("fresh_changed", 1),
                ("history_changed", history_depth - 1),
                ("unchanged_save", 1),
            ],
            &[
                "checkout_preserved",
                "saved_trees",
                "unchanged_blob_reuse",
                "recovery_trees",
            ],
        ),
        Workload::SnapshotRestore => (
            vec![
                ("clean_external_restore", 1),
                ("clean_inplace_restore", 1),
                ("dirty_external_restore", 1),
                ("dirty_inplace_restore", 1),
            ],
            &[
                "source_preserved",
                "clean_recovery",
                "dirty_recovery",
                "ignored_preserved",
            ],
        ),
        Workload::WorkspaceReuse { .. } => (
            vec![
                ("first_acquire", 1),
                ("warm_acquire", 1),
                ("dirty_acquire", 1),
                ("release", 1),
            ],
            &[
                "pristine_checkout",
                "cleanup_policy",
                "released",
                "source_preserved",
            ],
        ),
        Workload::WorkspaceSetup { .. } => (
            vec![("setup_miss", 1), ("setup_hit", 1)],
            &[
                "cache_miss",
                "cache_hit",
                "artifact_contents",
                "step_skipped",
                "released",
                "source_preserved",
            ],
        ),
        Workload::WorkspaceConcurrency { workers } => (
            vec![
                ("acquire", workers),
                ("exec", workers),
                ("release", workers),
                ("full_pool_rejection", 1),
            ],
            &[
                "unique_reservations",
                "successful_workers",
                "full_pool_rejection",
                "released",
                "source_preserved",
            ],
        ),
    };
    let expected: BTreeSet<_> = operations.iter().map(|(n, _)| n.to_string()).collect();
    require(
        evidence.measurements.keys().eq(expected.iter()),
        "Operation set differs from profile case",
    )?;
    let expected: BTreeSet<_> = checks.iter().map(|s| s.to_string()).collect();
    require(
        evidence.checks.keys().eq(expected.iter()) && evidence.checks.values().all(|v| *v),
        "Missing or failed correctness assertion",
    )?;
    let mut metrics = BTreeMap::new();
    for (name, count) in operations {
        let samples = &evidence.measurements[name];
        require(
            samples.len() == count,
            "Raw sample count differs from profile case",
        )?;
        for sample in samples {
            require(
                sample.seconds.is_finite() && sample.seconds > 0.0 && sample.peak_rss_kib > 0,
                "Invalid timing/resource sample",
            )?;
            if let Some(cpu) = &sample.cpu {
                for value in [cpu.user_seconds, cpu.system_seconds].into_iter().flatten() {
                    require(value.is_finite() && value >= 0.0, "Invalid CPU sample")?;
                }
            }
        }
        metrics.insert(
            format!("{name}_seconds"),
            crate::statistics::median(&samples.iter().map(|s| s.seconds).collect::<Vec<_>>()),
        );
        metrics.insert(
            format!("{name}_peak_rss_kib"),
            samples
                .iter()
                .map(|s| s.peak_rss_kib)
                .max()
                .expect("validated nonempty samples") as f64,
        );
        if samples.len() > 1 {
            metrics.insert(
                format!("{name}_max_seconds"),
                samples.iter().map(|s| s.seconds).fold(0.0, f64::max),
            );
        }
        for (role, system) in [("user", false), ("system", true)] {
            let values: Vec<_> = samples
                .iter()
                .filter_map(|s| {
                    s.cpu.as_ref().and_then(|c| {
                        if system {
                            c.system_seconds
                        } else {
                            c.user_seconds
                        }
                    })
                })
                .collect();
            require(
                values.is_empty() || values.len() == samples.len(),
                "Incomplete CPU samples",
            )?;
            if !values.is_empty() {
                metrics.insert(
                    format!("{name}_cpu_{role}_seconds"),
                    crate::statistics::median(&values),
                );
            }
        }
    }
    require(
        evidence.object_growth.is_some() == matches!(case.workload, Workload::SnapshotSave { .. }),
        "Storage measurements differ from profile case",
    )?;
    if let Some(storage) = &evidence.object_growth {
        metrics.insert(
            "object_growth_logical_bytes".into(),
            storage.logical_bytes as f64,
        );
        metrics.insert(
            "object_growth_allocated_bytes".into(),
            storage.allocated_bytes as f64,
        );
    }
    if let Workload::WorkspaceConcurrency { workers } = case.workload {
        let seconds = evidence.batch_seconds.ok_or_else(|| {
            crate::util::Error::Invalid("Missing concurrent wall measurement".into())
        })?;
        require(
            seconds.is_finite() && seconds > 0.0,
            "Invalid concurrent wall measurement",
        )?;
        metrics.insert("batch_seconds".into(), seconds);
        metrics.insert("throughput_per_second".into(), workers as f64 / seconds);
    } else {
        require(
            evidence.batch_seconds.is_none(),
            "Unexpected concurrent measurement",
        )?;
    }
    Ok(Observation {
        metrics,
        verified: true,
        detail: serde_json::to_value(evidence)?,
    })
}
