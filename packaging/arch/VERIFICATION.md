# Arch package verification

- Platform: Arch Linux, x86_64; verification date: 2026-10-05.
- Release source: `v0.1.0`, commit `0c4aed5bf5e195b2f08421b012e9d64c358eb414`.
- Build tools: devtools `1:1.5.1-1`, namcap `3.6.0-3`.
- Build environment: fresh `mkarchroot` root with the devtools `extra` repository configuration; `makechrootpkg` builds the pinned recipe with two Cargo jobs and `MINT_OP=/usr/bin/false` exported inside the chroot.
- Release build: CLI and Tauri application compile successfully in 6m 50s.
- Package checks: 47 tests pass; two subprocess fixtures remain marked ignored at the top level and run through their parent tests.
- Package revision 2: explicit linked-library and icon-theme dependencies replace implicit dependency satisfaction; Cargo-stripped release binaries have no separate debug package.
- Repackaging: `makechrootpkg -n -- --repackage` reuses the compiled source in the same chroot. All allocated ELF sections match revision 1; only `.gnu_debuglink` and the section-name table change.
- Namcap: no recipe findings, no package errors, no implicit-dependency warnings. Four runtime-dependency warnings remain as described below.
- Installed package: `mint 0.1.0-2`; `mint --version` reports `mint 0.1.0`.
- Package SHA-256: `80fbccd97726ac912b4bff42bdfc54c177f8850470df6746eea3375a96fffcee`.
- Pacman ownership and byte comparison pass for both binaries, the desktop file and icon; `pacman -Qk mint` reports no missing files.
- Native Wayland class: `mint-app`, matching the desktop entry's `StartupWMClass`. X11 `WM_CLASS` does not apply to this native Wayland run.
- Super+Ctrl+M: physical keycodes from a temporary input device exercise five hide/show transitions; each show focuses the same application process. The device is removed after the check.

## Installed files

| Path | Purpose |
| --- | --- |
| `/usr/bin/mint` | CLI and window launcher |
| `/usr/bin/mint-app` | Tauri application |
| `/usr/share/applications/mint.desktop` | Desktop entry |
| `/usr/share/icons/hicolor/256x256/apps/mint.png` | Application icon |
| `/usr/share/licenses/mint/LICENSE` | MIT license |

## Retained namcap warnings

| Dependency | Runtime use |
| --- | --- |
| `coreutils` | `/usr/bin/timeout` supervises clipboard processes. |
| `wl-clipboard` | `/usr/bin/wl-copy` and `/usr/bin/wl-paste` provide sensitive clipboard copying and conditional clearing. |
| `libappindicator-gtk3` | The tray integration loads `libappindicator3.so.1` dynamically. |
| `xdg-utils` | `xdg-open` opens a saved item's 1Password link. |
