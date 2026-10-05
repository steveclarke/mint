//! Clipboard writes that clipboard managers and history are told to skip.
//!
//! - macOS: the string plus `org.nspasteboard.ConcealedType` and
//!   `org.nspasteboard.TransientType` markers (nspasteboard.org convention).
//! - Linux: `wl-copy --sensitive`, which offers `x-kde-passwordManagerHint`
//!   (wl-clipboard 2.3+); X11 falls back to `xclip` with no hint.
//! - Windows: `ExcludeClipboardContentFromMonitorProcessing`, plus
//!   `CanIncludeInClipboardHistory` and `CanUploadToCloudClipboard` set to 0.
//!
//! Clearing only happens if the clipboard still holds what mint wrote: on
//! macOS and Windows that is checked with the clipboard's change counter (no
//! read of the contents); on Linux by comparing the text.

use crate::error::{Error, Result};

/// What a copy left behind, for a later [`clear_if_unchanged`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied {
    /// macOS `changeCount` or Windows clipboard sequence number after the
    /// write; 0 on Linux, which compares contents instead.
    pub token: i64,
    /// Whether the concealment hint was set. False only on Linux when the
    /// installed tool cannot set it.
    pub concealed: bool,
}

pub fn copy_concealed(secret: &str) -> Result<Copied> {
    imp::copy(secret)
}

/// Clears the clipboard if it still holds `secret` (or, where a change
/// counter exists, if nothing has been copied since). Returns whether it cleared.
pub fn clear_if_unchanged(copied: Copied, secret: &str) -> Result<bool> {
    imp::clear_if_unchanged(copied, secret)
}

fn failed(detail: impl std::fmt::Display) -> Error {
    Error::Clipboard(format!(
        "Could not use the clipboard ({detail}); print the password instead by leaving out --copy."
    ))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Copied, failed};
    use crate::error::Result;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSString};

    pub const CONCEALED: &str = "org.nspasteboard.ConcealedType";
    pub const TRANSIENT: &str = "org.nspasteboard.TransientType";

    pub fn copy(secret: &str) -> Result<Copied> {
        let pb = NSPasteboard::generalPasteboard();
        let concealed = NSString::from_str(CONCEALED);
        let transient = NSString::from_str(TRANSIENT);
        let string_type = unsafe { NSPasteboardTypeString };
        let types = NSArray::from_slice(&[string_type, &*concealed, &*transient]);
        pb.clearContents();
        unsafe { pb.declareTypes_owner(&types, None) };
        let ok = pb.setString_forType(&NSString::from_str(secret), string_type)
            && pb.setString_forType(&NSString::from_str(""), &concealed)
            && pb.setString_forType(&NSString::from_str(""), &transient);
        if !ok {
            return Err(failed("the pasteboard refused the write"));
        }
        Ok(Copied { token: pb.changeCount() as i64, concealed: true })
    }

    pub fn clear_if_unchanged(copied: Copied, _secret: &str) -> Result<bool> {
        let pb = NSPasteboard::generalPasteboard();
        if pb.changeCount() as i64 != copied.token {
            return Ok(false);
        }
        pb.clearContents();
        Ok(true)
    }
}

