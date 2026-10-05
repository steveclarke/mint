//! Named rule sets: built-ins plus the user's `presets.toml`.
//!
//! ```toml
//! [moneris]
//! description = "Moneris merchant portal"
//! length = "10-16"          # or a number
//! classes = ["upper", "lower", "digits", "symbols"]
//! require = 3
//! symbols = "!@#$"          # allowed symbol set
//! no_ambiguous = false
//!
//! [bank-pin]
//! pin = 4
//!
//! [memorable]
//! words = 6
//! separator = "."
//! capitalize = true
//! digit = true
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;
use crate::rule::{CharClass, Kind, LengthSpec, Rule};

/// One preset as written in TOML. Every field is optional; unset fields keep
/// mint's defaults.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PresetSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<LengthValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ambiguous: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capitalize: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<usize>,
}

/// `length = 24` or `length = "10-16"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum LengthValue {
    Number(usize),
    Text(String),
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Builtin,
    User,
}

#[derive(Serialize, Clone, Debug)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub source: Source,
    /// What the preset produces, in words.
    pub summary: String,
    #[serde(skip)]
    pub spec: PresetSpec,
}

impl PresetSpec {
    /// Applies this preset on top of `rule`.
    pub fn apply(&self, rule: &mut Rule) -> Result<()> {
        if let Some(n) = self.pin {
            rule.kind = Kind::Pin;
            rule.length = n;
            rule.length_min = None;
            return Ok(());
        }
        if let Some(n) = self.words {
            rule.kind = Kind::Words;
            rule.words = n;
        }
        if let Some(sep) = &self.separator {
            rule.separator = sep.clone();
        }
        if let Some(c) = self.capitalize {
            rule.capitalize = c;
        }
        if let Some(d) = self.digit {
            rule.word_digit = d;
        }
        if let Some(length) = &self.length {
            let spec = match length {
                LengthValue::Number(n) => LengthSpec::parse(&n.to_string())?,
                LengthValue::Text(t) => LengthSpec::parse(t)?,
            };
            rule.set_length(spec);
        }
        if let Some(classes) = &self.classes {
            for c in CharClass::ALL {
                rule.set_class(c, false);
            }
            for name in classes {
                let class = CharClass::parse(name).ok_or_else(|| {
                    Error::Usage(format!("Unknown class \"{name}\" in a preset; use upper, lower, digits or symbols."))
                })?;
                rule.set_class(class, true);
            }
        }
        if let Some(n) = self.require {
            rule.require = Some(n);
        }
        if let Some(s) = &self.symbols {
            rule.symbol_set = s.clone();
        }
        if let Some(a) = self.no_ambiguous {
            rule.no_ambiguous = a;
        }
        Ok(())
    }

    pub fn to_rule(&self, name: &str) -> Result<Rule> {
        let mut rule = Rule::default();
        self.apply(&mut rule)?;
        rule.preset = Some(name.to_string());
        Ok(rule)
    }
}

fn builtin_specs() -> Vec<(&'static str, PresetSpec)> {
    let all = || Some(vec!["upper".into(), "lower".into(), "digits".into(), "symbols".into()]);
    vec![
        (
            "moneris",
            PresetSpec {
                description: Some("Moneris: 10 to 16 characters, 3 of 4 types".into()),
                length: Some(LengthValue::Text("10-16".into())),
                classes: all(),
                require: Some(3),
                ..Default::default()
            },
        ),
        (
            "alnum",
            PresetSpec {
                description: Some("Letters and digits only, for sites that reject symbols".into()),
                length: Some(LengthValue::Number(24)),
                classes: Some(vec!["upper".into(), "lower".into(), "digits".into()]),
                ..Default::default()
            },
        ),
        (
            "wifi",
            PresetSpec {
                description: Some("Wi-Fi (WPA2/WPA3): 63 characters, nothing ambiguous to type".into()),
                length: Some(LengthValue::Number(63)),
                classes: all(),
                no_ambiguous: Some(true),
                ..Default::default()
            },
        ),
        (
            "words",
            PresetSpec {
                description: Some("Memorable passphrase: 5 words, capitalized, one digit".into()),
                words: Some(5),
                separator: Some("-".into()),
                capitalize: Some(true),
                digit: Some(true),
                ..Default::default()
            },
        ),
        ("pin4", PresetSpec { description: Some("4-digit PIN".into()), pin: Some(4), ..Default::default() }),
        ("pin6", PresetSpec { description: Some("6-digit PIN".into()), pin: Some(6), ..Default::default() }),
    ]
}

/// Parses a presets file's text. `origin` names the file in error messages.
pub fn parse_user_presets(text: &str, origin: &str) -> Result<BTreeMap<String, PresetSpec>> {
    toml::from_str::<BTreeMap<String, PresetSpec>>(text).map_err(|e| {
        let detail = e.message().trim().trim_end_matches('.').to_string();
        let line = e.span().map(|s| text[..s.start].matches('\n').count() + 1);
        match line {
            Some(l) => Error::Usage(format!("{origin} line {l}: {detail}; fix the file and try again.")),
            None => Error::Usage(format!("{origin}: {detail}; fix the file and try again.")),
        }
    })
}

fn load_user_presets(path: &Path) -> Result<BTreeMap<String, PresetSpec>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_user_presets(&text, &path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(Error::Usage(format!("Cannot read {}: {e}; check its permissions.", path.display()))),
    }
}

/// Built-ins followed by user presets. A user preset with a built-in's name
/// replaces the built-in.
pub fn all() -> Result<Vec<Preset>> {
    let user = match paths::presets_file() {
        Some(p) => load_user_presets(&p)?,
        None => BTreeMap::new(),
    };
    let mut out = Vec::new();
    for (name, spec) in builtin_specs() {
        if !user.contains_key(name) {
            out.push(make(name, spec, Source::Builtin)?);
        }
    }
    for (name, spec) in user {
        out.push(make(&name, spec, Source::User)?);
    }
    Ok(out)
}

fn make(name: &str, spec: PresetSpec, source: Source) -> Result<Preset> {
    let rule = spec.to_rule(name).map_err(|e| Error::Usage(format!("Preset \"{name}\": {}", e.message())))?;
    Ok(Preset {
        name: name.to_string(),
        description: spec.description.clone().unwrap_or_else(|| rule.summary()),
        source,
        summary: rule.summary(),
        spec,
    })
}

pub fn find(name: &str) -> Result<Preset> {
    let presets = all()?;
    let wanted = name.trim().to_ascii_lowercase();
    presets
        .into_iter()
        .find(|p| p.name.to_ascii_lowercase() == wanted)
        .ok_or_else(|| Error::Usage(format!("There is no preset named \"{name}\"; run `mint presets` to list them.")))
}

#[cfg(test)]
pub(crate) fn builtins_for_test() -> Vec<(&'static str, PresetSpec)> {
    builtin_specs()
}
