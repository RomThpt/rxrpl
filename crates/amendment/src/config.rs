use std::collections::HashSet;

use serde::Deserialize;

use crate::error::AmendmentError;
use crate::presets;
use crate::registry::FeatureRegistry;
use crate::table::AmendmentTable;

/// Per-feature amendment voting configuration loaded from TOML.
///
/// Two mutually-exclusive ways to configure votes:
/// - `compatibility`: a named preset that locks votes to the values shipped
///   by a specific rippled release. Used for mixed-validator topologies where
///   exact vote-for-vote agreement is required to converge `account_hash` at
///   flag ledgers.
/// - `vote` / `veto`: manual lists of amendment names to support or oppose.
///   Each name must match a known amendment in [`FeatureRegistry`].
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AmendmentConfig {
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default)]
    pub vote: Vec<String>,
    #[serde(default)]
    pub veto: Vec<String>,
}

impl AmendmentConfig {
    /// Apply the configuration to the given registry and table.
    ///
    /// When `compatibility` is set, looks up the preset and sets each
    /// amendment's `supported` flag to the preset value. Otherwise, applies
    /// the manual `vote` / `veto` lists: names in `vote` get `set_supported(true)`,
    /// names in `veto` get `set_supported(false)`.
    ///
    /// Errors:
    /// - `compatibility` and (`vote` or `veto`) both set
    /// - unknown preset name
    /// - unknown amendment name in `vote` or `veto`
    /// - the same amendment listed in both `vote` and `veto`
    pub fn apply(
        &self,
        registry: &FeatureRegistry,
        table: &mut AmendmentTable,
    ) -> Result<(), AmendmentError> {
        let manual_used = !self.vote.is_empty() || !self.veto.is_empty();
        if self.compatibility.is_some() && manual_used {
            return Err(AmendmentError::ConfigConflict);
        }

        if let Some(preset_name) = &self.compatibility {
            let preset = presets::lookup(preset_name)
                .ok_or_else(|| AmendmentError::UnknownPreset(preset_name.clone()))?;
            for (name, vote) in preset {
                let id = registry
                    .id_for_name(name)
                    .ok_or_else(|| AmendmentError::UnknownAmendment(name.to_string()))?;
                table.set_supported(&id, *vote);
            }
            return Ok(());
        }

        let mut seen: HashSet<&String> = HashSet::new();
        for name in &self.vote {
            if !seen.insert(name) {
                return Err(AmendmentError::DuplicateAmendment(name.clone()));
            }
            let id = registry
                .id_for_name(name)
                .ok_or_else(|| AmendmentError::UnknownAmendment(name.clone()))?;
            table.set_supported(&id, true);
        }
        for name in &self.veto {
            if !seen.insert(name) {
                return Err(AmendmentError::DuplicateAmendment(name.clone()));
            }
            let id = registry
                .id_for_name(name)
                .ok_or_else(|| AmendmentError::UnknownAmendment(name.clone()))?;
            table.set_supported(&id, false);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (FeatureRegistry, AmendmentTable) {
        let reg = FeatureRegistry::with_known_amendments();
        let table = AmendmentTable::new(&reg, 14 * 24 * 60 * 4);
        (reg, table)
    }

    #[test]
    fn default_is_no_op() {
        let (reg, mut table) = fresh();
        let before: Vec<_> = table.get_votes();
        AmendmentConfig::default().apply(&reg, &mut table).unwrap();
        let after: Vec<_> = table.get_votes();
        assert_eq!(before, after);
    }

    #[test]
    fn manual_vote_and_veto() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: None,
            vote: vec!["OwnerPaysFee".into()],
            veto: vec!["AMM".into()],
        };
        cfg.apply(&reg, &mut table).unwrap();
        let owner_id = reg.id_for_name("OwnerPaysFee").unwrap();
        let amm_id = reg.id_for_name("AMM").unwrap();
        assert!(table.is_supported(&owner_id));
        assert!(!table.is_supported(&amm_id));
    }

    #[test]
    fn unknown_amendment_errors() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: None,
            vote: vec!["NotARealAmendment".into()],
            veto: vec![],
        };
        let err = cfg.apply(&reg, &mut table).unwrap_err();
        assert!(matches!(err, AmendmentError::UnknownAmendment(_)));
    }

    #[test]
    fn compatibility_and_manual_conflict() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: Some("rippled-2.6.2".into()),
            vote: vec!["AMM".into()],
            veto: vec![],
        };
        let err = cfg.apply(&reg, &mut table).unwrap_err();
        assert!(matches!(err, AmendmentError::ConfigConflict));
    }

    #[test]
    fn unknown_preset_errors() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: Some("rippled-99.0.0".into()),
            vote: vec![],
            veto: vec![],
        };
        let err = cfg.apply(&reg, &mut table).unwrap_err();
        assert!(matches!(err, AmendmentError::UnknownPreset(_)));
    }

    #[test]
    fn duplicate_in_vote_and_veto_errors() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: None,
            vote: vec!["AMM".into()],
            veto: vec!["AMM".into()],
        };
        let err = cfg.apply(&reg, &mut table).unwrap_err();
        assert!(matches!(err, AmendmentError::DuplicateAmendment(_)));
    }

    #[test]
    fn compatibility_preset_applies() {
        let (reg, mut table) = fresh();
        let cfg = AmendmentConfig {
            compatibility: Some("rippled-2.6.2".into()),
            vote: vec![],
            veto: vec![],
        };
        cfg.apply(&reg, &mut table).unwrap();
        for (name, expected_vote) in presets::lookup("rippled-2.6.2").unwrap() {
            let id = reg.id_for_name(name).unwrap();
            assert_eq!(
                table.is_supported(&id),
                *expected_vote,
                "amendment {name} vote mismatch after preset apply"
            );
        }
    }
}
