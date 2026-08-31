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

pub struct ProgressLine {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ProgressLine {
    pub fn start(message: &'static str) -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut drawn = false;
            let mut frame = 0_usize;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(FRAME_INTERVAL);
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                eprint!("\r{} {message}", FRAMES[frame % FRAMES.len()]);
                let _ = std::io::stderr().flush();
                drawn = true;
                frame += 1;
            }
            if drawn {
                // The widest frame is three bytes but one column; clearing by
                // character count over-clears harmlessly.
                eprint!("\r{}\r", " ".repeat(message.len() + 2));
                let _ = std::io::stderr().flush();
            }
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
