//! A person's saved desktop (`users/<name>/desktop.json`, ADR-045 4): which settings may be in it, one rule
//! for torqcli, which leaves the rest out, and the broker, which refuses them, so the two cannot drift.

/// COSMIC's own components, the only ones kept: an applet or program from anyone else keeps what it likes in
/// its folder under `~/.config/cosmic` (a location, a token, a time it last looked), none of it a setting of
/// the person's desktop, and the repository it would go to is readable by every account.
pub const COSMIC: &str = "com.system76.";

/// A setting's name as COSMIC lays it out, `<component>/v<n>/<key>`, of one of COSMIC's own components: never
/// a path anywhere else, since a person's session writes it under their ~/.config/cosmic.
pub fn key_ok(k: &str) -> bool {
    let word = |s: &str| !s.is_empty() && s.len() <= 128 && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let mut parts = k.split('/');
    let (Some(component), Some(version), Some(key), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return false };
    let dotted: Vec<&str> = component.split('.').collect();
    component.starts_with(COSMIC)
        && dotted.len() >= 3
        && dotted.iter().all(|p| word(p))
        && version.strip_prefix('v').is_some_and(|n| !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()))
        && word(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_is_cosmics_own_and_never_a_path() {
        assert!(key_ok("com.system76.CosmicPanel.Panel/v1/size"));
        assert!(key_ok("com.system76.CosmicBackground/v1/same-on-all"));
        for bad in [
            "", "a/v1/b", "com.x/1/b", "com.x/v1", "com.x/v1/b/c", "../x/v1/b", "com.x/v1/..", "com..x/v1/b", "com.x/v1/-b", "com.x/v12345/b",
            "com.x/v1/a b", "com.vintagetechie.CosmicExtAppletTempest/v1/latitude", "com.system76/v1/a", "com.system76x.Cosmic/v1/a",
            "org.system76.CosmicPanel/v1/a", "com.system76./v1/a", "com.system76.CosmicPanel/v1/../a",
        ] {
            assert!(!key_ok(bad), "{bad}");
        }
    }
}
