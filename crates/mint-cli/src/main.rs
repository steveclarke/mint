//! `mint`: passwords on the command line, and the contract the window and the
//! Omarchy plugin build on (`--json`, `save`, exit codes).

#[cfg(not(target_os = "linux"))]
use std::io::Read;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use mint_core::clipboard::{self, Copied};
use mint_core::onepassword::{self, NewLogin, SavedItem};
use mint_core::presets;
use mint_core::settings::Settings;
use mint_core::{Error, Kind, LengthSpec, Password, Result, Rule};
use serde_json::{Value, json};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "mint",
    version,
    about = "Strong passwords from the OS random source, to stdout, the clipboard or 1Password.",
    after_help = "Exit codes: 0 success, 2 usage error, 3 rule cannot be satisfied, 4 1Password (op) missing or failed, 5 clipboard failed."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    rule_args: GenArgs,
    #[command(flatten)]
    out: OutArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a password and save it to 1Password (new Login, or --item to replace a password).
    Save(Box<SaveArgs>),
    /// Copy UTF-8 secret bytes from stdin, concealed and conditionally cleared.
    Copy,
    /// Open mint's window, or show/hide the running one.
    Gui {
        /// Show the window if hidden, hide it if shown.
        #[arg(long)]
        toggle: bool,
    },
    /// List built-in and user presets.
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// Internal: clears the clipboard after a delay if it still holds the
    /// password, which arrives on stdin.
    #[command(name = "__clear-clipboard", hide = true)]
    ClearClipboard {
        #[arg(long)]
        after: u64,
        #[arg(long, allow_negative_numbers = true)]
        token: i64,
    },
}

#[derive(Args, Clone, Default)]
struct GenArgs {
    /// Length, or a site's allowed range like 10-16 (mint uses the top). Default 24.
    #[arg(value_name = "LENGTH")]
    length: Option<String>,
    /// Use a named rule set (see `mint presets`).
    #[arg(long, short = 'p', value_name = "NAME")]
    preset: Option<String>,
    /// At least N character classes must be enabled; every enabled class appears.
    #[arg(long, short = 'r', value_name = "N")]
    require: Option<usize>,
    /// Allowed symbols, for sites that accept only some (turns symbols on).
    #[arg(long, value_name = "SET", allow_hyphen_values = true)]
    symbols: Option<String>,
    #[arg(long)]
    no_upper: bool,
    #[arg(long)]
    no_lower: bool,
    #[arg(long)]
    no_digits: bool,
    #[arg(long)]
    no_symbols: bool,
    /// Leave out look-alike characters (0 O o 1 l I |).
    #[arg(long)]
    no_ambiguous: bool,
    /// A passphrase of N words from the EFF large wordlist.
    #[arg(long, value_name = "N", conflicts_with = "pin")]
    words: Option<usize>,
    /// Passphrase separator (default "-").
    #[arg(long, value_name = "SEP", allow_hyphen_values = true)]
    separator: Option<String>,
    /// Capitalize each passphrase word.
    #[arg(long)]
    capitalize: bool,
    /// Add one digit to one passphrase word.
    #[arg(long)]
    digit: bool,
    /// A PIN of N digits.
    #[arg(long, value_name = "N")]
    pin: Option<usize>,
}

#[derive(Args, Clone, Default)]
struct OutArgs {
    /// Machine-readable output (errors become {"error", "code", "kind"} on stderr).
    #[arg(long, global = true)]
    json: bool,
    /// Copy to the clipboard, concealed from clipboard managers, instead of printing.
    #[arg(long, short = 'c', global = true)]
    copy: bool,
    /// How many passwords to print, one per line (a JSON array with --json).
    #[arg(long, short = 'n', value_name = "N")]
    count: Option<usize>,
    /// Leave the copied password on the clipboard.
    #[arg(long, global = true)]
    no_clear: bool,
    /// Seconds before the clipboard is cleared (default 45, or `clear_after` in config.toml).
    #[arg(long, value_name = "SECS", global = true)]
    clear_after: Option<u64>,
}

#[derive(Args)]
struct SaveArgs {
    /// Title of the new Login item.
    #[arg(long, short = 't')]
    title: Option<String>,
    /// Vault name or ID (default: op's default vault).
    #[arg(long)]
    vault: Option<String>,
    /// Website for the item.
    #[arg(long)]
    url: Option<String>,
    /// Username for the item.
    #[arg(long)]
    username: Option<String>,
    /// Replace the password of this existing item (name or ID) instead of creating one.
    #[arg(long, value_name = "ID", conflicts_with_all = ["title", "url", "username"])]
    item: Option<String>,
    /// Also print the password.
    #[arg(long)]
    show: bool,
    #[command(flatten)]
    rule_args: GenArgs,
}

