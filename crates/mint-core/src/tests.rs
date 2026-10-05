use std::collections::HashMap;

use crate::onepassword::{NewLogin, create_args, login_template, replace_password};
use crate::presets::{PresetSpec, builtins_for_test, parse_user_presets};
use crate::random::SecureRng;
use crate::rule::{AMBIGUOUS, CharClass, DEFAULT_SYMBOLS, Kind, LengthSpec, Rule};
use crate::{Error, wordlist};

/// Upper critical value of chi-square with `df` degrees of freedom at
/// p = 1e-6 (Wilson–Hilferty). A correct sampler fails this about once in a
/// million runs; a biased one fails it every time at these sample sizes.
fn chi_square_critical(df: f64) -> f64 {
    const Z: f64 = 4.753_424; // standard normal upper quantile for 1e-6
    let a = 2.0 / (9.0 * df);
    df * (1.0 - a + Z * a.sqrt()).powi(3)
}

fn chi_square(counts: &[u64], expected: f64) -> f64 {
    counts.iter().map(|&c| (c as f64 - expected).powi(2) / expected).sum()
}

#[test]
fn sampler_is_uniform_for_awkward_ranges() {
    // Sizes that are not powers of two, where `byte % n` would be biased.
    let mut rng = SecureRng::new();
    for n in [3usize, 10, 26, 62, 77, 200, 7776] {
        let per_bin = if n > 1000 { 60 } else { 4000 };
        let mut counts = vec![0u64; n];
        for _ in 0..n * per_bin {
            counts[rng.below(n)] += 1;
        }
        let stat = chi_square(&counts, per_bin as f64);
        let crit = chi_square_critical((n - 1) as f64);
        assert!(stat < crit, "n={n}: chi-square {stat:.1} exceeds {crit:.1}");
    }
}

#[test]
fn generated_characters_are_uniform() {
    // One class, so the guaranteed character comes from the same pool and
    // every position should be uniform over a-z.
    let rule = Rule { upper: false, digits: false, symbols: false, length: 64, ..Rule::default() };
    let mut counts: HashMap<char, u64> = HashMap::new();
    let rounds = 4000;
    for _ in 0..rounds {
        for c in rule.generate().unwrap().value.chars() {
            *counts.entry(c).or_default() += 1;
        }
    }
    assert_eq!(counts.len(), 26);
    let observed: Vec<u64> = counts.values().copied().collect();
    let expected = (rounds * 64) as f64 / 26.0;
    let stat = chi_square(&observed, expected);
    assert!(stat < chi_square_critical(25.0), "chi-square {stat:.1}");
}

#[test]
fn shuffle_is_uniform_over_permutations() {
    let mut rng = SecureRng::new();
    let mut counts: HashMap<[u8; 3], u64> = HashMap::new();
    let rounds = 60_000;
    for _ in 0..rounds {
        let mut v = [0u8, 1, 2];
        rng.shuffle(&mut v);
        *counts.entry(v).or_default() += 1;
    }
    assert_eq!(counts.len(), 6);
    let observed: Vec<u64> = counts.values().copied().collect();
    let stat = chi_square(&observed, rounds as f64 / 6.0);
    assert!(stat < chi_square_critical(5.0), "chi-square {stat:.1}");
}

#[test]
fn every_required_class_is_present_in_100k_generations() {
    for length in [4usize, 16] {
        let rule = Rule { length, ..Rule::default() };
        for _ in 0..100_000 {
            let p = rule.generate().unwrap();
            assert_eq!(p.value.chars().count(), length);
            assert_eq!(p.classes, CharClass::ALL.to_vec(), "missing a class in a {length}-character password");
        }
    }
}

#[test]
fn sampling_path_has_no_remainder_operator() {
    let source = include_str!("random.rs");
    for (n, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        assert!(!code.contains('%'), "random.rs line {} uses %: {line}", n + 1);
        assert!(!code.contains("rem_euclid") && !code.contains(".rem("), "random.rs line {} takes a remainder", n + 1);
    }
}

