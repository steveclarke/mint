//! Rules (what a password must look like) and generation.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::random::SecureRng;
use crate::wordlist;

/// Upper limit on characters, PIN digits and words: generous, but bounded so
/// a typo cannot ask for a gigabyte.
pub const MAX_LENGTH: usize = 4096;
pub const MAX_WORDS: usize = 512;
pub const DEFAULT_LENGTH: usize = 24;
pub const DEFAULT_WORDS: usize = 5;
/// Symbols accepted by almost every site: no quotes, backslash, angle
/// brackets, braces, spaces or characters that need escaping in URLs or shells
/// beyond the usual.
pub const DEFAULT_SYMBOLS: &str = "!@#$%^&*-_=+?";
/// Characters easily confused with one another in common fonts.
pub const AMBIGUOUS: &str = "0Oo1lI|";

const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGITS: &str = "0123456789";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum CharClass {
    Upper,
    Lower,
    Digits,
    Symbols,
}

impl CharClass {
    pub const ALL: [CharClass; 4] = [CharClass::Upper, CharClass::Lower, CharClass::Digits, CharClass::Symbols];

    pub fn name(self) -> &'static str {
        match self {
            CharClass::Upper => "upper",
            CharClass::Lower => "lower",
            CharClass::Digits => "digits",
            CharClass::Symbols => "symbols",
        }
    }

    pub fn parse(s: &str) -> Option<CharClass> {
        match s.trim().to_ascii_lowercase().as_str() {
            "upper" | "uppercase" | "u" => Some(CharClass::Upper),
            "lower" | "lowercase" | "l" => Some(CharClass::Lower),
            "digits" | "digit" | "numbers" | "d" => Some(CharClass::Digits),
            "symbols" | "symbol" | "s" => Some(CharClass::Symbols),
            _ => None,
        }
    }

    /// The class a character belongs to (anything not a letter or digit is a symbol).
    pub fn of(c: char) -> CharClass {
        if c.is_ascii_uppercase() {
            CharClass::Upper
        } else if c.is_ascii_lowercase() {
            CharClass::Lower
        } else if c.is_ascii_digit() {
            CharClass::Digits
        } else {
            CharClass::Symbols
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Random characters from the enabled classes.
    #[default]
    Chars,
    /// A passphrase of words from the EFF large wordlist.
    Words,
    /// Digits only.
    Pin,
}

/// A complete description of one password to generate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Rule {
    pub kind: Kind,
    /// Characters (`chars`) or digits (`pin`).
    pub length: usize,
    /// The bottom of a site's allowed range (`10-16`), kept for the summary.
    pub length_min: Option<usize>,
    pub upper: bool,
    pub lower: bool,
    pub digits: bool,
    pub symbols: bool,
    pub symbol_set: String,
    /// At least this many classes must be enabled; every enabled class appears.
    pub require: Option<usize>,
    pub no_ambiguous: bool,
    pub words: usize,
    pub separator: String,
    pub capitalize: bool,
    /// Adds one random digit to the end of one random word.
    pub word_digit: bool,
    /// The preset this rule came from, if any.
    pub preset: Option<String>,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            kind: Kind::Chars,
            length: DEFAULT_LENGTH,
            length_min: None,
            upper: true,
            lower: true,
            digits: true,
            symbols: true,
            symbol_set: DEFAULT_SYMBOLS.to_string(),
            require: None,
            no_ambiguous: false,
            words: DEFAULT_WORDS,
            separator: "-".to_string(),
            capitalize: false,
            word_digit: false,
            preset: None,
        }
    }
}

/// A parsed length: a single number, or a site's range where mint uses the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LengthSpec {
    pub min: Option<usize>,
    pub max: usize,
}

impl LengthSpec {
    pub fn parse(s: &str) -> Result<LengthSpec> {
        let s = s.trim();
        let bad = || {
            Error::Usage(format!(
                "Length \"{s}\" is not a number or a range; use a number like 24 or a range like 10-16."
            ))
        };
        let num = |part: &str| part.trim().parse::<usize>().map_err(|_| bad());
        let spec = match s.split_once(['-', '–', ':']) {
            Some((lo, hi)) => {
                let (lo, hi) = (num(lo)?, num(hi)?);
                if lo > hi {
                    return Err(Error::Usage(format!(
                        "Length range {lo}-{hi} runs backwards; write the smaller number first."
                    )));
                }
                LengthSpec { min: Some(lo), max: hi }
            }
            None => LengthSpec { min: None, max: num(s)? },
        };
        if spec.max == 0 || spec.max > MAX_LENGTH {
            return Err(Error::Usage(format!(
                "Length must be between 1 and {MAX_LENGTH}; choose a length in that range."
            )));
        }
        Ok(spec)
    }
}

