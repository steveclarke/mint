//! Bounded Linux clipboard I/O. Production callers supply fixed executable paths.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const DEADLINE: Duration = Duration::from_secs(5);

fn nonblocking(fd: i32) -> io::Result<i32> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags)
}

fn tick(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "clipboard deadline exceeded"));
    }
    std::thread::sleep(Duration::from_millis(5));
    Ok(())
}

/// Owns an unreaped child, so its process-group ID cannot be reused during cleanup.
struct Owned(Option<Child>);
impl Drop for Owned {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

fn write_input(child: &mut Child, input: &[u8], deadline: Instant) -> io::Result<()> {
    let mut pipe = child.stdin.take().ok_or_else(|| io::Error::other("missing stdin pipe"))?;
    nonblocking(pipe.as_raw_fd())?;
    let mut pending = input;
    while !pending.is_empty() {
        match pipe.write(pending) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "closed stdin")),
            Ok(n) => pending = &pending[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e),
        }
        tick(deadline)?;
    }
    Ok(())
}

/// Hands a secret to an already detached child; failed handoffs kill and reap it.
pub fn handoff(child: Child, input: &[u8]) -> io::Result<()> {
    handoff_until(child, input, DEADLINE)
}

fn handoff_until(child: Child, input: &[u8], timeout: Duration) -> io::Result<()> {
    let mut owned = Owned(Some(child));
    let deadline = Instant::now() + timeout;
    write_input(owned.0.as_mut().unwrap(), input, deadline)?;
    let mut pipe =
        owned.0.as_mut().unwrap().stdout.take().ok_or_else(|| io::Error::other("missing acknowledgment pipe"))?;
    nonblocking(pipe.as_raw_fd())?;
    loop {
        let mut ack = [0u8; 1];
        match pipe.read(&mut ack) {
            Ok(1) if ack == [1] => break,
            Ok(_) => return Err(io::Error::other("clearer did not acknowledge startup")),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e),
        }
        tick(deadline)?;
    }
    // The delayed clearer is intentionally independent after stdin closes.
    drop(owned.0.take());
    Ok(())
}

pub fn run(command: &mut Command, input: &[u8], cap: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    run_until(command, input, cap, DEADLINE)
}

