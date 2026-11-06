//! Spawning processes.
//!
//! Every shell-out goes through one [`Runner`], so PATH, the working
//! directory and timeouts are handled in one place, and tests can replay
//! recorded `gh` output instead of reaching the network.

use std::io::{self, BufRead as _, BufReader, Read, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// What a finished process wrote and how it exited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// Standard output, trimmed.
    pub stdout: String,
    /// Standard error, trimmed.
    pub stderr: String,
    /// The exit code. A non-zero exit is data, not a failure to launch.
    pub code: i32,
}

/// Exit code reported for a process killed by its timeout, as `timeout(1)`.
pub const TIMED_OUT: i32 = 124;
/// Exit code reported for a process stopped by the user, as a shell does
/// for Ctrl-C.
pub const CANCELLED: i32 = 130;

/// How long output is still read after a process exits, for grandchildren
/// that inherited its pipes.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Runs a program to completion.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args` in `dir` (the current directory when
    /// `None`), feeding `input` on stdin.
    ///
    /// # Errors
    ///
    /// Only when the program could not be started at all.
    fn run(&self, dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output>;

    /// Runs `program` with `args` in `dir`, passing each line it writes to
    /// stdout or stderr to `on_line` as it arrives, and returns its exit
    /// code.
    ///
    /// The process, and everything it started, is killed after `timeout`
    /// ([`TIMED_OUT`]) or once `cancel` is set ([`CANCELLED`]).
    ///
    /// The default runs [`Runner::run`] and replays its output afterwards,
    /// which is enough for recorded output in tests.
    ///
    /// # Errors
    ///
    /// Only when the program could not be started at all.
    fn stream(
        &self,
        dir: Option<&Path>,
        program: &str,
        args: &[&str],
        timeout: Duration,
        cancel: &AtomicBool,
        on_line: &mut dyn FnMut(&str),
    ) -> io::Result<i32> {
        let _ = (timeout, cancel);
        let out = self.run(dir, None, program, args)?;
        for line in out.stdout.lines().chain(out.stderr.lines()) {
            on_line(line);
        }
        Ok(out.code)
    }
}

/// Runs real processes, killing any that outlive `timeout`.
#[derive(Clone, Copy, Debug)]
pub struct SystemRunner {
    /// How long one command may take.
    pub timeout: Duration,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(30) }
    }
}

impl Runner for SystemRunner {
    fn run(&self, dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        hide_console(&mut command);

        let mut child = command.spawn()?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            let input = input.to_owned();
            thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
            });
        }
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(15));
        };

        let Some(status) = status else {
            // A grandchild may still hold the pipes open, so the readers are
            // left to finish on their own rather than joined.
            return Ok(Output {
                stdout: String::new(),
                stderr: format!("{program} timed out after {}s", self.timeout.as_secs()),
                code: TIMED_OUT,
            });
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        Ok(Output {
            stdout: stdout.trim().to_owned(),
            stderr: stderr.trim().to_owned(),
            code: status.code().unwrap_or(-1),
        })
    }

    fn stream(
        &self,
        dir: Option<&Path>,
        program: &str,
        args: &[&str],
        timeout: Duration,
        cancel: &AtomicBool,
        on_line: &mut dyn FnMut(&str),
    ) -> io::Result<i32> {
        let mut command = Command::new(program);
        pass_args(&mut command, program, args);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        hide_console(&mut command);
        // Its own process group, so a timeout can stop everything a build
        // script started, not just the shell.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }

        let mut child = command.spawn()?;
        let (lines, inbox) = mpsc::channel();
        if let Some(pipe) = child.stdout.take() {
            forward_lines(pipe, lines.clone());
        }
        if let Some(pipe) = child.stderr.take() {
            forward_lines(pipe, lines);
        }

        let deadline = Instant::now() + timeout;
        let code = loop {
            while let Ok(line) = inbox.recv_timeout(Duration::from_millis(15)) {
                on_line(&line);
            }
            if let Some(status) = child.try_wait()? {
                break status.code().unwrap_or(-1);
            }
            if cancel.load(Ordering::Acquire) {
                kill_tree(&mut child);
                on_line("stopped");
                break CANCELLED;
            }
            if Instant::now() >= deadline {
                kill_tree(&mut child);
                on_line(&format!("timed out after {}", human_duration(timeout)));
                break TIMED_OUT;
            }
        };

        // Lines still in flight, until every writer closed its end or a
        // background grandchild is clearly keeping a pipe open.
        let grace = Instant::now() + DRAIN_GRACE;
        while let Some(left) = grace.checked_duration_since(Instant::now()) {
            match inbox.recv_timeout(left) {
                Ok(line) => on_line(&line),
                Err(_) => break,
            }
        }
        Ok(code)
    }
}

/// A GUI app on Windows must not flash a console for every process.
fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// `cmd` does its own parsing of `/S /C "..."`, which Rust's quoting for
/// ordinary programs would break, so its arguments go through verbatim.
fn pass_args(command: &mut Command, program: &str, args: &[&str]) {
    #[cfg(windows)]
    if program.eq_ignore_ascii_case("cmd") {
        use std::os::windows::process::CommandExt as _;
        for arg in args {
            command.raw_arg(arg);
        }
        return;
    }
    let _ = program;
    command.args(args);
}

/// Kills `child` and every process it started.
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    // `unsafe` is off, so the group is signalled through the system tools
    // rather than libc.
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    {
        let mut taskkill = Command::new("taskkill");
        taskkill.args(["/T", "/F", "/PID", &pid]).stdout(Stdio::null()).stderr(Stdio::null());
        hide_console(&mut taskkill);
        let _ = taskkill.status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
    let _ = child.kill();
    let _ = child.wait();
}

