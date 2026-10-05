//! Saving to 1Password through the `op` CLI.
//!
//! The password only ever reaches `op` on stdin, as an item JSON template:
//! `op item create --vault V --format json -` for a new Login, and
//! `op item get ID --format json --reveal` then `op item edit ID --format json`
//! (template on stdin) to replace an existing item's password. Titles, vaults
//! and URLs are not secret and go in argv.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Vault {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct SavedItem {
    pub id: String,
    pub title: String,
    pub vault_id: String,
    pub vault: String,
    /// A `https://start.1password.com/open/i?...` link that opens the item in
    /// the 1Password app; absent when the account cannot be determined.
    pub link: Option<String>,
    /// True when an existing item's password was replaced.
    pub updated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct NewLogin {
    pub title: String,
    pub vault: Option<String>,
    pub url: Option<String>,
    pub username: Option<String>,
}

/// Finds `op`: `MINT_OP` if set, otherwise `PATH`, then the usual install locations, so
/// an app started at login (with a bare `PATH`) still finds it.
pub fn find_op() -> Option<PathBuf> {
    // An explicit MINT_OP is the only candidate, even if it is wrong.
    if let Some(p) = std::env::var_os("MINT_OP").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let exe = if cfg!(windows) { "op.exe" } else { "op" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let fallbacks: &[&str] = if cfg!(windows) {
        &[]
    } else {
        &["/opt/homebrew/bin/op", "/usr/local/bin/op", "/usr/bin/op", "/run/current-system/sw/bin/op"]
    };
    fallbacks.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Runs `op` with `args`, writing `stdin` to it if given. Returns stdout.
fn op(args: &[&str], stdin: Option<&[u8]>) -> Result<Zeroizing<String>> {
    let bin = find_op().ok_or_else(|| {
        Error::OnePassword(
            "The 1Password CLI (op) is not installed; install it from https://developer.1password.com/docs/cli/get-started and turn on CLI integration in the 1Password app.".into(),
        )
    })?;
    let mut child = Command::new(&bin)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::OnePassword(format!("Could not run {} ({e}); check the 1Password CLI install.", bin.display()))
        })?;
    if let Some(input) = stdin {
        // Drop the handle after writing so op sees end of input.
        let mut pipe = child.stdin.take().expect("stdin is piped");
        pipe.write_all(input)
            .map_err(|e| Error::OnePassword(format!("Could not send the item to op ({e}); try again.")))?;
    }
    let out =
        child.wait_with_output().map_err(|e| Error::OnePassword(format!("op did not finish ({e}); try again.")))?;
    let stdout = Zeroizing::new(String::from_utf8_lossy(&out.stdout).into_owned());
    if out.status.success() {
        return Ok(stdout);
    }
    Err(explain_failure(&String::from_utf8_lossy(&out.stderr)))
}

/// Turns op's stderr into one sentence that names the next step.
pub fn explain_failure(stderr: &str) -> Error {
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("op failed without saying why");
    // Strip op's "[ERROR] 2026/10/05 11:08:57 " prefix.
    let detail = match line.strip_prefix("[ERROR] ") {
        Some(rest) => rest.splitn(3, ' ').nth(2).unwrap_or(rest),
        None => line,
    };
    // And op's "unable to process line 1: " wrapper around template errors.
    let detail = match detail.find(": ").filter(|_| detail.starts_with("unable to process line")) {
        Some(i) => &detail[i + 2..],
        None => detail,
    };
    let lower = stderr.to_ascii_lowercase();
    let msg = if lower.contains("not currently signed in")
        || lower.contains("no accounts configured")
        || lower.contains("account is not signed in")
        || lower.contains("authorization prompt dismissed")
        || lower.contains("authorization timeout")
    {
        format!(
            "1Password did not authorize the request ({}); unlock 1Password and allow mint, or run `op signin`.",
            detail.trim_end_matches('.')
        )
    } else if lower.contains("isn't a vault") || lower.contains("no vault found") {
        format!("{}; run `op vault list` to see vault names.", detail.trim_end_matches('.'))
    } else if lower.contains("isn't an item") {
        format!("{}; check the item ID with `op item list`.", detail.trim_end_matches('.'))
    } else {
        format!("1Password CLI failed: {}; run the same op command by hand to see more.", detail.trim_end_matches('.'))
    };
    Error::OnePassword(msg)
}

fn parse(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| {
        Error::OnePassword(format!("op returned output mint cannot read ({e}); update the 1Password CLI."))
    })
}