/// A generated password plus what is known about it. `Debug` redacts the value.
pub struct Password {
    pub value: Zeroizing<String>,
    pub kind: Kind,
    /// Length in characters.
    pub length: usize,
    /// The classes present in the password, in a fixed order.
    pub classes: Vec<CharClass>,
    /// Conservative estimate of the entropy of the generation process.
    pub entropy_bits: f64,
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Password")
            .field("value", &"[redacted]")
            .field("kind", &self.kind)
            .field("length", &self.length)
            .field("classes", &self.classes)
            .field("entropy_bits", &self.entropy_bits)
            .finish()
    }
}

impl Rule {
    pub fn enabled_classes(&self) -> Vec<CharClass> {
        CharClass::ALL
            .into_iter()
            .filter(|c| match c {
                CharClass::Upper => self.upper,
                CharClass::Lower => self.lower,
                CharClass::Digits => self.digits,
                CharClass::Symbols => self.symbols,
            })
            .collect()
    }

    pub fn set_class(&mut self, class: CharClass, on: bool) {
        match class {
            CharClass::Upper => self.upper = on,
            CharClass::Lower => self.lower = on,
            CharClass::Digits => self.digits = on,
            CharClass::Symbols => self.symbols = on,
        }
    }

    pub fn set_length(&mut self, spec: LengthSpec) {
        self.length = spec.max;
        self.length_min = spec.min;
    }

    /// The character pool for each enabled class, after the symbol set and the
    /// ambiguous filter are applied. Errors if a pool ends up empty.
    pub fn pools(&self) -> Result<Vec<(CharClass, Vec<char>)>> {
        let symbols = validate_symbols(&self.symbol_set)?;
        let mut pools = Vec::new();
        for class in self.enabled_classes() {
            let source: Vec<char> = match class {
                CharClass::Upper => UPPER.chars().collect(),
                CharClass::Lower => LOWER.chars().collect(),
                CharClass::Digits => DIGITS.chars().collect(),
                CharClass::Symbols => symbols.clone(),
            };
            if source.is_empty() {
                return Err(Error::Unsatisfiable(
                    "Symbols are turned on but the symbol set is empty; give --symbols some characters or turn symbols off.".into(),
                ));
            }
            let pool: Vec<char> =
                source.into_iter().filter(|c| !(self.no_ambiguous && AMBIGUOUS.contains(*c))).collect();
            if pool.is_empty() {
                return Err(Error::Unsatisfiable(format!(
                    "No {} remain after removing ambiguous characters; turn that class off or allow ambiguous characters.",
                    class.name()
                )));
            }
            pools.push((class, pool));
        }
        Ok(pools)
    }

    /// Checks the rule can be satisfied, without generating anything.
    pub fn validate(&self) -> Result<()> {
        match self.kind {
            Kind::Chars => {
                check_range("Length", self.length, MAX_LENGTH)?;
                let pools = self.pools()?;
                let enabled = pools.len();
                if enabled == 0 {
                    return Err(Error::Unsatisfiable(
                        "Every character class is turned off; turn on at least one of upper, lower, digits or symbols."
                            .into(),
                    ));
                }
                if let Some(n) = self.require {
                    if n == 0 {
                        return Err(Error::Usage("--require 0 asks for nothing; use 1 to 4, or leave it out.".into()));
                    }
                    if n > 4 {
                        return Err(Error::Usage(format!(
                            "--require {n} asks for more than the four classes that exist; use 1 to 4."
                        )));
                    }
                    if n > enabled {
                        return Err(Error::Unsatisfiable(format!(
                            "The rule requires {n} classes but only {enabled} {} turned on; turn on more classes or lower --require.",
                            if enabled == 1 { "is" } else { "are" }
                        )));
                    }
                }
                if self.length < enabled {
                    return Err(Error::Unsatisfiable(format!(
                        "{} characters cannot hold one of each of the {enabled} enabled classes; use a length of at least {enabled} or turn a class off.",
                        self.length
                    )));
                }
                Ok(())
            }
            Kind::Pin => check_range("PIN length", self.length, MAX_LENGTH),
            Kind::Words => check_range("Word count", self.words, MAX_WORDS),
        }
    }

    /// Generates one password from the OS CSPRNG.
    pub fn generate(&self) -> Result<Password> {
        self.validate()?;
        let mut rng = SecureRng::new();
        let value = match self.kind {
            Kind::Chars => self.generate_chars(&mut rng)?,
            Kind::Pin => {
                let digits: Vec<char> = DIGITS.chars().collect();
                let mut out = Zeroizing::new(String::with_capacity(self.length));
                for _ in 0..self.length {
                    out.push(*rng.choose(&digits));
                }
                out
            }
            Kind::Words => self.generate_words(&mut rng),
        };
        let mut classes: Vec<CharClass> = value.chars().map(CharClass::of).collect();
        classes.sort();
        classes.dedup();
        Ok(Password {
            length: value.chars().count(),
            value,
            kind: self.kind,
            classes,
            entropy_bits: self.entropy_bits()?,
        })
    }