/// Sends each line `pipe` writes, without its line ending, until it closes.
fn forward_lines(pipe: impl Read + Send + 'static, lines: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if lines.send(line.trim_end_matches(['\r', '\n']).to_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// `90s`, `5m`, `1h 30m`.
pub fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match (secs / 3600, secs % 3600 / 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, 0, _) => format!("{h}h"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[cfg(test)]
pub(crate) mod fake {
    //! Recorded `gh` output for tests.

    use super::{Output, Runner};
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;

    /// One canned answer: used for the first command line containing `matches`.
    pub(crate) struct Reply {
        pub(crate) matches: &'static str,
        pub(crate) stdout: &'static str,
        pub(crate) stderr: &'static str,
        pub(crate) code: i32,
    }

    pub(crate) fn ok(matches: &'static str, stdout: &'static str) -> Reply {
        Reply { matches, stdout, stderr: "", code: 0 }
    }

    pub(crate) fn fail(matches: &'static str, stderr: &'static str) -> Reply {
        Reply { matches, stdout: "", stderr, code: 1 }
    }

    /// Replays [`Reply`]s and records every command line it was asked for.
    pub(crate) struct FakeRunner {
        replies: Vec<Reply>,
        pub(crate) commands: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        pub(crate) fn new(replies: Vec<Reply>) -> Self {
            Self { replies, commands: Mutex::new(Vec::new()) }
        }

        pub(crate) fn commands(&self) -> Vec<String> {
            self.commands.lock().map(|c| c.clone()).unwrap_or_default()
        }

        pub(crate) fn called(&self, needle: &str) -> bool {
            self.commands().iter().any(|c| c.contains(needle))
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, _dir: Option<&Path>, _input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
            let line = format!("{program} {}", args.join(" "));
            if let Ok(mut commands) = self.commands.lock() {
                commands.push(line.clone());
            }
            let reply = self.replies.iter().find(|r| line.contains(r.matches));
            match reply {
                Some(r) => Ok(Output { stdout: r.stdout.to_owned(), stderr: r.stderr.to_owned(), code: r.code }),
                None => panic!("unexpected command: {line}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_exit_code_and_trims_output() {
        let runner = SystemRunner::default();
        let (program, args): (&str, &[&str]) =
            if cfg!(windows) { ("cmd", &["/C", "echo hi & exit 3"]) } else { ("sh", &["-c", "echo '  hi  '; exit 3"]) };
        let out = runner.run(None, None, program, args).expect("spawn");
        assert_eq!(out.stdout, "hi");
        assert_eq!(out.code, 3);
    }

    #[test]
    fn a_missing_program_is_a_launch_error() {
        let runner = SystemRunner::default();
        assert!(runner.run(None, None, "signoff-no-such-program", &[]).is_err());
    }

    fn stream(program: &str, args: &[&str], timeout: Duration, cancel: &AtomicBool) -> (i32, Vec<String>) {
        let mut lines = Vec::new();
        let code = SystemRunner::default()
            .stream(None, program, args, timeout, cancel, &mut |line| lines.push(line.to_owned()))
            .expect("spawn");
        (code, lines)
    }

    #[test]
    fn streams_both_pipes_line_by_line() {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/D", "/S", "/C", "\"echo one& echo two 1>&2& exit 4\""])
        } else {
            ("sh", &["-c", "echo one; echo two >&2; exit 4"])
        };
        let (code, lines) = stream(program, args, Duration::from_secs(10), &AtomicBool::new(false));
        assert_eq!(code, 4);
        let mut sorted: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
        sorted.sort_unstable();
        assert_eq!(sorted, ["one", "two"]);
    }

    #[test]
    fn streaming_a_missing_program_is_a_launch_error() {
        let runner = SystemRunner::default();
        let cancel = AtomicBool::new(false);
        assert!(
            runner.stream(None, "signoff-no-such-program", &[], Duration::from_secs(1), &cancel, &mut |_| {}).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stream_timeout_kills_the_whole_process_group() {
        let started = Instant::now();
        let (code, lines) = stream(
            "sh",
            &["-c", "echo started; sleep 30 & sleep 30; echo never"],
            Duration::from_millis(300),
            &AtomicBool::new(false),
        );
        assert_eq!(code, TIMED_OUT);
        assert_eq!(lines.first().map(String::as_str), Some("started"));
        assert!(lines.iter().any(|l| l.starts_with("timed out")));
        assert!(!lines.iter().any(|l| l == "never"));
        // The background sleep held the pipes; it was killed with the group.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_stream_stops_when_cancelled() {
        let cancel = AtomicBool::new(true);
        let (code, lines) = stream("sh", &["-c", "sleep 30"], Duration::from_secs(60), &cancel);
        assert_eq!(code, CANCELLED);
        assert_eq!(lines.last().map(String::as_str), Some("stopped"));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(Duration::from_secs(45)), "45s");
        assert_eq!(human_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(human_duration(Duration::from_secs(1800)), "30m");
        assert_eq!(human_duration(Duration::from_secs(5400)), "1h 30m");
        assert_eq!(human_duration(Duration::from_secs(7200)), "2h");
    }

    #[cfg(unix)]
    #[test]
    fn kills_a_process_that_outlives_its_timeout() {
        let runner = SystemRunner { timeout: Duration::from_millis(150) };
        let started = Instant::now();
        let out = runner.run(None, None, "sh", &["-c", "sleep 30 & wait"]).expect("spawn");
        assert_eq!(out.code, TIMED_OUT);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