fn run_until(command: &mut Command, input: &[u8], cap: usize, timeout: Duration) -> io::Result<Zeroizing<Vec<u8>>> {
    command.process_group(0).stdin(Stdio::piped()).stderr(Stdio::null());
    command.stdout(if cap == 0 { Stdio::null() } else { Stdio::piped() });
    let mut owned = Owned(Some(command.spawn()?));
    let deadline = Instant::now() + timeout;
    write_input(owned.0.as_mut().unwrap(), input, deadline)?;
    let mut result = Zeroizing::new(Vec::new());
    if let Some(mut pipe) = owned.0.as_mut().unwrap().stdout.take() {
        nonblocking(pipe.as_raw_fd())?;
        loop {
            let mut chunk = Zeroizing::new([0u8; 4096]);
            let amount = (cap + 1 - result.len()).min(chunk.len());
            match pipe.read(&mut chunk[..amount]) {
                Ok(0) => break,
                Ok(n) => {
                    result.extend_from_slice(&chunk[..n]);
                    if result.len() > cap {
                        return Err(io::Error::other("clipboard output exceeds limit"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => (),
                Err(e) => return Err(e),
            }
            tick(deadline)?;
        }
    }
    // WNOWAIT observes completion without releasing the PID before cleanup.
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let code = unsafe {
            libc::waitid(
                libc::P_PID,
                owned.0.as_ref().unwrap().id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if code < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } != 0 {
            if info.si_code != libc::CLD_EXITED || unsafe { info.si_status() } != 0 {
                return Err(io::Error::other("clipboard command failed"));
            }
            // Successful wl-copy forks its clipboard owner, which must survive.
            let _ = owned.0.take().unwrap().wait()?;
            return Ok(result);
        }
        tick(deadline)?;
    }
}

/// Reads stdin with a byte ceiling and a deadline, including pipes that never close.
pub fn read_stdin(cap: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let fd = libc::STDIN_FILENO;
    let flags = nonblocking(fd)?;
    struct Restore(i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, self.0);
            }
        }
    }
    let _restore = Restore(flags);
    let deadline = Instant::now() + DEADLINE;
    let mut result = Zeroizing::new(Vec::new());
    loop {
        let mut chunk = Zeroizing::new([0u8; 4096]);
        let amount = (cap + 1 - result.len()).min(chunk.len());
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), amount) };
        if n == 0 {
            return Ok(result);
        }
        if n > 0 {
            result.extend_from_slice(&chunk[..n as usize]);
            if result.len() > cap {
                return Err(io::Error::other("stdin exceeds limit"));
            }
        } else {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::WouldBlock && e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
        tick(deadline)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python(code: &str) -> Command {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-I", "-S", "-c", code]).env("MINT_OP", "/usr/bin/false");
        command
    }

    #[test]
    fn secret_handoff_and_output_are_exact_and_bounded() {
        let secret = b" invented\nfixture\t";
        let out =
            run(&mut python("import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())"), secret, secret.len())
                .unwrap();
        assert_eq!(out.as_slice(), secret);
        let error = run(&mut python("import sys; sys.stdout.write('x'*65536)"), b"", 32).unwrap_err();
        assert!(!error.to_string().contains("xxxx"));
    }

    #[test]
    fn stalled_input_and_inherited_output_have_deadlines() {
        for (code, input, cap) in [
            ("import time; time.sleep(10)", vec![0; 1048576], 0),
            ("import os,time; p=os.fork(); time.sleep(10) if p==0 else None", vec![], 32),
            ("import os,time; os.close(1); time.sleep(10)", vec![], 32),
        ] {
            let start = Instant::now();
            let error = run_until(&mut python(code), &input, cap, Duration::from_millis(100)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn failure_reaps_owned_child_and_stops_its_group() {
        let mut command = python("import os,time; os.fork(); time.sleep(10)");
        command.process_group(0).stdin(Stdio::piped());
        let child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let mut owned = Owned(Some(child));
        assert!(
            write_input(owned.0.as_mut().unwrap(), &vec![0; 1048576], Instant::now() + Duration::from_millis(100))
                .is_err()
        );
        drop(owned);
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn clearer_acknowledges_exact_stdin_before_detaching() {
        let mut command = python(
            "import sys,time; assert sys.stdin.buffer.read()==b'invented'; sys.stdout.buffer.write(bytes([1])); sys.stdout.flush(); time.sleep(.05)",
        );
        let child = command.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let pid = child.id() as i32;
        handoff_until(child, b"invented", Duration::from_secs(1)).unwrap();
        // The acknowledged child remains owned by this test until it exits.
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert_eq!(status, 0);
    }

    #[test]
    fn missing_wrong_or_stalled_ack_kills_and_reaps_clearer() {
        for code in [
            "import sys; sys.stdin.read()",
            "import sys,time; sys.stdin.read(); sys.stdout.write('x'); sys.stdout.flush(); time.sleep(10)",
            "import sys,time; sys.stdin.read(); time.sleep(10)",
        ] {
            let mut command = python(code);
            let child = command.process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            let pid = child.id() as i32;
            let start = Instant::now();
            assert!(handoff_until(child, b"invented", Duration::from_millis(100)).is_err());
            assert!(start.elapsed() < Duration::from_secs(2));
            assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
        }
    }

    #[test]
    fn command_failure_does_not_return_secret_output() {
        let error = run(&mut python("import sys; print('invented-secret'); sys.exit(1)"), b"", 64).unwrap_err();
        assert!(!error.to_string().contains("invented-secret"));
    }
}
