//! mint's window, menu bar icon and global hotkey. Generation, clipboard and
//! 1Password all go through `mint-core`, the same engine as the CLI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mint_core::clipboard;
use mint_core::onepassword::{self, NewLogin, SavedItem, Vault};
use mint_core::presets;
use mint_core::settings::{Settings, Theme};
use mint_core::{CharClass, Kind, Rule};
use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use zeroize::Zeroizing;

const WIDTH: f64 = 460.0;
/// Launch-at-login starts the app with this flag so it stays in the menu bar.
/// The log restarts when it grows past this many bytes.
const LOG_CAP: u64 = 256 * 1024;
const HIDDEN_FLAG: &str = "--hidden";

struct AppState {
    settings: Settings,
    /// The current rule: the single source of truth for the window and the
    /// menu's "Copy New Password". Starts from `default_preset`.
    rule: Mutex<Rule>,
    /// The password the window shows. It never crosses IPC inbound: `copy`
    /// and `save` read it from here.
    last_password: Mutex<Option<Zeroizing<String>>>,
    /// Op calls in flight. While any runs, losing focus (to 1Password's
    /// approval prompt) must not hide the window.
    op_running: AtomicUsize,
    /// The registered hotkey, or why it could not be registered.
    hotkey: Mutex<Result<String, String>>,
    /// The menu's "Launch at Login" check, kept in step with the login item.
    login_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

#[derive(Serialize)]
struct CmdError {
    error: String,
    code: i32,
    kind: &'static str,
}

impl From<mint_core::Error> for CmdError {
    fn from(e: mint_core::Error) -> Self {
        CmdError { error: e.message().to_string(), code: e.exit_code(), kind: e.kind() }
    }
}

type CmdResult<T> = Result<T, CmdError>;

#[derive(Serialize)]
struct Generated {
    password: String,
    length: usize,
    kind: Kind,
    classes: Vec<CharClass>,
    entropy_bits: f64,
    summary: String,
}

#[derive(Serialize)]
struct PresetInfo {
    name: String,
    description: String,
    summary: String,
    user: bool,
    rule: Rule,
}

#[derive(Serialize)]
struct Init {
    rule: Rule,
    clear_after: u64,
    hotkey: Option<String>,
    hotkey_error: Option<String>,
    platform: &'static str,
    version: &'static str,
    /// Debug builds only: `MINT_DEMO` opens a fixed state for screenshots.
    demo: Option<String>,
}

#[derive(Deserialize)]
struct SaveRequest {
    title: String,
    vault: Option<String>,
    url: Option<String>,
    username: Option<String>,
}

/// Appends a line to mint's log (`~/Library/Logs/mint.log` on macOS, next to
/// the config elsewhere). Never logs secrets or item identifiers.
fn log(line: &str) {
    let path = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Logs/mint.log"))
    } else {
        mint_core::paths::config_dir().map(|d| d.join("mint.log"))
    };
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Start over once the log passes the cap, so it cannot grow without bound.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_CAP) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}

fn generated(rule: &Rule, p: mint_core::Password) -> Generated {
    Generated {
        password: p.value.to_string(),
        length: p.length,
        kind: p.kind,
        classes: p.classes,
        entropy_bits: (p.entropy_bits * 10.0).round() / 10.0,
        summary: rule.summary(),
    }
}

#[tauri::command]
fn init(state: State<AppState>) -> Init {
    let hotkey = state.hotkey.lock().unwrap().clone();
    Init {
        rule: state.rule.lock().unwrap().clone(),
        clear_after: state.settings.clear_after,
        hotkey: hotkey.clone().ok(),
        hotkey_error: hotkey.err(),
        platform: std::env::consts::OS,
        version: env!("CARGO_PKG_VERSION"),
        demo: demo(),
    }
}

#[tauri::command]
fn generate(rule: Rule, state: State<AppState>) -> CmdResult<Generated> {
    let p = rule.generate()?;
    *state.last_password.lock().unwrap() = Some(Zeroizing::new(p.value.to_string()));
    let out = generated(&rule, p);
    *state.rule.lock().unwrap() = rule;
    Ok(out)
}

#[tauri::command]
fn presets_list() -> CmdResult<Vec<PresetInfo>> {
    presets::all()?
        .into_iter()
        .map(|p| {
            Ok(PresetInfo {
                rule: p.spec.to_rule(&p.name)?,
                user: p.source == presets::Source::User,
                name: p.name,
                description: p.description,
                summary: p.summary,
            })
        })
        .collect()
}

