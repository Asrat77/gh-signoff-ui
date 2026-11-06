//! Spawning processes.
//!
//! Every shell-out goes through one [`Runner`], so PATH, the working
//! directory and timeouts are handled in one place, and tests can replay
//! recorded `gh` output instead of reaching the network.

use std::io::{self, Read, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
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

/// Runs a program to completion.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args` in `dir` (the current directory when
    /// `None`), feeding `input` on stdin.
    ///
    /// # Errors
    ///
    /// Only when the program could not be started at all.
    fn run(&self, dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output>;
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
        // A GUI app on Windows must not flash a console for every `gh` call.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

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
