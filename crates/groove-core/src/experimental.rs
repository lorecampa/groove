//! Named, opt-in features with persistent recognition after graduation.
//!
//! Register real features in [`FEATURES`]. Keep stabilized and removed entries so
//! saved configuration receives a deliberate interpretation on future releases.
//! Configuration capabilities never confer ownership or replace storage-version
//! checks. There are currently no registered experiments.

use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The lifecycle of a registered configuration capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureStatus {
    /// Disabled unless explicitly requested by user or project configuration.
    Experimental,
    /// Enabled by default; old opt-in entries remain accepted.
    Stabilized {
        /// First binary version in which the feature became stable.
        since: &'static str,
    },
    /// No longer implemented; requests fail rather than silently changing intent.
    Removed {
        /// First binary version without the feature.
        since: &'static str,
        /// Explanation or replacement shown when the old name is requested.
        reason: &'static str,
    },
}

/// A permanent feature name and its release history.
///
/// Names must never be repurposed. Runtime gates should use the registered
/// definition with [`FeatureSet::is_enabled`] rather than inspecting raw strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    /// Stable spelling used inside `experimental.features`.
    pub name: &'static str,
    /// First binary version accepting this feature name.
    pub introduced_in: &'static str,
    /// Current runtime and compatibility behavior.
    pub status: FeatureStatus,
}

/// Registry of real features, including retained stabilized and removed names.
///
/// No experiments are registered yet. Adding a feature requires its gate,
/// documentation and regression coverage in the same change; do not add dummy
/// names or a switch that automatically enables all future experiments.
pub const FEATURES: &[Feature] = &[];

/// Opt-in requests shared by user preferences and `.groove.toml`.
///
/// Missing configuration means no experimental opt-in. Deserialization checks
/// syntax; call [`Self::validate`] or [`Self::resolve`] before using requests.
///
/// ```
/// let config: groove_core::experimental::ExperimentalConfig =
///     toml::from_str("features = []")?;
/// config.validate()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentalConfig {
    /// Individual registered names; duplicates in one source are invalid.
    pub features: Vec<String>,
}

impl ExperimentalConfig {
    /// Whether this source contains no opt-ins. Used to preserve legacy output.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Validate requests without executing anything or changing the input file.
    ///
    /// # Errors
    /// Malformed or duplicate names are invalid arguments. Unknown and removed names produce
    /// [`ErrorCode::UnsupportedConfiguration`] with upgrade/removal guidance.
    pub fn validate(&self) -> Result<()> {
        self.resolve().map(|_| ())
    }

    /// Resolve this source plus all stabilized features into runtime capabilities.
    ///
    /// # Errors
    /// Returns the same configuration errors as [`Self::validate`].
    pub fn resolve(&self) -> Result<FeatureSet> {
        resolve(&[self], FEATURES)
    }

    /// Resolve the additive union of user and project opt-ins.
    ///
    /// Listing a feature in both sources is valid. Each source is validated
    /// independently; neither can hide an invalid request in the other.
    ///
    /// # Errors
    /// Returns the same configuration errors as [`Self::validate`].
    pub fn resolve_with(&self, other: &Self) -> Result<FeatureSet> {
        resolve(&[self, other], FEATURES)
    }
}

/// Validated runtime capabilities. Constructed only by configuration resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureSet {
    enabled: BTreeSet<&'static str>,
}

impl FeatureSet {
    /// Test a registered feature definition at an implementation gate.
    ///
    /// Stabilized features are enabled even when absent from configuration.
    /// Unknown definitions are never enabled.
    #[must_use]
    pub fn is_enabled(&self, feature: &Feature) -> bool {
        self.enabled.contains(feature.name)
    }
}

