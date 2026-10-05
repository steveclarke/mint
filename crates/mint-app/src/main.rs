//! mint's window, menu bar icon and global hotkey. Generation, clipboard and
//! 1Password all go through `mint-core`, the same engine as the CLI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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

const WIDTH: f64 = 460.0;
/// Launch-at-login starts the app with this flag so it stays in the menu bar.
const HIDDEN_FLAG: &str = "--hidden";

struct AppState {
    settings: Settings,
    /// The rule the window used last; "Copy New Password" in the menu uses it too.
    last_rule: Mutex<Rule>,
    /// While op runs, losing focus (to 1Password's approval prompt) must not
    /// hide the window.
    op_running: AtomicBool,
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
    password: String,
}

/// Appends a line to mint's log (`~/Library/Logs/mint.log` on macOS, next to
/// the config elsewhere). Never logs secrets.
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}

fn generated(rule: &Rule) -> CmdResult<Generated> {
    let p = rule.generate()?;
    Ok(Generated {
        password: p.value.to_string(),
        length: p.length,
        kind: p.kind,
        classes: p.classes,
        entropy_bits: (p.entropy_bits * 10.0).round() / 10.0,
        summary: rule.summary(),
    })
}

#[tauri::command]
fn init(state: State<AppState>) -> Init {
    let hotkey = state.hotkey.lock().unwrap().clone();
    Init {
        rule: state.last_rule.lock().unwrap().clone(),
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
    let out = generated(&rule)?;
    *state.last_rule.lock().unwrap() = rule;
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
    let secret = zeroize::Zeroizing::new(secret.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(clear_after));
        let _ = clipboard::clear_if_unchanged(copied, &secret);
    });
    Ok(Some(clear_after))
}

#[tauri::command]
fn copy(password: String, state: State<AppState>) -> CmdResult<Option<u64>> {
    let password = zeroize::Zeroizing::new(password);
    copy_secret(&password, state.settings.clear_after)
}

/// Runs an op call off the main thread. 1Password may put up an approval
/// prompt, which takes focus: the window stays up meanwhile and takes the
/// keyboard back afterwards.
async fn with_op<T: Send + 'static>(
    app: &AppHandle,
    call: impl FnOnce() -> mint_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let state = app.state::<AppState>();
    state.op_running.store(true, Ordering::SeqCst);
    let result = tauri::async_runtime::spawn_blocking(call).await;
    state.op_running.store(false, Ordering::SeqCst);
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

#[tauri::command]
async fn save(request: SaveRequest, app: AppHandle) -> CmdResult<SavedItem> {
    let saved = with_op(&app, move || {
        let password = zeroize::Zeroizing::new(request.password);
        let login =
            NewLogin { title: request.title, vault: request.vault, url: request.url, username: request.username };
        onepassword::create_login(&login, &password)
    })
    .await?;
    log(&format!("saved item {} to vault {}", saved.id, saved.vault));
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
    state.settings.hide_on_blur && !state.op_running.load(Ordering::SeqCst)
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

/// Handles `--toggle` / `--show` from a second launch (and `mint gui`).
fn handle_args(app: &AppHandle, args: &[String]) {
    if let Some(i) = args.iter().position(|a| a == "--launch-at-login") {
        set_launch_at_login(app, args.get(i + 1).is_some_and(|v| v == "on"));
        return;
    }
    if args.iter().any(|a| a == "--toggle") {
        toggle_window(app);
    } else if !args.iter().any(|a| a == HIDDEN_FLAG) {
        show_window(app, true);
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
                let rule = app.state::<AppState>().last_rule.lock().unwrap().clone();
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
        last_rule: Mutex::new(initial_rule(&settings)),
        settings,
        op_running: AtomicBool::new(false),
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
                let rule = handle.state::<AppState>().last_rule.lock().unwrap().clone();
                copy_from_menu(&handle, rule);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("mint failed to start");
}
