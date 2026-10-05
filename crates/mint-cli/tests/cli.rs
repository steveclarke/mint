//! The CLI contract the window and the Omarchy plugin rely on.

use std::process::{Command, Output};

use serde_json::Value;

/// Every test runs mint with its own config directory and with `MINT_OP`
/// pointing at a path that does not exist, so no test can reach the real
/// 1Password account.
fn command(config: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mint"));
    cmd.env("XDG_CONFIG_HOME", config).env("APPDATA", config).env("MINT_OP", "/nonexistent/mint-test/op");
    cmd
}

fn mint(args: &[&str]) -> Output {
    let config = std::env::temp_dir().join(format!("mint-cli-test-{}", std::process::id()));
    command(&config).args(args).output().expect("mint runs")
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[test]
fn default_prints_one_24_character_password_and_newline() {
    let out = mint(&[]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(text.trim_end_matches('\n').chars().count(), 24);
}

#[test]
fn json_has_the_documented_fields() {
    let out = mint(&["--preset", "moneris", "--json"]);
    assert!(out.status.success());
    let v = json(&out);
    assert_eq!(v["length"], 16);
    assert_eq!(v["password"].as_str().unwrap().chars().count(), 16);
    assert_eq!(v["kind"], "chars");
    assert_eq!(v["classes"], serde_json::json!(["upper", "lower", "digits", "symbols"]));
    assert!(v["entropy_bits"].as_f64().unwrap() > 80.0);
    assert_eq!(v["rule"]["preset"], "moneris");
    assert!(v["rule"]["summary"].is_string());
}

#[test]
fn count_gives_lines_or_a_json_array() {
    let out = mint(&["--count", "5", "12"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 5);
    assert!(text.lines().all(|l| l.chars().count() == 12));
    let v = json(&mint(&["--count", "1", "--json"]));
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn range_uses_the_top_and_words_and_pins_work() {
    let v = json(&mint(&["10-16", "--require", "3", "--json"]));
    assert_eq!(v["length"], 16);
    let v = json(&mint(&["--words", "4", "--separator", " ", "--json"]));
    assert_eq!(v["password"].as_str().unwrap().split(' ').count(), 4);
    let v = json(&mint(&["--pin", "6", "--json"]));
    assert!(v["password"].as_str().unwrap().chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn exit_codes_mean_something() {
    let cases: &[(&[&str], i32)] = &[
        (&["--bogus"], 2),
        (&["0"], 2),
        (&["--preset", "no-such-preset"], 2),
        (&["--words", "3", "20"], 2),
        (&["3"], 3),
        (&["--no-upper", "--no-lower", "--require", "3"], 3),
        (&["--require", "0"], 2),
        (&["save"], 2),
    ];
    for (args, code) in cases {
        let out = mint(args);
        assert_eq!(out.status.code(), Some(*code), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} printed to stdout");
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(!err.trim().is_empty(), "{args:?} printed no error");
    }
}

#[test]
fn json_errors_are_objects_on_stderr() {
    for (args, code) in [(vec!["3", "--json"], 3), (vec!["--bogus", "--json"], 2)] {
        let out = mint(&args);
        assert_eq!(out.status.code(), Some(code));
        let v: Value = serde_json::from_slice(&out.stderr).expect("stderr is JSON");
        assert_eq!(v["code"], code);
        assert!(v["error"].is_string() && v["kind"].is_string());
    }
}

#[test]
fn missing_op_exits_4() {
    let out = mint(&["save", "--title", "never created", "--json"]);
    assert_eq!(out.status.code(), Some(4));
    let v: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["kind"], "onepassword");
}

#[test]
fn presets_list_includes_builtins_and_user_presets() {
    let config = std::env::temp_dir().join(format!("mint-presets-test-{}", std::process::id()));
    std::fs::create_dir_all(config.join("mint")).unwrap();
    std::fs::write(config.join("mint").join("presets.toml"), "[bank]\nlength = \"8-12\"\nrequire = 2\n").unwrap();
    let out = command(&config).args(["presets", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = v.as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    for n in ["moneris", "pin6", "wifi", "bank"] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    let bank = v.as_array().unwrap().iter().find(|p| p["name"] == "bank").unwrap();
    assert_eq!(bank["source"], "user");
    let out = command(&config).args(["--preset", "bank", "--json"]).output().unwrap();
    assert_eq!(json(&out)["length"], 12);
}