fn str_at(v: &Value, ptr: &str) -> String {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn list_vaults() -> Result<Vec<Vault>> {
    let out = op(&["vault", "list", "--format", "json"], None)?;
    let v = parse(&out)?;
    let mut vaults: Vec<Vault> = v
        .as_array()
        .map(|a| a.iter().map(|x| Vault { id: str_at(x, "/id"), name: str_at(x, "/name") }).collect())
        .unwrap_or_default();
    vaults.sort_by_key(|v| v.name.to_lowercase());
    Ok(vaults)
}

/// Builds the item template for a new Login. Kept separate so tests can
/// check that the password lands in the template and nowhere else.
pub fn login_template(login: &NewLogin, password: &str) -> Zeroizing<String> {
    let mut fields = vec![json!({
        "id": "password", "type": "CONCEALED", "purpose": "PASSWORD", "label": "password", "value": password
    })];
    if let Some(user) = login.username.as_deref().filter(|u| !u.is_empty()) {
        fields.insert(
            0,
            json!({"id": "username", "type": "STRING", "purpose": "USERNAME", "label": "username", "value": user}),
        );
    }
    Zeroizing::new(json!({"title": login.title, "category": "LOGIN", "fields": fields}).to_string())
}

/// The argv for creating a Login. Never contains the password.
pub fn create_args(login: &NewLogin) -> Vec<String> {
    let mut args: Vec<String> = vec!["item".into(), "create".into(), "--format".into(), "json".into()];
    if let Some(v) = login.vault.as_deref().filter(|v| !v.is_empty()) {
        args.extend(["--vault".into(), v.into()]);
    }
    if let Some(u) = login.url.as_deref().filter(|u| !u.is_empty()) {
        args.extend(["--url".into(), u.into()]);
    }
    args.push("-".into());
    args
}

/// Creates a Login item holding `password`.
pub fn create_login(login: &NewLogin, password: &str) -> Result<SavedItem> {
    if login.title.trim().is_empty() {
        return Err(Error::Usage("A 1Password item needs a title; pass --title.".into()));
    }
    let template = login_template(login, password);
    let args = create_args(login);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = op(&argv, Some(template.as_bytes()))?;
    saved(&parse(&out)?, false)
}

/// Replaces the password of an existing item.
pub fn set_password(item: &str, vault: Option<&str>, password: &str) -> Result<SavedItem> {
    let mut get = vec!["item", "get", item, "--format", "json", "--reveal"];
    if let Some(v) = vault.filter(|v| !v.is_empty()) {
        get.extend(["--vault", v]);
    }
    let raw = op(&get, None)?;
    let mut doc = parse(&raw)?;
    let id = str_at(&doc, "/id");
    let template = replace_password(&mut doc, password)?;
    let out = op(&["item", "edit", &id, "--format", "json"], Some(template.as_bytes()))?;
    saved(&parse(&out)?, true)
}

/// Writes `password` into the item's password field (adding a concealed
/// `password` field if it has none) and returns the item as a template.
/// Refuses items that hold a passkey: op's JSON templates cannot carry
/// passkeys, and an edit through a template would drop it.
pub fn replace_password(doc: &mut Value, password: &str) -> Result<Zeroizing<String>> {
    let has_passkey = doc
        .get("fields")
        .and_then(Value::as_array)
        .is_some_and(|fields| fields.iter().any(|f| str_at(f, "/type").eq_ignore_ascii_case("passkey")))
        || doc.get("passkey").is_some();
    if has_passkey {
        return Err(Error::OnePassword(
            "This item holds a passkey, which the 1Password CLI would delete when editing it; change its password in the 1Password app instead.".into(),
        ));
    }
    let fields = doc
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::OnePassword("op returned an item without fields; update the 1Password CLI.".into()))?;
    let target = fields
        .iter()
        .position(|f| str_at(f, "/purpose") == "PASSWORD")
        .or_else(|| fields.iter().position(|f| str_at(f, "/type") == "CONCEALED" && str_at(f, "/label") == "password"));
    match target {
        Some(i) => fields[i]["value"] = Value::String(password.to_string()),
        None => fields.push(json!({"id": "password", "type": "CONCEALED", "label": "password", "value": password})),
    }
    Ok(Zeroizing::new(doc.to_string()))
}

fn saved(item: &Value, updated: bool) -> Result<SavedItem> {
    let id = str_at(item, "/id");
    if id.is_empty() {
        return Err(Error::OnePassword("op did not return the saved item's ID; check 1Password for the item.".into()));
    }
    let vault_id = str_at(item, "/vault/id");
    Ok(SavedItem {
        link: item_link(&vault_id, &id),
        title: str_at(item, "/title"),
        vault: str_at(item, "/vault/name"),
        vault_id,
        id,
        updated,
    })
}

/// The "private link" 1Password itself copies for an item.
fn item_link(vault_id: &str, item_id: &str) -> Option<String> {
    let out = op(&["account", "list", "--format", "json"], None).ok()?;
    let accounts = parse(&out).ok()?;
    let accounts = accounts.as_array()?;
    let wanted = std::env::var("OP_ACCOUNT").ok();
    let account = match &wanted {
        Some(w) => accounts.iter().find(|a| {
            [str_at(a, "/account_uuid"), str_at(a, "/url"), str_at(a, "/user_uuid"), str_at(a, "/email")].contains(w)
        })?,
        None if accounts.len() == 1 => &accounts[0],
        None => return None,
    };
    Some(format!(
        "https://start.1password.com/open/i?a={}&v={vault_id}&i={item_id}&h={}",
        str_at(account, "/account_uuid"),
        str_at(account, "/url")
    ))
}