    fn generate_chars(&self, rng: &mut SecureRng) -> Result<Zeroizing<String>> {
        let pools = self.pools()?;
        let union: Vec<char> = pools.iter().flat_map(|(_, p)| p.iter().copied()).collect();
        let mut chars: Zeroizing<Vec<char>> = Zeroizing::new(Vec::with_capacity(self.length));
        // One from each enabled class first, so every class is guaranteed...
        for (_, pool) in &pools {
            chars.push(*rng.choose(pool));
        }
        // ...then the rest from the union, then shuffle so the guaranteed
        // characters are not always at the front.
        while chars.len() < self.length {
            chars.push(*rng.choose(&union));
        }
        rng.shuffle(&mut chars);
        Ok(Zeroizing::new(chars.iter().collect()))
    }

    fn generate_words(&self, rng: &mut SecureRng) -> Zeroizing<String> {
        let list = wordlist::words();
        let mut words: Vec<Zeroizing<String>> = (0..self.words)
            .map(|_| {
                let w = *rng.choose(list);
                Zeroizing::new(if self.capitalize { capitalize(w) } else { w.to_string() })
            })
            .collect();
        if self.word_digit {
            let digits: Vec<char> = DIGITS.chars().collect();
            let at = rng.below(words.len());
            let d = *rng.choose(&digits);
            words[at].push(d);
        }
        let mut out = Zeroizing::new(String::new());
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                out.push_str(&self.separator);
            }
            out.push_str(w);
        }
        out
    }

    /// Conservative entropy of the generation process, in bits.
    ///
    /// Characters: `sum(log2 |class|)` for the guaranteed characters plus
    /// `(length - classes) * log2 |union|` for the rest; the shuffle adds a
    /// little more that is not counted. Words: `n * log2(7776)`, plus
    /// `log2(10)` for the digit (its position is not counted). PIN:
    /// `length * log2(10)`.
    pub fn entropy_bits(&self) -> Result<f64> {
        Ok(match self.kind {
            Kind::Chars => {
                let pools = self.pools()?;
                let union: usize = pools.iter().map(|(_, p)| p.len()).sum();
                let guaranteed: f64 = pools.iter().map(|(_, p)| (p.len() as f64).log2()).sum();
                let rest = self.length.saturating_sub(pools.len()) as f64 * (union as f64).log2();
                guaranteed + rest
            }
            Kind::Pin => self.length as f64 * 10f64.log2(),
            Kind::Words => {
                let base = self.words as f64 * (wordlist::words().len() as f64).log2();
                if self.word_digit { base + 10f64.log2() } else { base }
            }
        })
    }

    /// One line a person can read: what this rule produces.
    pub fn summary(&self) -> String {
        match self.kind {
            Kind::Chars => {
                let mut s = format!("{} characters", self.length);
                if let Some(min) = self.length_min {
                    s.push_str(&format!(" (site allows {min}-{})", self.length));
                }
                let names: Vec<&str> = self.enabled_classes().iter().map(|c| c.name()).collect();
                s.push_str(&format!(", {}", names.join(" + ")));
                if let Some(n) = self.require {
                    s.push_str(&format!(", at least {n} classes"));
                }
                if self.symbols && self.symbol_set != DEFAULT_SYMBOLS {
                    s.push_str(&format!(", symbols {}", self.symbol_set));
                }
                if self.no_ambiguous {
                    s.push_str(", no ambiguous characters");
                }
                s
            }
            Kind::Pin => format!("{}-digit PIN", self.length),
            Kind::Words => {
                let mut s = format!("{} words separated by \"{}\"", self.words, self.separator);
                if self.capitalize {
                    s.push_str(", capitalized");
                }
                if self.word_digit {
                    s.push_str(", one digit");
                }
                s
            }
        }
    }
}

fn check_range(what: &str, n: usize, max: usize) -> Result<()> {
    if n == 0 || n > max {
        return Err(Error::Usage(format!("{what} must be between 1 and {max}; choose a value in that range.")));
    }
    Ok(())
}

/// Symbols must be printable ASCII punctuation. Duplicates are dropped so a
/// repeated symbol does not become more likely than the others.
pub fn validate_symbols(set: &str) -> Result<Vec<char>> {
    let mut out: Vec<char> = Vec::new();
    for c in set.chars() {
        if !c.is_ascii_punctuation() {
            let shown = if c.is_whitespace() || c.is_control() { format!("{:?}", c) } else { c.to_string() };
            return Err(Error::Usage(format!(
                "The symbol set may only hold ASCII punctuation, and {shown} is not; remove it from --symbols."
            )));
        }
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}