/// Copies concealed and clears after `clear_after` seconds if unchanged.
fn copy_secret(secret: &str, clear_after: u64) -> CmdResult<Option<u64>> {
    let copied = clipboard::copy_concealed(secret)?;
    if clear_after == 0 {
        return Ok(None);
    }
    let secret = Zeroizing::new(secret.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(clear_after));
        let _ = clipboard::clear_if_unchanged(copied, &secret);
    });
    Ok(Some(clear_after))
}

#[tauri::command]
fn copy(state: State<AppState>) -> CmdResult<Option<u64>> {
    let password = state.last_password.lock().unwrap().clone().ok_or_else(no_password)?;
    copy_secret(&password, state.settings.clear_after)
}

fn no_password() -> CmdError {
    CmdError { error: "There is no password to use yet.".into(), code: 2, kind: "usage" }
}

/// Runs an op call off the main thread. 1Password may put up an approval
/// prompt, which takes focus: the window stays up meanwhile and takes the
/// keyboard back afterwards.
async fn with_op<T: Send + 'static>(
    app: &AppHandle,
    call: impl FnOnce() -> mint_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let state = app.state::<AppState>();
    state.op_running.fetch_add(1, Ordering::SeqCst);
    let result = tauri::async_runtime::spawn_blocking(call).await;
    state.op_running.fetch_sub(1, Ordering::SeqCst);
    if main_window(app).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || refocus(&handle));
    }
    result.map_err(|e| CmdError { error: e.to_string(), code: 4, kind: "onepassword" })?.map_err(Into::into)
}

fn refocus(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    panel::show(app);
    #[cfg(not(target_os = "macos"))]
    if let Some(w) = main_window(app) {
        let _ = w.set_focus();
    }
}

/// Screenshot mode for debug builds; never present in a release build.
fn demo() -> Option<String> {
    if cfg!(debug_assertions) { std::env::var("MINT_DEMO").ok() } else { None }
}

#[tauri::command]
async fn vaults(app: AppHandle) -> CmdResult<Vec<Vault>> {
    if demo().is_some() {
        let v = |id: &str, name: &str| Vault { id: id.into(), name: name.into() };
        return Ok(vec![v("demo1", "Personal"), v("demo2", "Shared")]);
    }
    with_op(&app, onepassword::list_vaults).await
}

/// Creates the login with `create`, except in demo mode, which returns a
/// made-up item and never calls it.
fn save_login(
    demo: bool,
    login: NewLogin,
    password: &str,
    create: impl FnOnce(&NewLogin, &str) -> mint_core::Result<SavedItem>,
) -> mint_core::Result<SavedItem> {
    if demo {
        return Ok(SavedItem {
            id: "demo".into(),
            vault_id: "demo".into(),
            vault: "Personal".into(),
            title: login.title,
            link: None,
            updated: false,
        });
    }
    create(&login, password)
}

#[tauri::command]
async fn save(request: SaveRequest, app: AppHandle) -> CmdResult<SavedItem> {
    let password = app.state::<AppState>().last_password.lock().unwrap().clone().ok_or_else(no_password)?;
    let demo = demo().is_some();
    let login = NewLogin { title: request.title, vault: request.vault, url: request.url, username: request.username };
    let saved = with_op(&app, move || save_login(demo, login, &password, onepassword::create_login)).await?;
    if !demo {
        log("saved an item to 1Password");
    }
    Ok(saved)
}

/// Opens a saved item in 1Password. Only 1Password's own links are accepted.
#[tauri::command]
fn open_item(link: String) -> CmdResult<()> {
    if !link.starts_with("https://start.1password.com/open/i?") {
        return Err(CmdError { error: "Not a 1Password item link.".into(), code: 2, kind: "usage" });
    }
    let opener = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&link).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("explorer").arg(&link).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&link).spawn()
    };
    opener.map(|_| ()).map_err(|e| CmdError {
        error: format!("Could not open the link ({e})."),
        code: 2,
        kind: "usage",
    })
}

#[tauri::command]
fn set_height(height: f64, window: WebviewWindow) {
    let _ = window.set_size(LogicalSize::new(WIDTH, height.clamp(200.0, 900.0)));
}

#[tauri::command]
fn hide(app: AppHandle) {
    hide_window(&app);
}

fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

fn hide_window(app: &AppHandle) {
    // On macOS the panel never activated mint, so the app that was in front
    // still is, and a paste lands there.
    #[cfg(target_os = "macos")]
    panel::hide(app);
    #[cfg(not(target_os = "macos"))]
    if let Some(w) = main_window(app) {
        let _ = w.hide();
    }
}

