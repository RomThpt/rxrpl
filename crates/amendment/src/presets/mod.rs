//! Named amendment vote presets.
//!
//! Each preset maps an amendment name to whether the node should vote to
//! enable it. Presets are checked into source to ensure reproducible vote
//! tables across releases. Used to align rxrpl's amendment table with a
//! specific rippled version for mixed-validator topologies (see issue #76).
//!
//! To capture a preset from a live rippled, run `rippled feature` on the
//! target version and record the `vetoed` flag for each amendment.

pub mod rippled_2_6_2;

/// Returns the preset (name, vote) pairs for the given preset name, or `None`
/// if the name is not recognised.
pub fn lookup(name: &str) -> Option<&'static [(&'static str, bool)]> {
    match name {
        "rippled-2.6.2" => Some(rippled_2_6_2::PRESET),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_preset_resolves() {
        assert!(lookup("rippled-2.6.2").is_some());
    }

    #[test]
    fn unknown_preset_returns_none() {
        assert!(lookup("rippled-99.0.0").is_none());
    }
}