#[test]
fn length_range_uses_the_top() {
    assert_eq!(LengthSpec::parse("10-16").unwrap(), LengthSpec { min: Some(10), max: 16 });
    assert_eq!(LengthSpec::parse("32").unwrap(), LengthSpec { min: None, max: 32 });
    assert_eq!(LengthSpec::parse("4096").unwrap().max, 4096);
    for bad in ["0", "4097", "16-10", "abc", "-5", ""] {
        assert_eq!(LengthSpec::parse(bad).unwrap_err().exit_code(), 2, "{bad}");
    }
}

#[test]
fn long_passwords_have_no_64_character_cap() {
    let p = Rule { length: 4096, ..Rule::default() }.generate().unwrap();
    assert_eq!(p.length, 4096);
}

#[test]
fn unsatisfiable_rules_exit_3() {
    let too_short = Rule { length: 3, ..Rule::default() };
    assert_eq!(too_short.generate().unwrap_err().exit_code(), 3);
    let too_few = Rule { symbols: false, digits: false, require: Some(3), ..Rule::default() };
    assert_eq!(too_few.generate().unwrap_err().exit_code(), 3);
    let none = Rule { upper: false, lower: false, digits: false, symbols: false, ..Rule::default() };
    assert_eq!(none.generate().unwrap_err().exit_code(), 3);
    let empty_symbols = Rule { symbol_set: String::new(), ..Rule::default() };
    assert_eq!(empty_symbols.generate().unwrap_err().exit_code(), 3);
    let only_ambiguous = Rule { symbol_set: "|".into(), no_ambiguous: true, ..Rule::default() };
    assert_eq!(only_ambiguous.generate().unwrap_err().exit_code(), 3);
}

#[test]
fn require_more_than_four_is_a_usage_error() {
    assert_eq!(Rule { require: Some(5), ..Rule::default() }.generate().unwrap_err().exit_code(), 2);
}

#[test]
fn symbol_set_is_respected_and_validated() {
    let rule = Rule { symbol_set: "!!#".into(), length: 200, ..Rule::default() };
    let p = rule.generate().unwrap();
    assert!(p.value.chars().filter(|c| !c.is_ascii_alphanumeric()).all(|c| c == '!' || c == '#'));
    for bad in ["a", "1", " ", "é", "\t"] {
        let rule = Rule { symbol_set: bad.into(), ..Rule::default() };
        assert_eq!(rule.generate().unwrap_err().exit_code(), 2, "{bad:?}");
    }
    assert!(DEFAULT_SYMBOLS.chars().all(|c| c.is_ascii_punctuation()));
}

#[test]
fn no_ambiguous_drops_lookalikes() {
    let rule = Rule { no_ambiguous: true, symbol_set: "!|#".into(), length: 400, ..Rule::default() };
    for _ in 0..50 {
        let p = rule.generate().unwrap();
        assert!(!p.value.chars().any(|c| AMBIGUOUS.contains(c)));
    }
}

#[test]
fn pins_are_digits() {
    let rule = Rule { kind: Kind::Pin, length: 6, ..Rule::default() };
    let p = rule.generate().unwrap();
    assert!(p.value.chars().all(|c| c.is_ascii_digit()) && p.length == 6);
    assert!((p.entropy_bits - 6.0 * 10f64.log2()).abs() < 1e-9);
}

#[test]
fn passphrases_come_from_the_wordlist() {
    assert_eq!(wordlist::words().len(), 7776);
    let rule = Rule { kind: Kind::Words, words: 6, separator: ".".into(), ..Rule::default() };
    let p = rule.generate().unwrap();
    let words: Vec<&str> = p.value.split('.').collect();
    assert_eq!(words.len(), 6);
    assert!(words.iter().all(|w| wordlist::words().contains(w)));
    assert!((p.entropy_bits - 6.0 * 7776f64.log2()).abs() < 1e-9);

    let rule = Rule {
        kind: Kind::Words,
        words: 4,
        separator: " ".into(),
        capitalize: true,
        word_digit: true,
        ..Rule::default()
    };
    let p = rule.generate().unwrap();
    assert_eq!(p.value.chars().filter(|c| c.is_ascii_digit()).count(), 1);
    assert!(p.value.split(' ').all(|w| w.chars().next().unwrap().is_ascii_uppercase()));
}