fn main() -> ExitCode {
    let wants_json = std::env::args().any(|a| a == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if !e.use_stderr() => {
            // --help and --version.
            let _ = e.print();
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            if wants_json {
                let first = e
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid arguments")
                    .trim_start_matches("error: ")
                    .to_string();
                report(&Error::Usage(format!("{first}; run `mint --help` for usage.")), true);
            } else {
                let _ = e.print();
            }
            return ExitCode::from(2);
        }
    };
    let json = cli.out.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report(&e, json);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn report(e: &Error, json: bool) {
    if json {
        eprintln!("{}", json!({"error": e.message(), "code": e.exit_code(), "kind": e.kind()}));
    } else {
        eprintln!("mint: {}", e.message());
    }
}

fn run(cli: Cli) -> Result<()> {
    let settings = Settings::load()?;
    match cli.command {
        None => generate(&cli.rule_args, &cli.out, &settings),
        Some(Command::Save(args)) => save(&args, &cli.out, &settings),
        Some(Command::Copy) => copy_stdin(&cli.out, &settings),
        Some(Command::Presets { json }) => list_presets(json || cli.out.json),
        Some(Command::Gui { toggle }) => gui(toggle),
        Some(Command::ClearClipboard { after, token }) => clear_later(after, token),
    }
}

/// Turns flags (and an optional preset) into a rule.
fn build_rule(args: &GenArgs, settings: &Settings) -> Result<Rule> {
    let mut rule = Rule::default();
    let shapes_kind = args.words.is_some() || args.pin.is_some();
    let preset = args.preset.clone().or_else(|| if shapes_kind { None } else { settings.default_preset.clone() });
    if let Some(name) = &preset {
        let preset = presets::find(name)?;
        preset.spec.apply(&mut rule)?;
        rule.preset = Some(preset.name);
    }
    if let Some(n) = args.pin {
        rule.kind = Kind::Pin;
        rule.length = n;
        rule.length_min = None;
    }
    if let Some(n) = args.words {
        rule.kind = Kind::Words;
        rule.words = n;
    }
    let word_flags = args.separator.is_some() || args.capitalize || args.digit;
    if word_flags && rule.kind != Kind::Words {
        return Err(Error::Usage("--separator, --capitalize and --digit shape passphrases; add --words N.".into()));
    }
    if let Some(sep) = &args.separator {
        rule.separator = sep.clone();
    }
    rule.capitalize |= args.capitalize;
    rule.word_digit |= args.digit;
    if let Some(length) = &args.length {
        match rule.kind {
            Kind::Words => {
                return Err(Error::Usage(
                    "A passphrase is measured in words; use --words N instead of a length.".into(),
                ));
            }
            Kind::Pin if args.pin.is_some() => {
                return Err(Error::Usage("Give the PIN length once, with --pin N.".into()));
            }
            _ => rule.set_length(LengthSpec::parse(length)?),
        }
    }
    let class_flags = args.no_upper || args.no_lower || args.no_digits || args.no_symbols || args.symbols.is_some();
    if (class_flags || args.require.is_some() || args.no_ambiguous) && rule.kind != Kind::Chars {
        return Err(Error::Usage(
            "Character class flags apply to character passwords, not PINs or passphrases; drop them.".into(),
        ));
    }
    if let Some(set) = &args.symbols {
        rule.symbol_set = set.clone();
        rule.symbols = true;
    }
    rule.upper &= !args.no_upper;
    rule.lower &= !args.no_lower;
    rule.digits &= !args.no_digits;
    rule.symbols &= !args.no_symbols;
    if args.require.is_some() {
        rule.require = args.require;
    }
    rule.no_ambiguous |= args.no_ambiguous;
    Ok(rule)
}

fn password_json(p: &Password, rule: &Rule, include_secret: bool) -> Value {
    let mut v = json!({
        "length": p.length,
        "kind": rule.kind,
        "classes": p.classes,
        "entropy_bits": (p.entropy_bits * 10.0).round() / 10.0,
        "rule": {"preset": rule.preset, "summary": rule.summary()},
    });
    if include_secret {
        v["password"] = Value::String(p.value.to_string());
    }
    v
}

fn generate(args: &GenArgs, out: &OutArgs, settings: &Settings) -> Result<()> {
    let rule = build_rule(args, settings)?;
    let count = out.count.unwrap_or(1);
    if count == 0 || count > 1000 {
        return Err(Error::Usage("--count must be between 1 and 1000; choose a count in that range.".into()));
    }
    if out.copy && count > 1 {
        return Err(Error::Usage("--copy copies one password; drop --count or --copy.".into()));
    }
    if out.copy {
        let p = rule.generate()?;
        let clears = copy(&p, out, settings)?;
        if out.json {
            let mut v = password_json(&p, &rule, false);
            v["copied"] = Value::Bool(true);
            v["clears_after"] = clears.map(Value::from).unwrap_or(Value::Null);
            println!("{v}");
        }
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    if out.json {
        let items: Vec<Value> =
            (0..count).map(|_| rule.generate().map(|p| password_json(&p, &rule, true))).collect::<Result<_>>()?;
        // An explicit --count (even 1) always gives an array.
        let v = if out.count.is_none() { items.into_iter().next().expect("one item") } else { Value::Array(items) };
        let text = Zeroizing::new(v.to_string());
        let _ = writeln!(stdout, "{}", text.as_str());
    } else {
        for _ in 0..count {
            let p = rule.generate()?;
            let _ = writeln!(stdout, "{}", p.value.as_str());
        }
    }
    Ok(())
}

/// Copies concealed and schedules the clear. Returns the clear delay, if any.
fn copy(p: &Password, out: &OutArgs, settings: &Settings) -> Result<Option<u64>> {
    let delay = copy_secret(&p.value, out, settings)?;
    if !out.json {
        let what = match p.kind {
            Kind::Words => "passphrase",
            Kind::Pin => "PIN",
            Kind::Chars => "password",
        };
        let mut msg = format!("Copied a {}-character {what} ({:.0} bits)", p.length, p.entropy_bits);
        msg.push_str(&if delay > 0 { format!("; the clipboard clears in {delay} s.") } else { ".".into() });
        eprintln!("{msg}");
    }
    Ok(if delay > 0 { Some(delay) } else { None })
}

/// The same clipboard path serves generation, save and stdin copies.
fn copy_secret(secret: &str, out: &OutArgs, settings: &Settings) -> Result<u64> {
    let copied = clipboard::copy_concealed(secret)?;
    let delay = if out.no_clear { 0 } else { out.clear_after.unwrap_or(settings.clear_after) };
    if delay > 0 {
        if let Err(error) = spawn_clearer(copied, delay, secret) {
            let _ = clipboard::clear_if_unchanged(copied, secret);
            return Err(error);
        }
    }
    Ok(delay)
}

fn read_secret() -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(target_os = "linux")]
    {
        mint_core::clipboard_process::read_stdin(16384)
            .map_err(|_| Error::Usage("Could not read bounded secret stdin.".into()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Usage("Could not read secret stdin.".into()))?;
        Ok(bytes)
    }
}

fn copy_stdin(out: &OutArgs, settings: &Settings) -> Result<()> {
    if std::io::stdin().is_terminal() {
        return Err(Error::Usage("Pipe the secret to mint copy on stdin.".into()));
    }
    let bytes = read_secret()?;
    if bytes.is_empty() || bytes.len() > 16384 || bytes.contains(&0) {
        return Err(Error::Usage("Stdin must contain 1 to 16384 UTF-8 bytes without NUL.".into()));
    }
    let secret = std::str::from_utf8(&bytes).map_err(|_| Error::Usage("Stdin must contain valid UTF-8.".into()))?;
    let delay = copy_secret(secret, out, settings)?;
    if out.json {
        println!("{}", json!({"copied": true, "clears_after": if delay > 0 { Some(delay) } else { None }}));
    } else {
        eprintln!(
            "Copied.{}",
            if delay > 0 { format!(" The clipboard clears in {delay} s if unchanged.") } else { String::new() }
        );
    }
    Ok(())
}

/// Lets a child outlive this process and the terminal: its own process
/// group on Unix, no console on Windows.
fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Whether the clearer must compare contents, and so needs the password.
/// macOS and Windows compare the clipboard's change counter instead.
const CLEARER_NEEDS_SECRET: bool = cfg!(all(unix, not(target_os = "macos")));

/// Starts a detached copy of mint that clears the clipboard after `delay`
/// seconds if it still holds the password. Where the password is needed
/// (Linux), it goes over a pipe; elsewhere the clearer never sees it.
fn spawn_clearer(copied: Copied, delay: u64, secret: &str) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::Clipboard(format!("Cannot find mint's own path ({e}); use --no-clear.")))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["__clear-clipboard", "--after", &delay.to_string(), "--token", &copied.token.to_string()])
        .stdin(if CLEARER_NEEDS_SECRET { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(if CLEARER_NEEDS_SECRET { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    #[allow(unused_mut)]
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Clipboard(format!("Cannot start the clipboard clearer ({e}); use --no-clear.")))?;
    #[cfg(target_os = "linux")]
    {
        mint_core::clipboard_process::handoff(child, secret.as_bytes())
            .map_err(|_| Error::Clipboard("Cannot hand the secret to the clipboard clearer.".into()))?;
    }
    #[cfg(not(target_os = "linux"))]
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(secret.as_bytes()).map_err(|e| {
            Error::Clipboard(format!("Cannot hand the password to the clipboard clearer ({e}); use --no-clear."))
        })?;
    }
    Ok(())
}

fn clear_later(after: u64, token: i64) -> Result<()> {
    let bytes = if CLEARER_NEEDS_SECRET { read_secret()? } else { Zeroizing::new(Vec::new()) };
    let secret = std::str::from_utf8(&bytes).map_err(|_| Error::Clipboard("Invalid secret stdin.".into()))?;
    if CLEARER_NEEDS_SECRET {
        std::io::stdout()
            .write_all(&[1])
            .and_then(|_| std::io::stdout().flush())
            .map_err(|_| Error::Clipboard("Cannot confirm clipboard clearer startup.".into()))?;
    }
    std::thread::sleep(std::time::Duration::from_secs(after));
    clipboard::clear_if_unchanged(Copied { token, concealed: true }, secret)?;
    Ok(())
}

fn save(args: &SaveArgs, out: &OutArgs, settings: &Settings) -> Result<()> {
    let rule = build_rule(&args.rule_args, settings)?;
    let title = args.title.as_deref().map(str::trim).unwrap_or_default();
    if args.item.is_none() && title.is_empty() {
        return Err(Error::Usage(
            "A new 1Password item needs a title; pass --title, or --item ID to replace an existing password.".into(),
        ));
    }
    let p = rule.generate()?;
    let item: SavedItem = match &args.item {
        Some(id) => onepassword::set_password(id, args.vault.as_deref(), &p.value)?,
        None => onepassword::create_login(
            &NewLogin {
                title: title.to_string(),
                vault: args.vault.clone(),
                url: args.url.clone(),
                username: args.username.clone(),
            },
            &p.value,
        )?,
    };
    // The item is saved, so a clipboard failure now is a warning, not a failure.
    let copied = out.copy.then(|| match copy(&p, out, settings) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("mint: warning: the item was saved but not copied: {}", e.message());
            false
        }
    });
    if out.json {
        let mut v = password_json(&p, &rule, args.show);
        let saved = serde_json::to_value(&item).expect("SavedItem serializes");
        if let (Some(obj), Value::Object(saved)) = (v.as_object_mut(), saved) {
            obj.extend(saved);
        }
        if let Some(copied) = copied {
            v["copied"] = Value::Bool(copied);
        }
        let text = Zeroizing::new(v.to_string());
        println!("{}", text.as_str());
    } else {
        let verb = if item.updated { "Updated the password of" } else { "Saved" };
        eprintln!("{verb} \"{}\" in {} ({} characters, {:.0} bits).", item.title, item.vault, p.length, p.entropy_bits);
        println!("{}", item.id);
        if let Some(link) = &item.link {
            println!("{link}");
        }
        if args.show {
            println!("{}", p.value.as_str());
        }
    }
    Ok(())
}

