//! Manifest rule evaluation.
//!
//! Minecraft version manifests attach `rules` to libraries and to individual
//! command-line arguments. Getting this wrong does not produce an error - it
//! produces a game that is missing a native library on one platform, or that
//! passes a demo-mode flag to a paying customer. So the semantics are worth
//! stating exactly:
//!
//! 1. **No rules at all means allowed.** This is the common case.
//! 2. **Any rules at all means denied by default.** An entry with rules that
//!    all fail to match is excluded, not included.
//! 3. **Last matching rule wins.** Rules are evaluated in order and each match
//!    overwrites the result; they are not short-circuited.
//! 4. **A rule with no conditions matches everything.** `{"action":"allow"}`
//!    on its own is how a manifest says "allowed, except where a later rule
//!    says otherwise".
//!
//! That combination is what makes the extremely common pair work:
//!
//! ```json
//! [{"action": "allow"}, {"action": "disallow", "os": {"name": "osx"}}]
//! ```
//!
//! allow-everything, then take macOS back out again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::platform::Platform;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    #[default]
    Allow,
    Disallow,
}

/// Conditions on the operating system. Every field present must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A **regular expression** matched against the OS version, not a literal.
    /// Real manifests use values like `^10\\.` to mean "Windows 10 or later".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsCondition>,
    /// Launcher-supplied flags such as `is_demo_user` or
    /// `has_custom_resolution`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<BTreeMap<String, bool>>,
}

/// Which optional launcher features are switched on.
///
/// Absent means false. A manifest rule asking for `is_demo_user: false` is
/// satisfied by a feature that was never set, which is why lookup defaults
/// rather than erroring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features(BTreeMap<String, bool>);

impl Features {
    pub fn new() -> Self {
        Self::default()
    }

    /// Turn a feature on (or explicitly off).
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, enabled: bool) -> Self {
        self.0.insert(name.into(), enabled);
        self
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.0.get(name).copied().unwrap_or(false)
    }
}

impl Rule {
    /// Whether this rule's conditions hold. A rule with no conditions always
    /// holds - that is how `{"action":"allow"}` works.
    fn matches(&self, platform: &Platform, features: &Features) -> bool {
        if let Some(os) = &self.os {
            if !os_matches(os, platform) {
                return false;
            }
        }

        if let Some(required) = &self.features {
            if !required
                .iter()
                .all(|(name, expected)| features.is_enabled(name) == *expected)
            {
                return false;
            }
        }

        true
    }
}

fn os_matches(condition: &OsCondition, platform: &Platform) -> bool {
    if let Some(name) = &condition.name {
        if name != platform.os.manifest_name() {
            return false;
        }
    }

    if let Some(arch) = &condition.arch {
        // Accept the aliases manifests actually use (amd64, aarch64, ...)
        // rather than only the canonical spelling.
        match crate::platform::Arch::from_manifest_name(arch) {
            Some(wanted) if wanted == platform.arch => {}
            // An architecture we do not recognise cannot match. Treating it as
            // a match would pull in libraries for the wrong CPU.
            _ => return false,
        }
    }

    if let Some(pattern) = &condition.version {
        return version_matches(pattern, &platform.version);
    }

    true
}

/// Match an `os.version` regex.
///
/// An invalid pattern is treated as not matching rather than as an error. A
/// malformed rule in one library should exclude that library, not abort
/// resolution of the whole version - and it must certainly not panic.
fn version_matches(pattern: &str, version: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(version))
}