#[test]
fn entropy_is_conservative() {
    // Default: 24 chars over 26+26+10+13 = 75, four guaranteed.
    let rule = Rule::default();
    let expected = 26f64.log2() * 2.0 + 10f64.log2() + 13f64.log2() + 20.0 * 75f64.log2();
    assert!((rule.entropy_bits().unwrap() - expected).abs() < 1e-9);
    assert!(rule.entropy_bits().unwrap() < 24.0 * 75f64.log2());
}

#[test]
fn builtin_presets_are_valid() {
    for (name, spec) in builtins_for_test() {
        let rule = spec.to_rule(name).unwrap();
        rule.generate().unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let moneris = builtins_for_test().into_iter().find(|(n, _)| *n == "moneris").unwrap().1.to_rule("moneris").unwrap();
    assert_eq!((moneris.length, moneris.length_min, moneris.require), (16, Some(10), Some(3)));
    let wifi = builtins_for_test().into_iter().find(|(n, _)| *n == "wifi").unwrap().1.to_rule("wifi").unwrap();
    assert_eq!((wifi.length, wifi.no_ambiguous), (63, true));
}

#[test]
fn user_presets_parse_and_report_line_numbers() {
    let text = "[bank]\nlength = \"8-12\"\nclasses = [\"upper\", \"digits\"]\nrequire = 2\n\n[pin]\npin = 4\n";
    let presets = parse_user_presets(text, "presets.toml").unwrap();
    let bank = presets["bank"].to_rule("bank").unwrap();
    assert_eq!((bank.length, bank.lower, bank.symbols, bank.require), (12, false, false, Some(2)));
    assert_eq!(presets["pin"].to_rule("pin").unwrap().kind, Kind::Pin);

    let err = parse_user_presets("[a]\nlength = 8\nlenght = 9\n", "presets.toml").unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.message().contains("line 3"), "{}", err.message());

    let bad_class = PresetSpec { classes: Some(vec!["emoji".into()]), ..Default::default() };
    assert_eq!(bad_class.to_rule("x").unwrap_err().exit_code(), 2);
}

#[test]
fn password_goes_in_the_template_never_in_argv() {
    let secret = "S3cret!-only-on-stdin";
    let login = NewLogin {
        title: "Example".into(),
        vault: Some("Private".into()),
        url: Some("https://example.com".into()),
        username: Some("me".into()),
    };
    assert!(create_args(&login).iter().all(|a| !a.contains(secret)));
    assert_eq!(create_args(&login).last().map(String::as_str), Some("-"));
    let template: serde_json::Value = serde_json::from_str(&login_template(&login, secret)).unwrap();
    assert_eq!(template["fields"][1]["value"], secret);
    assert_eq!(template["fields"][1]["purpose"], "PASSWORD");
    assert_eq!(template["fields"][0]["value"], "me");
}

#[test]
fn replacing_a_password_keeps_other_fields_and_refuses_passkeys() {
    let mut doc = serde_json::json!({"id": "x", "fields": [
        {"id": "username", "purpose": "USERNAME", "value": "me"},
        {"id": "password", "type": "CONCEALED", "purpose": "PASSWORD", "value": "old"}
    ]});
    let t: serde_json::Value = serde_json::from_str(&replace_password(&mut doc, "new").unwrap()).unwrap();
    assert_eq!(t["fields"][1]["value"], "new");
    assert_eq!(t["fields"][0]["value"], "me");

    let mut passkey = serde_json::json!({"id": "x", "fields": [{"id": "pk", "type": "PASSKEY"}]});
    assert!(matches!(replace_password(&mut passkey, "new"), Err(Error::OnePassword(_))));
}

#[test]
fn op_errors_name_a_next_step() {
    let e = crate::onepassword::explain_failure(
        "[ERROR] 2026/10/05 11:08:57 \"zzz\" isn't a vault in this account. Specify the vault with its ID or name.\n",
    );
    assert_eq!(e.exit_code(), 4);
    assert!(e.message().contains("op vault list"), "{}", e.message());
    assert!(!e.message().contains("2026/10/05"));
}
