//! `replayr-obs`: runs OBS (libobs) for Replayr's Modern recording engine.
//!
//! The app starts this program from its `obs\` folder, next to the OBS
//! files, and talks to it with one JSON message per line:
//! `obs_protocol::Request` on stdin, `obs_protocol::Reply` on stdout. stderr
//! carries OBS's log as plain text. One process serves one configuration:
//! the first `Configure` starts OBS; for another configuration the app
//! starts a new process.

// The pure helpers are only used by the Windows code.
#![cfg_attr(not(windows), allow(dead_code))]

mod capture;
mod monitors;
mod session;

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("replayr-obs needs Windows");
    std::process::ExitCode::from(1)
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    host::run()
}

#[cfg(windows)]
mod host {
    use std::io::{BufRead, Write};
    use std::process::ExitCode;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::time::{Duration, Instant};

    use obs_protocol::{Reply, Request};

    use crate::session::Session;

    /// How often levels go out and the game hook is checked.
    const TICK: Duration = Duration::from_millis(100);
    /// How long to wait for libobs to finish shutting down before exiting.
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    /// What the main loop reacts to, besides its tick.
    pub enum Event {
        Request(Result<Request, String>),
        /// stdin closed: the app is gone.
        Closed,
        /// A reply from a background job (saving, stopping a recording).
        Reply(Reply),
    }

    pub fn run() -> ExitCode {
        quiet_errors();
        logging::init();
        let (events, rx) = mpsc::channel();
        read_stdin(events.clone());

        let mut session: Option<Session> = None;
        let mut next_tick = Instant::now() + TICK;
        let code = loop {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(Event::Request(Ok(Request::Quit)) | Event::Closed) => break ExitCode::SUCCESS,
                Ok(Event::Request(Ok(Request::Configure(_)))) if session.is_some() => {
                    send(&error("already configured"));
                }
                Ok(Event::Request(Ok(Request::Configure(config)))) => {
                    match Session::start(config, events.clone()) {
                        Ok((started, replies)) => {
                            send(&Reply::Ready);
                            replies.iter().for_each(send);
                            session = Some(started);
                        }
                        // Without OBS there is nothing to serve; the app
                        // sees the exit and counts it as a failed start.
                        Err(message) => {
                            send(&Reply::Error { message });
                            break ExitCode::from(1);
                        }
                    }
                }
                Ok(Event::Request(Ok(request))) => {
                    let reply = match session.as_mut() {
                        None => Some(error("not configured")),
                        Some(session) => match request {
                            Request::Save { dir } => session.save(dir),
                            Request::RecordStart { dir } => session.record_start(dir),
                            Request::RecordStop => session.record_stop(),
                            Request::Configure(_) | Request::Quit => None,
                        },
                    };
                    if let Some(reply) = reply {
                        send(&reply);
                    }
                }
                Ok(Event::Request(Err(e))) => send(&error(&format!("bad request: {e}"))),
                Ok(Event::Reply(reply)) => send(&reply),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break ExitCode::SUCCESS,
            }
            let now = Instant::now();
            if now >= next_tick {
                next_tick = now + TICK;
                if let Some(session) = session.as_mut() {
                    session.tick(now).iter().for_each(send);
                }
            }
        };

        if let Some(session) = session {
            session.shutdown().iter().for_each(send);
        }
        forward_job_replies(&rx);
        logging::wait_for_obs_shutdown(SHUTDOWN_TIMEOUT);
        code
    }

    fn error(message: &str) -> Reply {
        Reply::Error {
            message: message.to_string(),
        }
    }

    /// Replies that background jobs sent while the session shut down.
    fn forward_job_replies(rx: &Receiver<Event>) {
        while let Ok(event) = rx.try_recv() {
            if let Event::Reply(reply) = event {
                send(&reply);
            }
        }
    }

    /// The only place that writes to stdout: one whole line per reply.
    fn send(reply: &Reply) {
        let mut out = std::io::stdout().lock();
        // A closed stdout means the app is gone; stdin closing ends the loop.
        let _ = out
            .write_all(obs_protocol::encode(reply).as_bytes())
            .and_then(|()| out.flush());
    }

    fn read_stdin(events: Sender<Event>) {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if events
                    .send(Event::Request(obs_protocol::decode(&line)))
                    .is_err()
                {
                    return;
                }
            }
            let _ = events.send(Event::Closed);
        });
    }

    /// No Windows error dialogs for this process (crash, missing media):
    /// the app is a tray program and restarts the helper itself.
    fn quiet_errors() {
        use windows::Win32::System::Diagnostics::Debug::{
            SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX,
        };
        // SAFETY: only changes this process's error mode.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        }
    }

    /// OBS's log and the `log` crate (libobs-wrapper, its crash handler) as
    /// plain text on stderr, which the app copies into its own log.
    pub mod logging {
        use std::io::Write;
        use std::sync::{Condvar, Mutex};
        use std::time::{Duration, Instant};

        use libobs_wrapper::enums::ObsLogLevel;
        use libobs_wrapper::logger::ObsLogger;

        /// libobs-wrapper logs this once `obs_shutdown` has returned.
        const SHUTDOWN_DONE: &str = "OBS context shutdown.";

        struct Lifetime {
            started: bool,
            shut_down: bool,
        }

        static LIFETIME: Mutex<Lifetime> = Mutex::new(Lifetime {
            started: false,
            shut_down: false,
        });
        static CHANGED: Condvar = Condvar::new();

        fn write_line(line: std::fmt::Arguments) {
            // Never panic here: this runs inside OBS's C log callback.
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }

        pub fn init() {
            static LOG: StderrLog = StderrLog;
            if log::set_logger(&LOG).is_ok() {
                log::set_max_level(log::LevelFilter::Warn);
            }
        }

        /// Called once libobs is running, so that exiting waits for its
        /// shutdown.
        pub fn obs_started() {
            if let Ok(mut lifetime) = LIFETIME.lock() {
                lifetime.started = true;
            }
        }

        /// libobs shuts down on its own thread once the last handle is
        /// dropped; exiting before it is done can crash in OBS's teardown.
        pub fn wait_for_obs_shutdown(timeout: Duration) {
            let deadline = Instant::now() + timeout;
            let Ok(mut lifetime) = LIFETIME.lock() else {
                return;
            };
            while lifetime.started && !lifetime.shut_down {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    write_line(format_args!("[replayr-obs] OBS did not shut down in time"));
                    return;
                }
                match CHANGED.wait_timeout(lifetime, left) {
                    Ok((guard, _)) => lifetime = guard,
                    Err(_) => return,
                }
            }
        }

        #[derive(Debug)]
        pub struct StderrLogger;

        impl ObsLogger for StderrLogger {
            fn log(&mut self, level: ObsLogLevel, msg: String) {
                if level != ObsLogLevel::Debug {
                    write_line(format_args!("[{level:?}] {msg}"));
                }
                if msg == SHUTDOWN_DONE {
                    if let Ok(mut lifetime) = LIFETIME.lock() {
                        lifetime.shut_down = true;
                        CHANGED.notify_all();
                    }
                }
            }
        }

        struct StderrLog;

        impl log::Log for StderrLog {
            fn enabled(&self, metadata: &log::Metadata) -> bool {
                metadata.level() <= log::Level::Warn
            }

            fn log(&self, record: &log::Record) {
                if self.enabled(record.metadata()) {
                    write_line(format_args!(
                        "[{}] {}: {}",
                        record.level(),
                        record.target(),
                        record.args()
                    ));
                }
            }

            fn flush(&self) {}
        }
    }
}
