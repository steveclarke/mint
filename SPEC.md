# mint — spec

One password engine with several ways in: a command line, a small window, a global hotkey, and an Omarchy bar widget. It saves straight into 1Password. It replaces the menu bar generator 1Password 8 removed.

## Principles

- **One engine.** Every way in calls the same Rust core. The Omarchy plugin calls the `mint` binary's JSON output; it never generates passwords itself.
- **Secrets never touch argv, logs or disk.** Passwords go to `op` and the clipboard over stdin or a pipe, and nowhere else.
- **Rules in one flag.** A site rule ("10 to 16 characters, 3 of 4 types") is one flag or one preset, never a settings hunt.
- **Agent-friendly.** Plain output by default, `--json` on request, exit codes that mean something, no prompts when stdin is not a TTY.

## Randomness

- Source: the operating system's CSPRNG through the `getrandom` crate (or `rand::rngs::OsRng`, which wraps it). That is `getentropy` on macOS, `getrandom(2)` on Linux and `ProcessPrng` on Windows. No userspace seeded PRNG holds password material.
- Character choice is unbiased: rejection sampling (or a library uniform-range sampler documented as unbiased), never `byte % n`.
- Required classes: one character from each required class is placed first, the rest are filled from the union of enabled classes, and the whole thing is Fisher–Yates shuffled with the same CSPRNG.
- Tests: distribution test (chi-square over a large sample, fixed tolerance), every required class always present across 100k generations, and no modulo in the sampling path.
- Entropy (bits) is reported in the CLI's JSON output and shown in the window.

## Generation options

| Option | Meaning | Default |
|---|---|---|
| length | Any positive integer, no 64 cap (sane upper limit 4096). A range `10-16` means "the site allows 10 to 16": mint uses the top of the range. | 24 |
| classes | `upper`, `lower`, `digits`, `symbols`; each can be switched off | all four |
| `--require N` | At least N classes must appear. mint satisfies this by using every enabled class and guaranteeing each one appears; it errors if fewer than N classes are enabled. | every enabled class |
| `--symbols SET` | The allowed symbol set, for sites that accept only some | `!@#$%^&*-_=+?` (builder confirms a broadly accepted set) |
| `--no-ambiguous` | Drops `0O1lI|` and similar | off |
| `--words N` | Memorable passphrase from an embedded EFF large wordlist, with separator and optional capital/digit | off |
| `--pin N` | Digits only | off |
| `--preset NAME` | A named rule set | none |

Presets: built-ins (e.g. `moneris` = 10-16, all four classes, 3 required; `pin6`; `wifi` = 63 chars without ambiguous characters) plus user presets in `~/.config/mint/presets.toml` (`%APPDATA%\mint\presets.toml` on Windows), one table per site. `mint presets` lists them.

## Command line

```
mint                         # one password, default rules, printed with a trailing newline
mint 32                      # length 32
mint 10-16 --require 3       # the Moneris rule, inline
mint --preset moneris
mint --json                  # {"password":..., "length":..., "classes":[...], "entropy_bits":..., "rule":...}
mint --copy                  # copy instead of print (prints nothing but a confirmation on stderr)
mint --count 5               # five, one per line (JSON array with --json)
mint save --title "Moneris" [--vault V] [--url U] [--username U] [--item ID] [generation flags]
mint gui [--toggle]          # open the window, or show/hide the running one
mint presets [--json]
```

- `mint save` generates a password and creates a Login item (or, with `--item`, sets that item's password field) through `op`, piping the secret in, never in argv. It prints the item ID and URL, not the password (`--show` prints it too). The builder proves the exact `op` mechanism (template over stdin, or an alternative) against the real account once, into a throwaway item it deletes afterwards, and records the run in the PR.
- Exit codes: 0 success; 2 usage error; 3 rule cannot be satisfied; 4 `op` missing, not signed in, or failed; 5 clipboard failed.
- Errors go to stderr as one sentence that names the next step (`--json` errors are `{"error":..., "code":...}`).

## Window

A small Tauri window, native-feeling, light and dark:
- the password in large monospace, with character classes tinted subtly
- a length slider plus a number field for any length
- class toggles, the allowed-symbols field, and a preset menu
- Regenerate, Copy, and Save to 1Password (a title field, a vault menu from `op vault list --format json`, and an optional URL and username)
- strength in bits
- Keyboard: Enter copies and hides, ⌘/Ctrl+R regenerates, ⌘/Ctrl+S saves, Esc hides. Opens with focus and a fresh password.

## Hotkey and menu bar

- macOS and Windows: a tray or menu bar icon (Generate, Copy new password, Open window, Presets, Quit), plus a global shortcut that toggles the window, registered by the app. The default is chosen so it doesn't clash with macOS, 1Password or Raycast, and is configurable. Launches at login (opt-in toggle, on for Steve's install).
- Linux (Hyprland): no app can register a global key under Wayland, so a compositor bind runs `mint gui --toggle`. The app is single-instance; a second launch toggles the first.

## Clipboard

- macOS: write with the `org.nspasteboard.ConcealedType` and `TransientType` markers so clipboard managers skip it.
- Linux: `wl-copy` with its sensitive/password-manager hint. Confirm that Omarchy's clipboard history (Walker/Elephant) honours it; if not, find what does.
- Windows: set `ExcludeClipboardContentFromMonitorProcessing` and `CanIncludeInClipboardHistory = 0`.
- Auto-clear after 45 seconds if the clipboard still holds the password (configurable; off with `--no-clear`).

## Packaging

- macOS: a signed (ad hoc is acceptable for Steve's own machine) `.app` in `/Applications`, plus the `mint` CLI on PATH. A Homebrew tap formula or cask is a stretch goal.
- Linux: `mint` binary plus a desktop file; an AUR-style PKGBUILD in the repo.
- Windows: cross-compiled build in CI, marked untested on a real machine.
- CI on GitHub Actions: test on macOS, Linux and Windows.

## Omarchy plugin

A separate repo, `omarchy-mint` (marketplace rules: no compiled binaries, no agent files in the plugin tree). A bar glyph opens a panel with the same controls as the window, built from first-party Quattro parts. It calls `mint --json` and `mint save` and needs `mint` on PATH. It passes the `omarchy-plugin-security` grep audit.

## Done

Installed and used on the Mac Studio (menu bar, hotkey, window, CLI, save to 1Password) and on uber-om (CLI, window via Hyprland bind, Omarchy plugin, save to 1Password). A security review of randomness, clipboard handling and the `op` path has passed.