/// Whether losing focus should hide the window now.
fn should_hide_on_blur(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    state.settings.hide_on_blur && state.op_running.load(Ordering::SeqCst) == 0
}

/// Shows the window centered on the screen under the pointer, a third of the
/// way down, focused. `fresh` asks the page for a new password.
fn show_window(app: &AppHandle, fresh: bool) {
    let Some(w) = main_window(app) else { return };
    if let (Ok(cursor), Ok(monitors)) = (app.cursor_position(), w.available_monitors()) {
        let monitor = monitors.into_iter().find(|m| {
            let (p, s) = (m.position(), m.size());
            cursor.x >= p.x as f64
                && cursor.x < (p.x + s.width as i32) as f64
                && cursor.y >= p.y as f64
                && cursor.y < (p.y + s.height as i32) as f64
        });
        if let (Some(m), Ok(size)) = (monitor, w.outer_size()) {
            let (p, s) = (m.position(), m.size());
            let x = p.x + (s.width as i32 - size.width as i32) / 2;
            let y = p.y + (s.height as i32 - size.height as i32) / 3;
            let _ = w.set_position(PhysicalPosition::new(x, y));
        }
    }
    #[cfg(target_os = "macos")]
    panel::show(app);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    let _ = w.emit("mint://shown", fresh);
}

fn toggle_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let shown = panel::is_key(app);
    #[cfg(not(target_os = "macos"))]
    let shown = main_window(app).is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false));
    if shown { hide_window(app) } else { show_window(app, true) }
}

/// On macOS the window is a non-activating panel, like Spotlight's: it takes
/// the keyboard without activating mint, so it works over any app and gives
/// focus straight back when it hides.
#[cfg(target_os = "macos")]
#[allow(clippy::unused_unit)] // panel_event! requires the explicit `-> ()`
mod panel {
    use super::{AppHandle, log, main_window, should_hide_on_blur};
    use tauri_nspanel::{CollectionBehavior, ManagerExt, PanelLevel, StyleMask, WebviewWindowExt, tauri_panel};

    tauri_panel! {
        panel!(MintPanel {
            config: {
                can_become_key_window: true,
                is_floating_panel: true
            }
        })

        panel_event!(MintPanelEvents {
            window_did_resign_key(notification: &NSNotification) -> ()
        })
    }

    pub fn install(app: &AppHandle) -> tauri::Result<()> {
        let Some(window) = main_window(app) else { return Ok(()) };
        let panel = window.to_panel::<MintPanel>()?;
        if let Err(e) = panel.add_style_mask(StyleMask::empty().nonactivating_panel().value()) {
            log(&format!("panel style: {e:?}"));
        }
        panel.set_level(PanelLevel::ModalPanel.value());
        panel.set_hides_on_deactivate(false);
        panel.set_collection_behavior(
            CollectionBehavior::new().can_join_all_spaces().full_screen_auxiliary().transient().value(),
        );
        let events = MintPanelEvents::new();
        let handle = app.clone();
        events.window_did_resign_key(move |_| {
            if should_hide_on_blur(&handle) {
                hide(&handle);
            }
        });
        panel.set_event_handler(Some(events.as_ref()));
        Ok(())
    }

    pub fn show(app: &AppHandle) {
        if let Ok(panel) = app.get_webview_panel("main") {
            // Demo (screenshot) mode shows without taking the keyboard.
            if super::demo().is_some() {
                panel.order_front_regardless();
            } else {
                panel.show_and_make_key();
            }
        }
    }

    pub fn hide(app: &AppHandle) {
        if let Ok(panel) = app.get_webview_panel("main") {
            panel.hide();
        }
    }

    pub fn is_key(app: &AppHandle) -> bool {
        app.get_webview_panel("main").is_ok_and(|p| p.is_visible() && p.as_panel().isKeyWindow())
    }
}

/// What a launch (first or second) asks the running app to do.
#[derive(Debug, PartialEq, Eq)]
enum LaunchAction {
    LaunchAtLogin(bool),
    Toggle,
    Show,
    /// `--hidden`: stay in the menu bar.
    Stay,
}

fn parse_args(args: &[String]) -> LaunchAction {
    if let Some(i) = args.iter().position(|a| a == "--launch-at-login") {
        return LaunchAction::LaunchAtLogin(args.get(i + 1).is_some_and(|v| v == "on"));
    }
    if args.iter().any(|a| a == "--toggle") {
        LaunchAction::Toggle
    } else if args.iter().any(|a| a == HIDDEN_FLAG) {
        LaunchAction::Stay
    } else {
        LaunchAction::Show
    }
}

