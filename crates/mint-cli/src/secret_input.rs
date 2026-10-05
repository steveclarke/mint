//! Bounded, deadline-limited stdin. Clipboard-tool deadlines are separate.
use std::io;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const STDIN_DEADLINE: Duration = Duration::from_secs(120);
const LIMIT: usize = 16384;

/// Normalization belongs only to the public copy command, never the clearer.
pub fn copy_value(bytes: &[u8]) -> mint_core::Result<&str> {
    if bytes.len() > LIMIT || bytes.contains(&0) {
        return Err(mint_core::Error::Usage("Stdin must contain at most 16384 UTF-8 bytes without NUL.".into()));
    }
    let value =
        std::str::from_utf8(bytes).map_err(|_| mint_core::Error::Usage("Stdin must contain valid UTF-8.".into()))?;
    let value = value.strip_suffix("\r\n").or_else(|| value.strip_suffix('\n')).unwrap_or(value);
    if value.is_empty() {
        return Err(mint_core::Error::Usage("Stdin must not be empty after removing one trailing newline.".into()));
    }
    Ok(value)
}

pub fn read_stdin(timeout: Duration) -> io::Result<Zeroizing<Vec<u8>>> {
    let deadline = Instant::now() + timeout;
    let input = platform::Input::new()?;
    // Fixed capacity avoids leaving secret bytes in abandoned allocations.
    let mut result = Zeroizing::new(Vec::with_capacity(LIMIT + 1));
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "stdin deadline exceeded"));
        }
        let mut chunk = Zeroizing::new([0u8; 4096]);
        let amount = (LIMIT + 1 - result.len()).min(chunk.len());
        match input.read(&mut chunk[..amount]) {
            Ok(0) => return Ok(result),
            Ok(n) => {
                result.extend_from_slice(&chunk[..n]);
                if result.len() > LIMIT {
                    return Err(io::Error::other("stdin exceeds limit"));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::io;

    pub struct Input(i32);
    impl Input {
        pub fn new() -> io::Result<Self> {
            let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(flags))
        }

        pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let n = unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
        }
    }
    impl Drop for Input {
        fn drop(&mut self) {
            unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, self.0) };
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType, ReadFile};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    pub struct Input {
        handle: HANDLE,
        pipe: bool,
    }
    impl Input {
        pub fn new() -> io::Result<Self> {
            let handle = io::stdin().as_raw_handle();
            let kind = unsafe { GetFileType(handle) };
            if kind != FILE_TYPE_DISK && kind != FILE_TYPE_PIPE {
                return Err(io::Error::other("stdin must be a file or pipe"));
            }
            Ok(Self { handle, pipe: kind == FILE_TYPE_PIPE })
        }

        pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut amount = buffer.len() as u32;
            if self.pipe {
                let mut available = 0;
                // This CLI has one stdin reader. Never issue a blocking pipe read
                // before bytes are available; no worker survives a timeout.
                if unsafe { PeekNamedPipe(self.handle, null_mut(), 0, null_mut(), &mut available, null_mut()) } == 0 {
                    return eof_or_error();
                }
                if available == 0 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                amount = amount.min(available);
            }
            let mut read = 0;
            if unsafe { ReadFile(self.handle, buffer.as_mut_ptr(), amount, &mut read, null_mut()) } == 0 {
                return eof_or_error();
            }
            Ok(read as usize)
        }
    }

    fn eof_or_error() -> io::Result<usize> {
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|n| n as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF) => Ok(0),
            _ => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    #[test]
    fn copy_trims_exactly_one_newline_and_preserves_everything_else() {
        for (input, expected) in [
            ("invented\n", "invented"),
            ("invented\r\n", "invented"),
            ("invented\n\n", "invented\n"),
            ("invented\r\n\r\n", "invented\r\n"),
            ("invented\r", "invented\r"),
            ("\n\n", "\n"),
            ("\r", "\r"),
            (" \t\n", " \t"),
            ("  invented\nsecret\t ", "  invented\nsecret\t "),
            ("🔑\n", "🔑"),
        ] {
            assert_eq!(copy_value(input.as_bytes()).unwrap(), expected);
        }
        for invalid in [b"".as_slice(), b"\n", b"\r\n", b"invented\0\n", &[0xff]] {
            assert!(copy_value(invalid).is_err());
        }
        assert_eq!(copy_value(&vec![b'x'; LIMIT]).unwrap().len(), LIMIT);
        assert!(copy_value(&[vec![b'x'; LIMIT], b"\n".to_vec()].concat()).is_err());
        assert_eq!(copy_value(&[vec![b'x'; LIMIT - 2], b"\r\n".to_vec()].concat()).unwrap().len(), LIMIT - 2);
    }

    #[test]
    fn stdin_deadlines_bounds_and_exact_handoff() {
        assert_eq!(STDIN_DEADLINE, Duration::from_secs(120));
        for case in ["empty", "exact", "limit", "overflow", "stall", "partial", "approval", "trickle"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "secret_input::tests::stdin_worker", "--ignored", "--nocapture"])
                .env("MINT_STDIN_TEST_CASE", case)
                .env("MINT_OP", "/nonexistent/mint-test/op")
                .stdin(Stdio::piped())
                .stdout(if case == "approval" { Stdio::piped() } else { Stdio::null() })
                .spawn()
                .unwrap();
            let mut pipe = child.stdin.take().unwrap();
            let mut ready = child.stdout.take().map(io::BufReader::new);
            match case {
                "exact" => pipe.write_all(b" invented\r\n\n").unwrap(),
                "limit" => pipe.write_all(&vec![b'x'; LIMIT]).unwrap(),
                "overflow" => pipe.write_all(&vec![b'x'; LIMIT + 1]).unwrap(),
                "partial" => pipe.write_all(b"invented").unwrap(),
                "approval" => {
                    let reader = ready.as_mut().unwrap();
                    loop {
                        let mut line = String::new();
                        assert!(reader.read_line(&mut line).unwrap() > 0, "reader exited before readiness");
                        if line.trim() == "MINT_STDIN_READY" {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(40));
                    pipe.write_all(b"invented\n").unwrap();
                }
                _ => (),
            }
            let (held, trickle) = match case {
                "trickle" => (
                    None,
                    Some(std::thread::spawn(move || {
                        while pipe.write_all(b"x").is_ok() {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    })),
                ),
                "stall" | "partial" => (Some(pipe), None),
                _ => {
                    drop(pipe);
                    (None, None)
                }
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "stdin fixture {case} failed");
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("stdin fixture {case} exceeded its deadline");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(held);
            if let Some(writer) = trickle {
                writer.join().unwrap();
            }
        }
    }

    #[test]
    #[ignore = "subprocess fixture with exclusive stdin ownership"]
    fn stdin_worker() {
        let case = std::env::var("MINT_STDIN_TEST_CASE").unwrap();
        #[cfg(unix)]
        let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
        let start = Instant::now();
        if case == "approval" {
            std::io::stdout().write_all(b"\nMINT_STDIN_READY\n").unwrap();
            std::io::stdout().flush().unwrap();
        }
        let timeout = if case == "approval" { Duration::from_millis(500) } else { Duration::from_millis(100) };
        let result = read_stdin(timeout);
        #[cfg(unix)]
        assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, flags);
        match case.as_str() {
            "empty" => assert!(result.unwrap().is_empty()),
            "exact" => assert_eq!(result.unwrap().as_slice(), b" invented\r\n\n"),
            "limit" => assert_eq!(result.unwrap().len(), LIMIT),
            "overflow" => assert!(result.is_err()),
            "approval" => {
                assert_eq!(result.unwrap().as_slice(), b"invented\n");
                assert!(start.elapsed() >= Duration::from_millis(40));
            }
            "stall" | "partial" | "trickle" => {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
                assert!(start.elapsed() >= Duration::from_millis(100));
                assert!(start.elapsed() < Duration::from_secs(2));
            }
            _ => panic!("unknown fixture"),
        }
    }
}
