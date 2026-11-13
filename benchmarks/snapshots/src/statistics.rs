use crate::util::{require, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub(crate) metrics: BTreeMap<String, f64>,
    pub(crate) verified: bool,
    pub(crate) detail: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Block {
    pub(crate) order: Vec<String>,
    pub(crate) candidate: Observation,
    pub(crate) baseline: Option<Observation>,
}
pub(crate) fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    (values[(values.len() - 1) / 2] + values[values.len() / 2]) / 2.0
}
fn spread(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let q1 = sorted[(sorted.len() - 1) / 4];
    let q3 = sorted[(sorted.len() - 1) * 3 / 4];
    let fence = (q3 - q1) * 1.5;
    let outliers: Vec<_> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| (*v < q1 - fence || *v > q3 + fence).then_some(i))
        .collect();
    json!({"range":[sorted[0],sorted[sorted.len()-1]],"iqr":[q1,q3],"outlier_block_indices":outliers,"outliers_retained":true})
}
pub(crate) fn summarize(blocks: &[Block], paired: bool, canonical: bool) -> Result<Value> {
    require(!blocks.is_empty(), "Missing measurement blocks")?;
    let keys = &blocks[0].candidate.metrics;
    require(!keys.is_empty(), "Missing metrics")?;
    for (i, block) in blocks.iter().enumerate() {
        let expected = if paired {
            if i % 2 == 0 {
                vec!["baseline", "candidate"]
            } else {
                vec!["candidate", "baseline"]
            }
        } else {
            vec!["candidate"]
        };
        require(block.order == expected, "Invalid paired execution order")?;
        require(
            block.baseline.is_some() == paired,
            "Incomplete measurement pair",
        )?;
        for observation in std::iter::once(&block.candidate).chain(block.baseline.iter()) {
            require(observation.verified, "Unverified observation")?;
            require(
                observation.metrics.keys().eq(keys.keys()),
                "Metric sets differ",
            )?;
            require(
                observation.metrics.iter().all(|(name, v)| {
                    v.is_finite()
                        && *v >= 0.0
                        && (!name.ends_with("_seconds") || name.contains("_cpu_") || *v > 0.0)
                }),
                "Invalid raw measurement",
            )?;
        }
    }
    let mut output = BTreeMap::new();
    for name in keys.keys() {
        let candidate: Vec<_> = blocks.iter().map(|b| b.candidate.metrics[name]).collect();
        let candidate_median = median(&candidate);
        let baseline: Vec<_> = blocks
            .iter()
            .filter_map(|b| b.baseline.as_ref().map(|b| b.metrics[name]))
            .collect();
        let mut result = json!({"candidate_median":candidate_median,"candidate_range":[candidate.iter().copied().fold(f64::INFINITY,f64::min),candidate.iter().copied().fold(0.0,f64::max)],"blocks":blocks.len(),"verdict":"unpaired"});
        result["candidate_spread"] = spread(&candidate);
        if paired {
            let before = median(&baseline);
            let differences: Vec<_> = candidate
                .iter()
                .zip(&baseline)
                .map(|(a, b)| a - b)
                .collect();
            // Resample independent paired blocks, never correlated history samples.
            // A fixed seed keeps inspect/recomputed reports identical.
            let mut state = 0x9e3779b97f4a7c15_u64;
            let mut estimates = Vec::with_capacity(4000);
            for _ in 0..4000 {
                let sample: Vec<_> = (0..blocks.len())
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        differences[state as usize % differences.len()]
                    })
                    .collect();
                estimates.push(median(&sample));
            }
            estimates.sort_by(f64::total_cmp);
            let (low, high) = (estimates[99], estimates[3899]);
            let threshold = before.abs() * 0.05;
            let higher_better = name.ends_with("_per_second");
            let verdict = if name.ends_with("_cache_hits") || name.ends_with("_cache_misses") {
                "informational"
            } else if blocks.len() < 6 {
                "inconclusive"
            } else if low > threshold {
                if higher_better {
                    "faster"
                } else {
                    "slower"
                }
            } else if high < -threshold {
                if higher_better {
                    "slower"
                } else {
                    "faster"
                }
            } else if low >= -threshold && high <= threshold {
                "no_meaningful_change"
            } else {
                "inconclusive"
            };
            result["baseline_median"] = json!(before);
            result["baseline_spread"] = spread(&baseline);
            result["ratio"] = if before > 0.0 {
                json!(candidate_median / before)
            } else {
                Value::Null
            };
            result["paired_difference_95_ci"] = json!([low, high]);
            result["practical_threshold"] = json!(threshold);
            result["verdict"] = json!(verdict);
            result["canonical"] = json!(canonical);
        }
        output.insert(name, result);
    }
    Ok(json!(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blocks(delta: f64) -> Vec<Block> {
        (0..12)
            .map(|i| Block {
                order: if i % 2 == 0 {
                    vec!["baseline".into(), "candidate".into()]
                } else {
                    vec!["candidate".into(), "baseline".into()]
                },
                baseline: Some(Observation {
                    metrics: BTreeMap::from([("save_seconds".into(), 1.0 + i as f64 * 0.1)]),
                    verified: true,
                    detail: Value::Null,
                }),
                candidate: Observation {
                    metrics: BTreeMap::from([(
                        "save_seconds".into(),
                        1.0 + i as f64 * 0.1 + delta,
                    )]),
                    verified: true,
                    detail: Value::Null,
                },
            })
            .collect()
    }
    #[test]
    fn cache_counters_are_evidence_instead_of_speed_verdicts() {
        let mut data = blocks(0.0);
        for block in &mut data {
            block.baseline.as_mut().unwrap().metrics = BTreeMap::from([
                ("repeat_cache_hits".into(), 32.0),
                ("repeat_cache_misses".into(), 0.0),
            ]);
            block.candidate.metrics = BTreeMap::from([
                ("repeat_cache_hits".into(), 16.0),
                ("repeat_cache_misses".into(), 16.0),
            ]);
        }
        let result = summarize(&data, true, true).unwrap();
        assert_eq!(result["repeat_cache_hits"]["verdict"], "informational");
        assert_eq!(result["repeat_cache_misses"]["verdict"], "informational");
    }
    #[test]
    fn paired_effect_survives_overlapping_ranges_and_small_samples_are_inconclusive() {
        let data = blocks(0.2);
        assert_eq!(
            summarize(&data, true, true).unwrap()["save_seconds"]["verdict"],
            "slower"
        );
        assert_eq!(
            summarize(&data[..3], true, true).unwrap()["save_seconds"]["verdict"],
            "inconclusive"
        );
        assert_eq!(
            summarize(&blocks(0.0), true, true).unwrap()["save_seconds"]["verdict"],
            "no_meaningful_change"
        );
        assert_eq!(
            summarize(&blocks(-0.2), true, true).unwrap()["save_seconds"]["verdict"],
            "faster"
        );
    }
    #[test]
    fn incomplete_unverified_and_invalid_blocks_are_rejected() {
        let mut data = blocks(0.0);
        data[0].candidate.verified = false;
        assert!(summarize(&data, true, true).is_err());
        data[0].candidate.verified = true;
        data[0]
            .candidate
            .metrics
            .insert("save_seconds".into(), f64::NAN);
        assert!(summarize(&data, true, true).is_err());
        data[0].candidate.metrics.insert("save_seconds".into(), 1.0);
        data[0].baseline = None;
        assert!(summarize(&data, true, true).is_err());
    }
}
