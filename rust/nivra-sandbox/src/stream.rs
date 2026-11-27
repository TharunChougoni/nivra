//! A bounded, lockstep JSON-lines process transport, with a bubblewrap launcher.
use super::{Failure, SandboxConfig, cleanup, prepare_program};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsFd, AsRawFd},
    process::{Child, ChildStdin, ChildStdout, Stdio},
    time::{Duration, Instant},
};

pub struct Stream {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    wall: Duration,
    max_output: usize,
}

fn nonblocking(fd: &impl AsFd) -> Result<(), Failure> {
    // SAFETY: the pipe is owned and remains open during this call.
    if unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(Failure::Setup);
    }
    Ok(())
}

fn ready(fd: &impl AsFd, events: i16, until: Instant) -> Result<(), Failure> {
    let mut poll = libc::pollfd {
        fd: fd.as_fd().as_raw_fd(),
        events,
        revents: 0,
    };
    loop {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Failure::WallTimeout);
        }
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: poll points at one initialized descriptor.
        match unsafe { libc::poll(&mut poll, 1, millis) } {
            n if n > 0 => return Ok(()),
            0 => continue,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => (),
            _ => return Err(Failure::Exit),
        }
    }
}

impl Stream {
    /// Wrap an already-spawned child whose stdin and stdout are piped.
    pub fn from_pipes(
        mut child: Child,
        wall: Duration,
        max_output: usize,
    ) -> Result<Self, Failure> {
        let pipes = (|| {
            let input = child.stdin.take().ok_or(Failure::Setup)?;
            let output = child.stdout.take().ok_or(Failure::Setup)?;
            nonblocking(&input)?;
            nonblocking(&output)?;
            Ok((input, output))
        })();
        let (input, output) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                let pid = child.id() as i32;
                cleanup(&mut child, pid, false);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
            wall,
            max_output,
        })
    }

    pub fn start(
        config: &SandboxConfig,
        program: &[u8],
        initialization: &[u8],
        cpu_seconds: u64,
    ) -> Result<Self, Failure> {
        if config.network || !(1..=600).contains(&cpu_seconds) {
            return Err(Failure::Config);
        }
        let mut prepared = prepare_program(config, program, initialization, cpu_seconds)?;
        // Keep the mounted files until bwrap has consumed them.
        prepared.cmd.stdin(Stdio::piped());
        let child = prepared.cmd.spawn().map_err(|_| Failure::Setup)?;
        let mut stream = Self::from_pipes(
            child,
            Duration::from_millis(config.wall_ms),
            config.max_output_bytes,
        )?;
        let mut line = Vec::new();
        stream.read_line_within(&mut line, stream.wall)?;
        if line != b"ready\n" {
            return Err(Failure::OutputInvalid);
        }
        Ok(stream)
    }

    /// Read one newline-delimited frame before `timeout` elapses.
    pub fn read_line_within(
        &mut self,
        response: &mut Vec<u8>,
        timeout: Duration,
    ) -> Result<(), Failure> {
        self.read_line(response, Instant::now() + timeout)
    }

    fn read_line(&mut self, response: &mut Vec<u8>, until: Instant) -> Result<(), Failure> {
        response.clear();
        loop {
            let cap = self.max_output + 1 - response.len();
            match (&mut self.output)
                .take(cap as u64)
                .read_until(b'\n', response)
            {
                _ if response.len() > self.max_output => return Err(Failure::OutputLimit),
                Ok(_) if response.ends_with(b"\n") => return Ok(()),
                Ok(_) => return Err(Failure::Exit),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    ready(self.output.get_ref(), libc::POLLIN, until)?;
                }
                Err(_) => return Err(Failure::Exit),
            }
        }
    }

    /// Send one complete timestamp batch and wait for its actions.
    pub fn exchange(&mut self, line: &[u8], response: &mut Vec<u8>) -> Result<(), Failure> {
        self.exchange_within(line, response, self.wall)
    }

    /// Send one request line and read one response line before `timeout` elapses.
    pub fn exchange_within(
        &mut self,
        mut line: &[u8],
        response: &mut Vec<u8>,
        timeout: Duration,
    ) -> Result<(), Failure> {
        let until = Instant::now() + timeout;
        let input = self.input.as_mut().ok_or(Failure::Setup)?;
        while !line.is_empty() {
            ready(input, libc::POLLOUT, until)?;
            match input.write(line) {
                Ok(n) if n > 0 => line = &line[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                _ => return Err(Failure::Exit),
            }
        }
        self.read_line(response, until)
    }

    pub fn finish(mut self) -> Result<(), Failure> {
        drop(self.input.take());
        let until = Instant::now() + self.wall;
        loop {
            match self.output.fill_buf() {
                Ok([]) => break,
                Ok(_) => return Err(Failure::OutputInvalid),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    ready(self.output.get_ref(), libc::POLLIN, until)?;
                }
                Err(_) => return Err(Failure::Exit),
            }
        }
        while Instant::now() < until {
            if let Some(status) = self.child.try_wait().map_err(|_| Failure::Exit)? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Failure::Exit)
                };
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(Failure::WallTimeout)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let pid = self.child.id() as i32;
            cleanup(&mut self.child, pid, false);
        }
    }
}
