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
| `--require N` | At least N classes must appear (N is 1 to 4; 0 is a usage error). mint satisfies this by using every enabled class and guaranteeing each one appears; it errors if fewer than N classes are enabled. | every enabled class |
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
- Errors go to stderr as one sentence that names the next step (`--json` errors are `{"error":..., "code":..., "kind":...}`, also on stderr; stdout stays empty on failure).

### JSON contract

- `mint --json`: one object.
  `{"password": str, "length": int, "kind": "chars"|"words"|"pin", "classes": ["upper"|"lower"|"digits"|"symbols", ...], "entropy_bits": float (1 decimal), "rule": {"preset": str|null, "summary": str}}`.
  `classes` lists the classes present in the password; `length` is in characters.
- `mint --count N --json`: an array of those objects. Any explicit `--count`, including 1, gives an array.
- `mint --copy --json`: the same object without `password`, plus `"copied": true` and `"clears_after": int|null` (seconds).
- `mint save --json`: the object without `password` (included with `--show`), plus `"id"`, `"title"`, `"vault"`, `"vault_id"`, `"link"` (1Password private link, or null) and `"updated"` (true with `--item`). With `--copy`, also `"copied": bool`; a clipboard failure after the item is saved gives `"copied": false`, a warning on stderr and exit 0.
- `mint save` plain output: the item ID, then the link, one per line, on stdout; a one-line confirmation on stderr.
- `mint presets --json`: an array of `{"name", "description", "source": "builtin"|"user", "summary"}`.
- `mint copy` reads 1–16384 UTF-8 bytes from stdin, preserves whitespace, rejects NUL, and uses the shared concealed copy and conditional clear. No secret argument or secret output is accepted. `--json` returns `{"copied":true,"clears_after":45}` (null when clearing is disabled); `--clear-after` and `--no-clear` apply.
- Errors (`kind`): `usage` (2), `unsatisfiable` (3), `onepassword` (4), `clipboard` (5).

### 1Password mechanism (proven against op 2.39)

- New Login: `op item create --vault V [--url U] --format json -` with the item JSON template (title, username, password) on stdin.
- Existing item (`--item`): `op item get ID --format json --reveal`, the password field replaced in memory, then `op item edit ID --format json` with that JSON on stdin. Items holding a passkey are refused: op's JSON templates drop passkeys.
- `op` is found through `MINT_OP`, else `PATH`, else `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`.

## Window

A small Tauri window, native-feeling, light and dark:
- the password in large monospace, with character classes tinted subtly
- a length slider plus a number field for any length
- class toggles, the allowed-symbols field, and a preset menu
- Regenerate, Copy, and Save to 1Password (a title field, a vault menu from `op vault list --format json`, and an optional URL and username)
- strength in bits
- Keyboard: Enter copies and hides, ⌘/Ctrl+R regenerates, ⌘/Ctrl+S saves, Esc hides. Opens with focus and a fresh password.

## Hotkey and menu bar

- macOS and Windows: a tray or menu bar icon (Generate, Copy new password, Open window, Presets, Quit), plus a global shortcut that toggles the window, registered by the app. The default is ⌃⌥⌘P on macOS (clear of Spotlight ⌘Space, 1Password ⇧⌘Space, Raycast ⌥Space) and Ctrl+Shift+Alt+P on Windows, set by `hotkey` in `~/.config/mint/config.toml` (`%APPDATA%\mint\config.toml`). Launches at login (opt-in toggle, on for Steve's install).
- `config.toml` keys: `hotkey`, `clear_after` (seconds, 0 = never), `hide_on_blur`, `default_preset`, `theme` (`system`, `light`, `dark`).
- macOS: the window is a non-activating panel (as Spotlight's is): it takes the keyboard over any app, including full-screen ones, without activating mint, so focus returns to the previous app when it hides. While `op` runs, losing focus to 1Password's approval prompt does not hide it.
- `mint-app --launch-at-login on|off` sets the login item from a script (installers); the menu has the same toggle.
- Linux (Hyprland): no app can register a global key under Wayland, so a compositor bind runs `mint gui --toggle`. The app is single-instance; a second launch toggles the first.

## Clipboard

- macOS: write with the `org.nspasteboard.ConcealedType` and `TransientType` markers so clipboard managers skip it.
- Linux: `wl-copy --sensitive` (wl-clipboard 2.3+) offers `x-kde-passwordManagerHint`, honoured by Omarchy Quattro clipboard history. Unsupported tools and non-Wayland sessions fail without an unhinted copy.
- Windows: set `ExcludeClipboardContentFromMonitorProcessing` and `CanIncludeInClipboardHistory = 0`.
- Linux clipboard tools use `/usr/bin/wl-copy` and `/usr/bin/wl-paste`, five-second I/O deadlines, bounded reads, and owned process-group cleanup on failure. The CLI waits for the clearer to acknowledge stdin before reporting a scheduled clear; scheduling failure conditionally removes the unchanged secret.
- Auto-clear after 45 seconds if the clipboard still holds the password (configurable; off with `--no-clear`). macOS and Windows check the clipboard change counter instead of reading the contents; Linux compares the text. The CLI starts a detached copy of itself for the delayed clear; only on Linux, where it compares contents, does it receive the password, over a pipe.

## Packaging

- macOS: a signed (ad hoc is acceptable for Steve's own machine) `.app` in `/Applications`, plus the `mint` CLI on PATH. A Homebrew tap formula or cask is a stretch goal.
- Linux: `mint` binary plus a desktop file; an AUR-style PKGBUILD in the repo.
- Windows: cross-compiled build in CI, marked untested on a real machine.
- CI on GitHub Actions: test on macOS, Linux and Windows.

## Omarchy plugin

A separate repo, `omarchy-mint` (marketplace rules: no compiled binaries, no agent files in the plugin tree). A bar glyph opens a panel with the same controls as the window, built from first-party Quattro parts. It calls `mint --json` and `mint save` and needs `mint` on PATH. It passes the `omarchy-plugin-security` grep audit.

## Done

Installed and used on the Mac Studio (menu bar, hotkey, window, CLI, save to 1Password) and on uber-om (CLI, window via Hyprland bind, Omarchy plugin, save to 1Password). A security review of randomness, clipboard handling and the `op` path has passed.