fn resolve(configs: &[&ExperimentalConfig], registry: &[Feature]) -> Result<FeatureSet> {
    let mut enabled = BTreeSet::new();
    for config in configs {
        let mut seen = BTreeSet::new();
        for name in &config.features {
            if name.is_empty()
                || name.len() > 64
                || !name.as_bytes()[0].is_ascii_lowercase()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Experimental feature names must contain 1-64 lowercase ASCII \
                     letters, digits or hyphens, starting with a letter",
                ));
            }
            if !seen.insert(name) {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    format!("Experimental feature specified twice: {name}"),
                ));
            }
        }
        for name in &config.features {
            let feature = registry
                .iter()
                .find(|feature| feature.name == name)
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::UnsupportedConfiguration,
                        format!(
                            "Unknown experimental feature: {name}; use a binary \
                                 that supports it or remove its experimental.features entry"
                        ),
                    )
                })?;
            match feature.status {
                FeatureStatus::Experimental | FeatureStatus::Stabilized { .. } => {
                    enabled.insert(feature.name);
                }
                FeatureStatus::Removed { since, reason } => {
                    return Err(Error::new(
                        ErrorCode::UnsupportedConfiguration,
                        format!(
                            "Experimental feature {name} was removed in {since}: {reason}; \
                                 remove its experimental.features entry and review the \
                                 replacement before continuing"
                        ),
                    ));
                }
            }
        }
    }
    for feature in registry {
        if matches!(feature.status, FeatureStatus::Stabilized { .. }) {
            enabled.insert(feature.name);
        }
    }
    Ok(FeatureSet { enabled })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE: Feature = Feature {
        name: "fixture-feature",
        introduced_in: "0.1.0",
        status: FeatureStatus::Experimental,
    };

    #[test]
    fn graduation_keeps_the_original_file_valid_and_enables_the_default() {
        let requested: ExperimentalConfig =
            toml::from_str(include_str!("../tests/fixtures/experimental/opt-in.toml")).unwrap();
        let absent = ExperimentalConfig::default();
        let stable = Feature {
            status: FeatureStatus::Stabilized { since: "0.2.0" },
            ..ACTIVE
        };
        let active_default = resolve(&[&absent], &[ACTIVE]).unwrap();
        let active_opt_in = resolve(&[&requested], &[ACTIVE]).unwrap();
        let stable_default = resolve(&[&absent], &[stable]).unwrap();
        let stable_legacy = resolve(&[&requested], &[stable]).unwrap();
        let actual = serde_json::json!({
            "experimental_default": active_default.enabled,
            "experimental_opt_in": active_opt_in.enabled,
            "stabilized_default": stable_default.enabled,
            "stabilized_legacy_opt_in": stable_legacy.enabled,
        });
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/experimental/graduation.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
        assert!(!active_default.is_enabled(&ACTIVE));
        assert!(active_opt_in.is_enabled(&ACTIVE));
        assert!(stable_default.is_enabled(&stable));
        assert_eq!(stable_default, stable_legacy);
    }

    #[test]
    fn unsupported_requests_and_duplicates_fail_without_enabling_a_fallback() {
        let requested = ExperimentalConfig {
            features: vec![ACTIVE.name.into()],
        };
        let removed = Feature {
            status: FeatureStatus::Removed {
                since: "0.3.0",
                reason: "Use the stable replacement",
            },
            ..ACTIVE
        };
        let unknown = resolve(&[&requested], &[]).unwrap_err();
        assert_eq!(unknown.code, ErrorCode::UnsupportedConfiguration);
        assert!(unknown.message.contains(ACTIVE.name));
        let removed_error = resolve(&[&requested], &[removed]).unwrap_err();
        assert_eq!(removed_error.code, ErrorCode::UnsupportedConfiguration);
        assert!(removed_error.message.contains("0.3.0"));
        assert!(removed_error.message.contains("Use the stable replacement"));
        assert!(!resolve(&[&ExperimentalConfig::default()], &[removed])
            .unwrap()
            .is_enabled(&removed));
        let duplicate = ExperimentalConfig {
            features: vec![ACTIVE.name.into(), ACTIVE.name.into()],
        };
        assert_eq!(
            resolve(&[&duplicate], &[ACTIVE]).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn user_and_project_requests_are_additive_and_both_sources_are_validated() {
        let requested = ExperimentalConfig {
            features: vec![ACTIVE.name.into()],
        };
        let absent = ExperimentalConfig::default();
        for sources in [
            [&requested, &absent],
            [&absent, &requested],
            [&requested, &requested],
        ] {
            assert!(resolve(&sources, &[ACTIVE]).unwrap().is_enabled(&ACTIVE));
        }
        let unknown = ExperimentalConfig {
            features: vec!["future-feature".into()],
        };
        for sources in [[&requested, &unknown], [&unknown, &requested]] {
            assert_eq!(
                resolve(&sources, &[ACTIVE]).unwrap_err().code,
                ErrorCode::UnsupportedConfiguration
            );
        }
    }
}
