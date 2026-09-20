//! Command-line argument templating.
//!
//! A version manifest does not contain a command line - it contains a recipe.
//! Arguments are conditional on platform and on launcher features, and carry
//! `${placeholder}` slots the launcher fills in.
//!
//! Two format generations, both still in use:
//!
//! - **Modern** (1.13+): `arguments.game` and `arguments.jvm`, arrays whose
//!   entries are either a plain string or `{rules, value}`. `value` is itself
//!   either a string or an array of strings - the macOS JVM entry in 1.21.11
//!   uses an array while the Windows one beside it uses a string, so both
//!   shapes have to work.
//! - **Legacy** (pre-1.13): `minecraftArguments`, one space-separated string
//!   with no rules at all.
//!
//! An unfilled placeholder is treated as an error rather than passed through.
//! A game launched with a literal `${classpath}` on its command line fails in a
//! way that takes an hour to diagnose; failing here names the missing value.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::Deserializer;
use serde::Deserialize;

use crate::platform::Platform;
use crate::rules::{evaluate, Features, Rule};

/// One entry from `arguments.game` or `arguments.jvm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argument {
    /// An unconditional argument.
    Plain(String),
    /// Included only when its rules allow. May expand to several arguments.
    Conditional {
        rules: Vec<Rule>,
        values: Vec<String>,
    },
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Plain(String),
            Conditional {
                rules: Vec<Rule>,
                value: StringOrList,
            },
        }

        match Raw::deserialize(deserializer)? {
            Raw::Plain(text) => Ok(Self::Plain(text)),
            Raw::Conditional { rules, value } => Ok(Self::Conditional {
                rules,
                values: value.into_vec(),
            }),
        }
    }
}

/// `value` may be a single string or an array of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl StringOrList {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(text) => vec![text],
            Self::Many(list) => list,
        }
    }
}

/// The values that fill `${placeholder}` slots.
#[derive(Debug, Clone, Default)]
pub struct Substitutions(BTreeMap<String, String>);

