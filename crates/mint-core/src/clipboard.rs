//! Clipboard writes that clipboard managers and history are told to skip.
//!
//! - macOS: the string plus `org.nspasteboard.ConcealedType` and
//!   `org.nspasteboard.TransientType` markers (nspasteboard.org convention).
//! - Linux: `wl-copy --sensitive`, which offers `x-kde-passwordManagerHint`
//!   (wl-clipboard 2.3+); unsupported sessions fail without an unhinted copy.
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
    /// Whether the concealment hint was set.
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
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
        SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
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

#[cfg(target_os = "linux")]
mod imp {
    use super::{Copied, failed};
    use crate::{clipboard_process, error::Result};
    use std::process::Command;

    pub fn copy(secret: &str) -> Result<Copied> {
        if !std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
            return Err(failed("concealed copy requires Wayland and wl-clipboard 2.3 or later"));
        }
        copy_using(secret, &mut Command::new("/usr/bin/wl-copy"))
    }

    fn copy_using(secret: &str, command: &mut Command) -> Result<Copied> {
        clipboard_process::run(command.args(["--sensitive", "--type", "text/plain"]), secret.as_bytes(), 0)
            .map_err(|_| failed("sensitive copy failed; wl-clipboard 2.3 or later is required"))?;
        Ok(Copied { token: 0, concealed: true })
    }

    pub fn clear_if_unchanged(_copied: Copied, secret: &str) -> Result<bool> {
        clear_using(secret, &mut Command::new("/usr/bin/wl-paste"), &mut Command::new("/usr/bin/wl-copy"))
    }

    fn clear_using(secret: &str, read: &mut Command, clear: &mut Command) -> Result<bool> {
        let current = clipboard_process::run(read.args(["--no-newline", "--type", "text/plain"]), b"", secret.len())
            .map_err(failed)?;
        if current.as_slice() != secret.as_bytes() {
            return Ok(false);
        }
        clipboard_process::run(clear.arg("--clear"), b"", 0).map_err(failed)?;
        Ok(true)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn stub(code: &str) -> Command {
            let mut command = Command::new("/usr/bin/python3");
            command.args(["-I", "-S", "-c", code]).env("MINT_OP", "/usr/bin/false");
            command
        }
        #[test]
        fn copies_only_with_hint_and_stdin() {
            let mut command = stub(
                "import sys; assert sys.argv[1:]==['--sensitive','--type','text/plain']; assert sys.stdin.read()==' invented\\n'",
            );
            assert!(copy_using(" invented\n", &mut command).unwrap().concealed);
            assert!(copy_using("invented", &mut stub("import sys; sys.exit(1)")).is_err());
        }
        #[test]
        fn changed_or_oversized_clipboard_is_never_cleared() {
            assert!(
                !clear_using("invented", &mut stub("print('changed',end='')"), &mut Command::new("/nonexistent"))
                    .unwrap()
            );
            assert!(
                clear_using("invented", &mut stub("print('x'*100000,end='')"), &mut Command::new("/nonexistent"))
                    .is_err()
            );
        }
        #[test]
        fn unchanged_clipboard_is_cleared() {
            let mut clear = stub("import sys; assert sys.argv[1:]==['--clear']; assert sys.stdin.read()==''");
            assert!(clear_using("invented", &mut stub("print('invented',end='')"), &mut clear).unwrap());
        }
    }
}