/// Evaluate a rule list.
///
/// See the module docs for the exact semantics; the short version is "no rules
/// means yes, any rules means no unless something says otherwise, last match
/// wins".
pub fn evaluate(rules: &[Rule], platform: &Platform, features: &Features) -> bool {
    if rules.is_empty() {
        return true;
    }

    let mut allowed = false;
    for rule in rules {
        if rule.matches(platform, features) {
            allowed = rule.action == Action::Allow;
        }
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Arch, Os};

    fn windows() -> Platform {
        Platform::new(Os::Windows, Arch::X86_64, "10.0.26200")
    }
    fn linux() -> Platform {
        Platform::new(Os::Linux, Arch::X86_64, "6.8.0")
    }
    fn macos() -> Platform {
        Platform::new(Os::Osx, Arch::Arm64, "14.5")
    }

    fn parse(json: &str) -> Vec<Rule> {
        serde_json::from_str(json).expect("test rule JSON should parse")
    }

    #[test]
    fn no_rules_means_allowed() {
        assert!(evaluate(&[], &windows(), &Features::new()));
    }

    /// Rule 2: the presence of rules flips the default to deny.
    #[test]
    fn rules_that_never_match_mean_denied() {
        let rules = parse(r#"[{"action":"allow","os":{"name":"osx"}}]"#);
        assert!(!evaluate(&rules, &windows(), &Features::new()));
        assert!(evaluate(&rules, &macos(), &Features::new()));
    }

    /// The single most common shape in real manifests.
    #[test]
    fn allow_all_then_disallow_one_os() {
        let rules = parse(r#"[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]"#);
        assert!(evaluate(&rules, &windows(), &Features::new()));
        assert!(evaluate(&rules, &linux(), &Features::new()));
        assert!(!evaluate(&rules, &macos(), &Features::new()));
    }

    /// Rule 3: later matches overwrite earlier ones, so order is significant.
    /// The same two rules reversed give the opposite answer.
    #[test]
    fn last_matching_rule_wins() {
        let deny_then_allow =
            parse(r#"[{"action":"disallow","os":{"name":"windows"}},{"action":"allow"}]"#);
        assert!(evaluate(&deny_then_allow, &windows(), &Features::new()));

        let allow_then_deny =
            parse(r#"[{"action":"allow"},{"action":"disallow","os":{"name":"windows"}}]"#);
        assert!(!evaluate(&allow_then_deny, &windows(), &Features::new()));
    }

    #[test]
    fn architecture_narrows_a_match() {
        let rules = parse(r#"[{"action":"allow","os":{"name":"windows","arch":"x86"}}]"#);
        assert!(!evaluate(&rules, &windows(), &Features::new()));

        let x86 = Platform::new(Os::Windows, Arch::X86, "10.0");
        assert!(evaluate(&rules, &x86, &Features::new()));
    }

    #[test]
    fn architecture_aliases_are_understood() {
        let rules = parse(r#"[{"action":"allow","os":{"arch":"amd64"}}]"#);
        assert!(evaluate(&rules, &windows(), &Features::new()));
    }

    /// An unrecognised architecture must not match. Matching it would pull in
    /// libraries built for a different CPU.
    #[test]
    fn unknown_architecture_never_matches() {
        let rules = parse(r#"[{"action":"allow","os":{"arch":"sparc"}}]"#);
        assert!(!evaluate(&rules, &windows(), &Features::new()));
        assert!(!evaluate(&rules, &linux(), &Features::new()));
    }

    /// `os.version` is a regex, not a literal prefix.
    #[test]
    fn os_version_is_matched_as_a_regex() {
        let rules = parse(r#"[{"action":"allow","os":{"name":"windows","version":"^10\\."}}]"#);
        assert!(evaluate(&rules, &windows(), &Features::new()));

        let win7 = Platform::new(Os::Windows, Arch::X86_64, "6.1.7601");
        assert!(!evaluate(&rules, &win7, &Features::new()));
    }

    /// A malformed regex excludes that one entry rather than aborting the whole
    /// version resolution - and must never panic.
    #[test]
    fn a_broken_version_regex_excludes_rather_than_explodes() {
        let rules = parse(r#"[{"action":"allow","os":{"version":"("}}]"#);
        assert!(!evaluate(&rules, &windows(), &Features::new()));
    }

    #[test]
    fn feature_flags_gate_arguments() {
        let rules = parse(r#"[{"action":"allow","features":{"is_demo_user":true}}]"#);
        assert!(!evaluate(&rules, &windows(), &Features::new()));
        assert!(evaluate(
            &rules,
            &windows(),
            &Features::new().with("is_demo_user", true)
        ));
    }

    /// A rule asking for a feature to be *off* is satisfied by one that was
    /// never set, which is how the resolution arguments are gated.
    #[test]
    fn an_unset_feature_counts_as_off() {
        let rules = parse(r#"[{"action":"allow","features":{"has_custom_resolution":false}}]"#);
        assert!(evaluate(&rules, &windows(), &Features::new()));
        assert!(!evaluate(
            &rules,
            &windows(),
            &Features::new().with("has_custom_resolution", true)
        ));
    }

    #[test]
    fn every_condition_in_one_rule_must_hold() {
        let rules = parse(
            r#"[{"action":"allow","os":{"name":"windows","arch":"x86_64"},"features":{"is_demo_user":true}}]"#,
        );
        assert!(!evaluate(&rules, &windows(), &Features::new()));
        assert!(!evaluate(
            &rules,
            &linux(),
            &Features::new().with("is_demo_user", true)
        ));
        assert!(evaluate(
            &rules,
            &windows(),
            &Features::new().with("is_demo_user", true)
        ));
    }

    /// Shape lifted from a real LWJGL 3 entry: allowed everywhere except a
    /// specific OS/arch combination.
    #[test]
    fn real_lwjgl_shaped_rules() {
        let rules = parse(
            r#"[{"action":"allow"},{"action":"disallow","os":{"name":"osx","arch":"arm64"}}]"#,
        );
        assert!(evaluate(&rules, &windows(), &Features::new()));
        assert!(evaluate(&rules, &linux(), &Features::new()));
        assert!(!evaluate(&rules, &macos(), &Features::new()));

        let intel_mac = Platform::new(Os::Osx, Arch::X86_64, "13.0");
        assert!(evaluate(&rules, &intel_mac, &Features::new()));
    }
}