impl Substitutions {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(name.into(), value.into());
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

/// A placeholder with no value supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedPlaceholders(pub Vec<String>);

impl fmt::Display for UnresolvedPlaceholders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no value supplied for {}",
            self.0
                .iter()
                .map(|name| format!("${{{name}}}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnresolvedPlaceholders {}

/// Expand `${name}` occurrences, collecting any that have no value.
///
/// Collects rather than failing on the first, so one pass reports every missing
/// value instead of revealing them one launch attempt at a time.
fn substitute(template: &str, subs: &Substitutions, missing: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];

        let Some(end) = after.find('}') else {
            // An unterminated `${` is literal text, not a placeholder.
            out.push_str(&rest[start..]);
            return out;
        };

        let name = &after[..end];
        match subs.get(name) {
            Some(value) => out.push_str(value),
            None => {
                if !missing.iter().any(|m| m == name) {
                    missing.push(name.to_owned());
                }
                // Keep the placeholder so the failure message can show context.
                out.push_str("${");
                out.push_str(name);
                out.push('}');
            }
        }

        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
}

/// Build the final argument vector.
///
/// Arguments are produced as a vector, never joined into a string: the game
/// directory and player name can contain spaces and quotes, and re-splitting a
/// joined command line is where launchers acquire injection bugs.
pub fn resolve(
    arguments: &[Argument],
    platform: &Platform,
    features: &Features,
    subs: &Substitutions,
) -> Result<Vec<String>, UnresolvedPlaceholders> {
    let mut out = Vec::new();
    let mut missing = Vec::new();

    for argument in arguments {
        match argument {
            Argument::Plain(text) => out.push(substitute(text, subs, &mut missing)),
            Argument::Conditional { rules, values } => {
                if evaluate(rules, platform, features) {
                    for value in values {
                        out.push(substitute(value, subs, &mut missing));
                    }
                }
            }
        }
    }

    if missing.is_empty() {
        Ok(out)
    } else {
        Err(UnresolvedPlaceholders(missing))
    }
}

/// Expand a legacy `minecraftArguments` string.
///
/// Pre-1.13 versions have no rules and no arrays - just one space-separated
/// line. Splitting on whitespace is correct here because the *template* never
/// contains spaces within a token; the substituted values may, which is exactly
/// why substitution happens after the split rather than before.
pub fn resolve_legacy(
    template: &str,
    subs: &Substitutions,
) -> Result<Vec<String>, UnresolvedPlaceholders> {
    let mut missing = Vec::new();
    let out: Vec<String> = template
        .split_whitespace()
        .map(|token| substitute(token, subs, &mut missing))
        .collect();

    if missing.is_empty() {
        Ok(out)
    } else {
        Err(UnresolvedPlaceholders(missing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Arch, Os};

    fn windows() -> Platform {
        Platform::new(Os::Windows, Arch::X86_64, "10.0.26200")
    }
    fn macos() -> Platform {
        Platform::new(Os::Osx, Arch::Arm64, "14.5")
    }

    fn parse(json: &str) -> Vec<Argument> {
        serde_json::from_str(json).expect("argument JSON should parse")
    }

    fn base_subs() -> Substitutions {
        Substitutions::new()
            .with("auth_player_name", "Notch")
            .with("version_name", "1.21.11")
            .with("classpath", "a.jar;b.jar")
            .with("natives_directory", "C:\\inst\\natives")
    }

    #[test]
    fn plain_arguments_pass_through_in_order() {
        let args = parse(r#"["--username","${auth_player_name}","--version","${version_name}"]"#);
        let got = resolve(&args, &windows(), &Features::new(), &base_subs()).unwrap();
        assert_eq!(got, ["--username", "Notch", "--version", "1.21.11"]);
    }

    /// Lifted from 1.21.11: the macOS entry's `value` is an array while the
    /// Windows entry beside it is a plain string. Both must work.
    #[test]
    fn value_may_be_a_string_or_an_array() {
        let args = parse(
            r#"[
                {"rules":[{"action":"allow","os":{"name":"osx"}}],"value":["-XstartOnFirstThread"]},
                {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":"-XX:HeapDumpPath=Mojang"}
            ]"#,
        );

        let on_mac = resolve(&args, &macos(), &Features::new(), &base_subs()).unwrap();
        assert_eq!(on_mac, ["-XstartOnFirstThread"]);

        let on_win = resolve(&args, &windows(), &Features::new(), &base_subs()).unwrap();
        assert_eq!(on_win, ["-XX:HeapDumpPath=Mojang"]);
    }

    #[test]
    fn conditional_arguments_respect_features() {
        let args = parse(
            r#"[{"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"}]"#,
        );

        assert!(resolve(&args, &windows(), &Features::new(), &base_subs())
            .unwrap()
            .is_empty());

        let demo = Features::new().with("is_demo_user", true);
        assert_eq!(
            resolve(&args, &windows(), &demo, &base_subs()).unwrap(),
            ["--demo"]
        );
    }

    #[test]
    fn placeholders_embedded_in_a_larger_token_are_expanded() {
        let args = parse(r#"["-Djava.library.path=${natives_directory}"]"#);
        let got = resolve(&args, &windows(), &Features::new(), &base_subs()).unwrap();
        assert_eq!(got, ["-Djava.library.path=C:\\inst\\natives"]);
    }

    #[test]
    fn several_placeholders_in_one_token() {
        let subs = Substitutions::new().with("a", "1").with("b", "2");
        let args = parse(r#"["x=${a},y=${b}"]"#);
        let got = resolve(&args, &windows(), &Features::new(), &subs).unwrap();
        assert_eq!(got, ["x=1,y=2"]);
    }

    /// The important one. A literal `${classpath}` reaching the JVM fails in a
    /// way that takes an hour to diagnose.
    #[test]
    fn a_missing_value_is_an_error_not_a_literal_placeholder() {
        let args = parse(r#"["-cp","${classpath}","${auth_uuid}"]"#);
        let err = resolve(&args, &windows(), &Features::new(), &Substitutions::new())
            .expect_err("missing values must fail");

        assert!(err.0.contains(&"classpath".to_owned()));
        assert!(err.0.contains(&"auth_uuid".to_owned()));
        assert!(err.to_string().contains("${classpath}"));
    }

    /// Every missing value is reported in one pass, not one launch at a time.
    #[test]
    fn all_missing_values_are_reported_together() {
        let args = parse(r#"["${one}","${two}","${three}"]"#);
        let err = resolve(&args, &windows(), &Features::new(), &Substitutions::new()).unwrap_err();
        assert_eq!(err.0.len(), 3);
    }

    /// A placeholder inside an argument that gets filtered out is not missing -
    /// it is simply never used.
    #[test]
    fn placeholders_in_excluded_arguments_are_not_required() {
        let args = parse(
            r#"[{"rules":[{"action":"allow","os":{"name":"osx"}}],"value":"${mac_only_thing}"}]"#,
        );
        let got = resolve(&args, &windows(), &Features::new(), &Substitutions::new()).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn substituted_values_may_contain_spaces() {
        let subs = Substitutions::new().with("game_directory", r"C:\Users\A B\instance one");
        let args = parse(r#"["--gameDir","${game_directory}"]"#);
        let got = resolve(&args, &windows(), &Features::new(), &subs).unwrap();
        assert_eq!(got, ["--gameDir", r"C:\Users\A B\instance one"]);
        assert_eq!(got.len(), 2, "a space must not split one argument into two");
    }

    #[test]
    fn an_unterminated_placeholder_is_literal_text() {
        let args = parse(r#"["cost is ${100"]"#);
        let got = resolve(&args, &windows(), &Features::new(), &Substitutions::new()).unwrap();
        assert_eq!(got, ["cost is ${100"]);
    }

    /// Pre-1.13 shape: one space-separated line, no rules.
    #[test]
    fn legacy_argument_string_is_split_then_substituted() {
        let subs = Substitutions::new()
            .with("auth_player_name", "Notch")
            .with("game_directory", r"C:\Users\A B\mc");
        let got = resolve_legacy(
            "--username ${auth_player_name} --gameDir ${game_directory}",
            &subs,
        )
        .unwrap();
        assert_eq!(
            got,
            ["--username", "Notch", "--gameDir", r"C:\Users\A B\mc"]
        );
    }

    #[test]
    fn legacy_reports_missing_values_too() {
        let err = resolve_legacy("--session ${auth_session}", &Substitutions::new()).unwrap_err();
        assert_eq!(err.0, ["auth_session"]);
    }
}
