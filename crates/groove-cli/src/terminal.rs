//! Bounded stderr delivery, independent of stdout results and persistent logging.

use std::{
    io::{self, IsTerminal, Write},
    os::fd::AsFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

const CHUNK_BYTES: usize = 8192;
const QUEUE_CHUNKS: usize = 128;
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy)]
pub struct Verbosity {
    pub verbose: u8,
    pub quiet: bool,
}
impl Verbosity {
    pub fn allows(self, level: tracing::Level) -> bool {
        let threshold = if self.quiet {
            tracing::Level::ERROR
        } else {
            match self.verbose {
                0 => tracing::Level::WARN,
                1 => tracing::Level::INFO,
                2 => tracing::Level::DEBUG,
                _ => tracing::Level::TRACE,
            }
        };
        level <= threshold
    }
}

#[derive(Debug)]
enum Message {
    Bytes(Vec<u8>),
    Finish(mpsc::Sender<()>),
}

/// A best-effort stderr sink. Never holds up the managed-worker supervisor.
#[derive(Debug, Clone)]
pub struct Terminal {
    sender: SyncSender<Message>,
    dropped: Arc<AtomicBool>,
    pub verbosity: Verbosity,
}
impl Terminal {
    /// Animate slow interactive queries on stderr. Dropping the guard clears the line.
    pub fn spinner(&self, json: bool, label: &str) -> Spinner {
        if json
            || self.verbosity.quiet
            || !io::stderr().is_terminal()
            || std::env::var_os("TERM").is_some_and(|term| term == "dumb")
        {
            return Spinner(None);
        }
        self.start_spinner(label)
    }

    fn start_spinner(&self, label: &str) -> Spinner {
        let (stop, receiver) = mpsc::channel();
        let terminal = self.clone();
        let label = label.to_owned();
        let worker = std::thread::Builder::new()
            .name("groove-progress".into())
            .spawn(move || {
                let mut frame = ["|", "/", "-", "\\"].into_iter().cycle();
                let mut visible = false;
                // Avoid flicker for quick queries. The channel wakes immediately on drop.
                while receiver.recv_timeout(Duration::from_millis(100))
                    == Err(mpsc::RecvTimeoutError::Timeout)
                {
                    visible = true;
                    terminal.write(format!("\r{} {label}", frame.next().unwrap_or("|")).as_bytes());
                }
                if visible {
                    terminal.write(b"\r\x1b[2K");
                }
            });
        Spinner(worker.ok().map(|worker| (stop, worker)))
    }

    pub fn new(verbosity: Verbosity) -> Self {
        // A separate descriptor avoids holding Rust's global stderr mutex while
        // a pipe blocks, including during process shutdown.
        match io::stderr().as_fd().try_clone_to_owned() {
            Ok(fd) => Self::with_writer(verbosity, std::fs::File::from(fd)),
            Err(_) => Self::with_writer(verbosity, io::sink()),
        }
    }

    fn with_writer(verbosity: Verbosity, mut writer: impl Write + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CHUNKS);
        let dropped = Arc::new(AtomicBool::new(false));
        let lost = dropped.clone();
        // Failure to create the presentation thread disables stderr mirroring;
        // execution, captured logs and stdout results remain available.
        let _ = std::thread::Builder::new()
            .name("groove-stderr".into())
            .spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Bytes(bytes) => {
                        if writer.write_all(&bytes).is_err() {
                            break;
                        }
                    }
                    Message::Finish(done) => {
                        if lost.load(Ordering::Relaxed) && !verbosity.quiet {
                            let _ = writer.write_all(b"\nwarning: terminal output was dropped; consult captured logs.\n");
                        }
                        let _ = writer.flush();
                        let _ = done.send(());
                        break;
                    }
                }
            }
            });
        Self {
            sender,
            dropped,
            verbosity,
        }
    }

    pub fn write(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(CHUNK_BYTES) {
            if self
                .sender
                .try_send(Message::Bytes(chunk.to_vec()))
                .is_err()
            {
                self.dropped.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    /// Drain normal output, but never wait indefinitely for a stopped pipe reader.
    pub fn finish(&self) {
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        let (done, receiver) = mpsc::channel();
        let mut message = Message::Finish(done);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => {
                    let _ =
                        receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                    return;
                }
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                _ => return,
            }
        }
    }
}

/// Stops and joins the presentation worker on success, error or unwind.
pub struct Spinner(Option<(mpsc::Sender<()>, std::thread::JoinHandle<()>)>);

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some((stop, worker)) = self.0.take() {
            let _ = stop.send(());
            let _ = worker.join();
        }
    }
}

impl groove_core::ExecutionObserver for Terminal {
    fn output(&self, _stream: groove_core::OutputStream, bytes: &[u8]) {
        if !self.verbosity.quiet {
            self.write(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Capture(mpsc::Sender<Vec<u8>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.send(bytes.to_vec()).map_err(io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn progress_clears_a_visible_frame_when_an_operation_fails() {
        let (sender, receiver) = mpsc::channel();
        let terminal = Terminal::with_writer(
            Verbosity {
                verbose: 0,
                quiet: false,
            },
            Capture(sender),
        );
        fn failed_query(terminal: &Terminal, receiver: &mpsc::Receiver<Vec<u8>>) -> Result<(), ()> {
            let _progress = terminal.start_spinner("Measuring disk usage");
            let frame = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(String::from_utf8_lossy(&frame).contains("Measuring disk usage"));
            Err(())
        }
        let result = failed_query(&terminal, &receiver);
        assert_eq!(result, Err(()));
        terminal.finish();
        let output: Vec<u8> = receiver.try_iter().flatten().collect();
        assert!(output.ends_with(b"\r\x1b[2K"));
    }
}
