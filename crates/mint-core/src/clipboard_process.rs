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
    // A separate timeout supervisor retains the deadline if Mint is killed.
    // Parent death sends TERM to timeout, which terminates its tool group and
    // escalates after one second. Successful clipboard owners may stay alive.
    let mut supervisor = Command::new("/usr/bin/timeout");
    supervisor
        .args(["--kill-after=1", "--", &timeout.as_secs_f64().to_string()])
        .arg(command.get_program())
        .args(command.get_args());
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            supervisor.env(key, value);
        } else {
            supervisor.env_remove(key);
        }
    }
    supervisor.process_group(0).stdin(Stdio::piped()).stderr(Stdio::null());
    supervisor.stdout(if cap == 0 { Stdio::null() } else { Stdio::piped() });
    let parent = unsafe { libc::getpid() };
    unsafe {
        supervisor.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::other("clipboard parent exited"));
            }
            Ok(())
        });
    }
    let mut owned = Owned(Some(supervisor.spawn()?));
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
                return Err(io::Error::new(
                    if unsafe { info.si_status() } == 124 { io::ErrorKind::TimedOut } else { io::ErrorKind::Other },
                    "clipboard command failed",
                ));
            }
            // Successful wl-copy forks its clipboard owner, which must survive.
            let _ = owned.0.take().unwrap().wait()?;
            return Ok(result);
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
            // The independent watchdog can close stdin before the local timer fires.
            run_until(&mut python(code), &input, cap, Duration::from_millis(100)).unwrap_err();
            assert!(start.elapsed() >= Duration::from_millis(80));
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
    #[ignore = "subprocess fixture for parent-death cleanup"]
    fn parent_death_worker() {
        let path = std::env::var("MINT_PARENT_DEATH_FIXTURE").expect("fixture path required");
        let mut command = python(
            "import os,signal,sys,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); child=os.fork(); open(sys.argv[1], 'x').write(str(os.getpid())+' '+str(child)) if child else None; time.sleep(30)",
        );
        command.arg(path);
        let _ = run(&mut command, b"", 0);
    }

    #[test]
    fn killing_mint_stops_clipboard_descendants_even_when_they_ignore_term() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("mint-parent-death-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("pids");
        let worker = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "clipboard_process::tests::parent_death_worker", "--ignored"])
            .env("MINT_PARENT_DEATH_FIXTURE", &path)
            .env("MINT_OP", "/usr/bin/false")
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut worker = Owned(Some(worker));
        let deadline = Instant::now() + Duration::from_secs(3);
        let pids = loop {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                let ids: Vec<i32> = raw.split_whitespace().filter_map(|p| p.parse().ok()).collect();
                if ids.len() == 2 {
                    break ids;
                }
            }
            tick(deadline).unwrap();
        };
        // Kill the owning Mint test process, not the tool or timeout supervisor.
        worker.0.as_mut().unwrap().kill().unwrap();
        worker.0.take().unwrap().wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive = pids.iter().any(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .is_ok_and(|s| s.rsplit_once(") ").is_some_and(|(_, tail)| !tail.starts_with('Z')))
            });
            if !alive {
                break;
            }
            tick(deadline).expect("clipboard descendants survived parent death");
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn command_failure_does_not_return_secret_output() {
        let error = run(&mut python("import sys; print('invented-secret'); sys.exit(1)"), b"", 64).unwrap_err();
        assert!(!error.to_string().contains("invented-secret"));
    }
}