fn list_presets(json: bool) -> Result<()> {
    let all = presets::all()?;
    if json {
        println!("{}", serde_json::to_string(&all).expect("presets serialize"));
        return Ok(());
    }
    let width = all.iter().map(|p| p.name.len()).max().unwrap_or(0);
    for p in &all {
        let tag = if p.source == presets::Source::User { " (yours)" } else { "" };
        println!("{:width$}  {}{tag}", p.name, p.description);
    }
    if std::io::stdout().is_terminal() {
        if let Some(path) = mint_core::paths::presets_file() {
            println!("\nYour presets: {}", path.display());
        }
    }
    Ok(())
}

/// Finds mint's window app: next to this binary, inside mint.app, or on PATH.
fn app_binary() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "mint-app.exe" } else { "mint-app" };
    let here = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let mut candidates: Vec<std::path::PathBuf> =
        here.iter().filter_map(|p| p.parent().map(|d| d.join(name))).collect();
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/mint.app/Contents/MacOS/mint-app".into());
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(std::path::PathBuf::from(home).join("Applications/mint.app/Contents/MacOS/mint-app"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(name)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn gui(toggle: bool) -> Result<()> {
    let app = app_binary().ok_or_else(|| {
        Error::Usage("mint's window app (mint-app) is not installed; install mint.app or put mint-app on PATH.".into())
    })?;
    let mut cmd = std::process::Command::new(app);
    cmd.arg(if toggle { "--toggle" } else { "--show" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| Error::Usage(format!("Could not start mint's window ({e}); check the mint-app install.")))?;
    Ok(())
}
