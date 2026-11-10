//! Bounded stderr delivery, independent of stdout results and persistent logging.

use std::{
    io::{self, Write},
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

impl groove_core::ExecutionObserver for Terminal {
    fn output(&self, _stream: groove_core::OutputStream, bytes: &[u8]) {
        if !self.verbosity.quiet {
            self.write(bytes);
        }
    }
}
