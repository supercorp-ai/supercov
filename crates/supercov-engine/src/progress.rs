//! A branded spinner on stderr while a long, otherwise-silent step runs.
//!
//! The frames grow from a dot into the Supercov mark and back (the "bloom").
//! It is silent unless stderr is an interactive terminal, and for the first
//! frame interval of fast steps, so agents, pipes, and quick commands never
//! see it. The owner must drop it before writing any other output.

use std::{
    io::{IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const FRAMES: [&str; 6] = ["·", "✻", "✽", "❋", "✽", "✻"];
const FRAME_INTERVAL: Duration = Duration::from_millis(120);

/// Carriage-return animation is only safe on a terminal a person is watching.
/// CI systems and dumb terminals record every write, turning each frame into
/// its own log line, so there the spinner degrades to one stable, newline-
/// terminated line that still names what is happening.
enum Mode {
    Animated,
    StaticLine,
}

fn mode() -> Option<Mode> {
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let ci = std::env::var("CI").is_ok_and(|value| {
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    });
    if ci || term.is_empty() || term == "dumb" {
        return Some(Mode::StaticLine);
    }
    Some(Mode::Animated)
}

pub struct ProgressLine {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ProgressLine {
    pub fn start(message: &'static str) -> Option<Self> {
        let mode = mode()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            // Nothing is shown for the first interval, so fast steps stay
            // clean in every mode.
            std::thread::sleep(FRAME_INTERVAL);
            if flag.load(Ordering::Relaxed) {
                return;
            }
            if matches!(mode, Mode::StaticLine) {
                eprintln!("❋ {message}");
                return;
            }
            let mut frame = 0_usize;
            loop {
                eprint!("\r{} {message}", FRAMES[frame % FRAMES.len()]);
                let _ = std::io::stderr().flush();
                frame += 1;
                std::thread::sleep(FRAME_INTERVAL);
                if flag.load(Ordering::Relaxed) {
                    break;
                }
            }
            // The widest frame is three bytes but one column; clearing by
            // character count over-clears harmlessly.
            eprint!("\r{}\r", " ".repeat(message.len() + 2));
            let _ = std::io::stderr().flush();
        });
        Some(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for ProgressLine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