#[cfg(windows)]
mod imp {
    use super::{Copied, failed};
    use crate::error::Result;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
        SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    struct Open;
    impl Open {
        fn new() -> Result<Open> {
            for _ in 0..10 {
                if unsafe { OpenClipboard(null_mut()) } != 0 {
                    return Ok(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(failed("another program has the clipboard open"))
        }
    }
    impl Drop for Open {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Copies `bytes` into a movable global block and hands it to the clipboard.
    unsafe fn set(format: u32, bytes: &[u8]) -> Result<()> {
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if mem.is_null() {
                return Err(failed("out of memory"));
            }
            let ptr = GlobalLock(mem) as *mut u8;
            if ptr.is_null() {
                GlobalFree(mem);
                return Err(failed("could not lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            GlobalUnlock(mem);
            if SetClipboardData(format, mem).is_null() {
                GlobalFree(mem);
                return Err(failed("the clipboard refused the write"));
            }
            Ok(())
        }
    }

    pub fn copy(secret: &str) -> Result<Copied> {
        let _open = Open::new()?;
        unsafe {
            EmptyClipboard();
            let text: Vec<u8> = wide(secret).iter().flat_map(|u| u.to_le_bytes()).collect();
            set(CF_UNICODETEXT as u32, &text)?;
            let zero = 0u32.to_le_bytes();
            set(RegisterClipboardFormatW(wide("ExcludeClipboardContentFromMonitorProcessing").as_ptr()), &zero)?;
            set(RegisterClipboardFormatW(wide("CanIncludeInClipboardHistory").as_ptr()), &zero)?;
            set(RegisterClipboardFormatW(wide("CanUploadToCloudClipboard").as_ptr()), &zero)?;
        }
        drop(_open);
        Ok(Copied { token: unsafe { GetClipboardSequenceNumber() } as i64, concealed: true })
    }

    pub fn clear_if_unchanged(copied: Copied, _secret: &str) -> Result<bool> {
        if unsafe { GetClipboardSequenceNumber() } as i64 != copied.token {
            return Ok(false);
        }
        let _open = Open::new()?;
        unsafe { EmptyClipboard() };
        Ok(true)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::{Copied, failed};
    use crate::error::Result;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
    }

    /// Runs a clipboard tool with `input` on stdin. Its output streams go to
    /// null: wl-copy and xclip fork a server that keeps them open for as long
    /// as it owns the clipboard, so capturing them would block.
    fn pipe_to(program: &str, args: &[&str], input: &[u8]) -> std::io::Result<std::process::ExitStatus> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        child.stdin.take().expect("stdin is piped").write_all(input)?;
        child.wait()
    }

    fn missing(tool: &str, package: &str) -> crate::error::Error {
        failed(format!("{tool} is not installed; install {package}"))
    }

    pub fn copy(secret: &str) -> Result<Copied> {
        if wayland() {
            let out = pipe_to("wl-copy", &["--sensitive", "--type", "text/plain"], secret.as_bytes())
                .map_err(|_| missing("wl-copy", "wl-clipboard"))?;
            if out.success() {
                return Ok(Copied { token: 0, concealed: true });
            }
            // wl-clipboard older than 2.3 has no --sensitive.
            let out = pipe_to("wl-copy", &["--type", "text/plain"], secret.as_bytes())
                .map_err(|_| missing("wl-copy", "wl-clipboard"))?;
            if out.success() {
                return Ok(Copied { token: 0, concealed: false });
            }
            return Err(failed(format!("wl-copy exited with {out}")));
        }
        let out = pipe_to("xclip", &["-selection", "clipboard", "-in"], secret.as_bytes())
            .map_err(|_| missing("xclip", "xclip"))?;
        if out.success() {
            Ok(Copied { token: 0, concealed: false })
        } else {
            Err(failed(format!("xclip exited with {out}")))
        }
    }

    pub fn clear_if_unchanged(_copied: Copied, secret: &str) -> Result<bool> {
        let (read, clear): (&[&str], &[&str]) = if wayland() {
            (&["wl-paste", "--no-newline", "--type", "text/plain"], &["wl-copy", "--clear"])
        } else {
            (&["xclip", "-selection", "clipboard", "-out"], &["xclip", "-selection", "clipboard", "-in"])
        };
        let current = Command::new(read[0]).args(&read[1..]).stderr(Stdio::null()).output().map_err(failed)?;
        let mut held = current.stdout;
        let same = held == secret.as_bytes();
        zeroize::Zeroize::zeroize(&mut held);
        if !same {
            return Ok(false);
        }
        let out = pipe_to(clear[0], &clear[1..], b"").map_err(failed)?;
        Ok(out.success())
    }
}