/// Handles `--toggle` / `--show` from a second launch (and `mint gui`).
fn handle_args(app: &AppHandle, args: &[String]) {
    match parse_args(args) {
        LaunchAction::LaunchAtLogin(on) => set_launch_at_login(app, on),
        LaunchAction::Toggle => toggle_window(app),
        LaunchAction::Show => show_window(app, true),
        LaunchAction::Stay => {}
    }
}

/// Turns the login item on or off and logs what the system reports back.
fn set_launch_at_login(app: &AppHandle, on: bool) {
    let launcher = app.autolaunch();
    let result = if on { launcher.enable() } else { launcher.disable() };
    let now = launcher.is_enabled().unwrap_or(false);
    log(&format!(
        "launch at login: requested {on}, now {now}{}",
        result.err().map(|e| format!(" ({e})")).unwrap_or_default()
    ));
    if let Some(item) = app.state::<AppState>().login_item.lock().unwrap().as_ref() {
        let _ = item.set_checked(now);
    }
}

/// Generates with `rule`, copies it, and flashes a check in the menu bar.
fn copy_from_menu(app: &AppHandle, rule: Rule) {
    let state = app.state::<AppState>();
    let result =
        rule.generate().map_err(CmdError::from).and_then(|p| copy_secret(&p.value, state.settings.clear_after));
    let (title, tip) = match result {
        Ok(_) => ("✓", "mint: copied".to_string()),
        Err(e) => ("!", format!("mint: {}", e.error)),
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_title(Some(title));
        let _ = tray.set_tooltip(Some(&tip));
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1600));
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_title(None::<&str>);
                let _ = tray.set_tooltip(Some("mint"));
            }
        });
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let generate = MenuItem::with_id(app, "generate", "Generate…", true, None::<&str>)?;
    let copy = MenuItem::with_id(app, "copy", "Copy New Password", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Window", true, None::<&str>)?;
    let presets_menu = Submenu::with_id(app, "presets", "Copy From Preset", true)?;
    for p in presets::all().unwrap_or_default() {
        let item = MenuItem::with_id(
            app,
            format!("preset:{}", p.name),
            format!("{} — {}", p.name, p.description),
            true,
            None::<&str>,
        )?;
        presets_menu.append(&item)?;
    }
    let login = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(app, "autostart", "Launch at Login", true, login, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit mint", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &generate,
            &copy,
            &open,
            &presets_menu,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    *app.state::<AppState>().login_item.lock().unwrap() = Some(autostart);
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("mint")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "generate" => show_window(app, true),
            "open" => show_window(app, false),
            "copy" => {
                let rule = app.state::<AppState>().rule.lock().unwrap().clone();
                copy_from_menu(app, rule);
            }
            "autostart" => {
                let want = !app.autolaunch().is_enabled().unwrap_or(false);
                set_launch_at_login(app, want);
            }
            "quit" => app.exit(0),
            id => {
                if let Some(name) = id.strip_prefix("preset:") {
                    if let Ok(rule) = presets::find(name).and_then(|p| p.spec.to_rule(&p.name)) {
                        copy_from_menu(app, rule);
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn register_hotkey(app: &AppHandle) {
    let state = app.state::<AppState>();
    let combo = state.settings.hotkey.clone();
    let result =
        app.global_shortcut().register(combo.as_str()).map(|_| combo.clone()).map_err(|e| {
            format!("Could not register the hotkey {combo} ({e}); set another with `hotkey` in config.toml.")
        });
    match &result {
        Ok(k) => log(&format!("hotkey registered: {k}")),
        Err(e) => log(&format!("hotkey failed: {e}")),
    }
    *state.hotkey.lock().unwrap() = result;
}

fn initial_rule(settings: &Settings) -> Rule {
    settings
        .default_preset
        .as_deref()
        .and_then(|name| presets::find(name).ok())
        .and_then(|p| p.spec.to_rule(&p.name).ok())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn nspanel_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_nspanel::init()
}

/// Elsewhere a no-op plugin keeps the builder chain the same.
#[cfg(not(target_os = "macos"))]
fn nspanel_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("mint-panel").build()
}

fn main() {
    let (settings, settings_error) = match Settings::load() {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(e.message().to_string())),
    };
    if let Some(e) = &settings_error {
        log(&format!("config ignored: {e}"));
    }
    let state = AppState {
        rule: Mutex::new(initial_rule(&settings)),
        settings,
        last_password: Mutex::new(None),
        op_running: AtomicUsize::new(0),
        hotkey: Mutex::new(Err("not registered yet".into())),
        login_item: Mutex::new(None),
    };

    tauri::Builder::default()
        // Must come first: a second launch hands its arguments over and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || handle_args(&handle, &argv));
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_FLAG])))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })
                .build(),
        )
        .manage(state)
        .plugin(nspanel_plugin())
        .invoke_handler(tauri::generate_handler![
            init,
            generate,
            presets_list,
            copy,
            vaults,
            save,
            open_item,
            set_height,
            hide
        ])
        .on_window_event(|window, event| {
            #[cfg(not(target_os = "macos"))]
            if let WindowEvent::Focused(false) = event {
                if should_hide_on_blur(window.app_handle()) {
                    let _ = window.hide();
                }
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_window(window.app_handle());
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                panel::install(&handle)?;
            }
            let theme = match handle.state::<AppState>().settings.theme {
                Theme::System => None,
                Theme::Light => Some(tauri::Theme::Light),
                Theme::Dark => Some(tauri::Theme::Dark),
            };
            if let Some(w) = main_window(&handle) {
                let _ = w.set_theme(theme);
            }
            build_tray(&handle)?;
            register_hotkey(&handle);
            log(&format!("started, version {}", env!("CARGO_PKG_VERSION")));
            let args: Vec<String> = std::env::args().collect();
            handle_args(&handle, &args);
            if demo().as_deref() == Some("copy") {
                let rule = handle.state::<AppState>().rule.lock().unwrap().clone();
                copy_from_menu(&handle, rule);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("mint failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn plain_launch_shows_the_window() {
        assert_eq!(parse_args(&args(&["mint-app"])), LaunchAction::Show);
    }

    #[test]
    fn hidden_launch_stays_in_the_menu_bar() {
        assert_eq!(parse_args(&args(&["mint-app", "--hidden"])), LaunchAction::Stay);
    }

    #[test]
    fn toggle_wins_over_hidden() {
        assert_eq!(parse_args(&args(&["mint-app", "--hidden", "--toggle"])), LaunchAction::Toggle);
    }

    #[test]
    fn launch_at_login_reads_on_and_off() {
        assert_eq!(parse_args(&args(&["mint-app", "--launch-at-login", "on"])), LaunchAction::LaunchAtLogin(true));
        assert_eq!(parse_args(&args(&["mint-app", "--launch-at-login", "off"])), LaunchAction::LaunchAtLogin(false));
        assert_eq!(parse_args(&args(&["mint-app", "--launch-at-login"])), LaunchAction::LaunchAtLogin(false));
    }

    #[test]
    fn launch_at_login_wins_over_other_flags() {
        let a = args(&["mint-app", "--toggle", "--launch-at-login", "on"]);
        assert_eq!(parse_args(&a), LaunchAction::LaunchAtLogin(true));
    }

    #[test]
    fn initial_rule_without_a_preset_is_the_default() {
        assert_eq!(initial_rule(&Settings::default()), Rule::default());
    }

    #[test]
    fn initial_rule_follows_default_preset() {
        let settings = Settings { default_preset: Some("pin6".into()), ..Settings::default() };
        let rule = initial_rule(&settings);
        assert_eq!(rule.kind, Kind::Pin);
        assert_eq!(rule.length, 6);
    }

    #[test]
    fn initial_rule_ignores_an_unknown_preset() {
        let settings = Settings { default_preset: Some("no-such-preset".into()), ..Settings::default() };
        assert_eq!(initial_rule(&settings), Rule::default());
    }

    #[test]
    fn demo_save_never_calls_create() {
        let login = NewLogin { title: "Example".into(), vault: Some("demo2".into()), ..NewLogin::default() };
        let item = save_login(true, login, "secret", |_, _| panic!("demo mode reached 1Password")).unwrap();
        assert_eq!(item.title, "Example");
        assert!(item.link.is_none());
    }

    #[test]
    fn real_save_passes_the_login_and_password_through() {
        let login = NewLogin { title: "Example".into(), ..NewLogin::default() };
        let item = save_login(false, login, "secret", |l, p| {
            assert_eq!((l.title.as_str(), p), ("Example", "secret"));
            Ok(SavedItem {
                id: "i".into(),
                title: l.title.clone(),
                vault_id: "v".into(),
                vault: "V".into(),
                link: None,
                updated: false,
            })
        })
        .unwrap();
        assert_eq!(item.id, "i");
    }
}
