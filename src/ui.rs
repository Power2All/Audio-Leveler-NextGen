//! Terminal output: the start-up splash and a live per-file status list.
//!
//! Each file being worked on gets a `[|] name` line whose ASCII spinner advances every
//! 250 ms (four small redraws a second, so it costs next to nothing). When the file finishes the
//! line becomes a green `[✓]`, a red `[✗]` with the error, or a yellow `[-]` if it was
//! skipped. When stderr isn't a terminal (or `NO_COLOR` is set) only the final lines are
//! printed, without colours or cursor movement.

use std::io::{IsTerminal, Write};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn fancy() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Print the title and a few `label  value` rows to stderr.
pub fn splash(title: &str, rows: &[(&str, String)]) {
    let (b, d, r) = if fancy() { (BOLD, DIM, RESET) } else { ("", "", "") };
    let mut s = format!("\n  {b}{title}{r}\n  {d}{}{r}\n", "─".repeat(title.chars().count()));
    for (label, value) in rows {
        s += &format!("  {d}{label:<9}{r}{value}\n");
    }
    eprintln!("{s}");
}

/// How a file ended.
pub enum Finish {
    /// Converted; optional note (e.g. tags that couldn't be copied).
    Done(Option<String>),
    /// Scanned; the report is printed under the file's line.
    Report(String),
    Skipped,
    Failed(String),
}

#[derive(Default)]
struct State {
    active: Vec<(usize, String)>, // files being worked on: (job index, label)
    finished: Vec<String>,        // final lines not printed yet
    drawn: usize,                 // status lines currently on screen
    frame: usize,
    stop: bool,
}

pub struct Progress {
    fancy: bool,
    state: Mutex<State>,
    wake: Condvar,
}

impl Progress {
    pub fn new() -> Self {
        Progress { fancy: fancy(), state: Mutex::default(), wake: Condvar::new() }
    }

    pub fn start(&self, id: usize, label: String) {
        self.state.lock().unwrap().active.push((id, label));
        self.wake.notify_one();
    }

    pub fn finish(&self, id: usize, result: Finish) {
        let mut st = self.state.lock().unwrap();
        let Some(pos) = st.active.iter().position(|(i, _)| *i == id) else {
            return;
        };
        let (_, label) = st.active.remove(pos);
        let mut report = None;
        let (mark, colour, note) = match result {
            Finish::Done(note) => ('✓', GREEN, note),
            Finish::Report(text) => {
                report = Some(text);
                ('✓', GREEN, None)
            }
            Finish::Skipped => ('-', YELLOW, Some("already exists, skipped".to_string())),
            Finish::Failed(err) => ('✗', RED, Some(err)),
        };
        let mark = if self.fancy { format!("{colour}{mark}{RESET}") } else { mark.to_string() };
        let note = note.map(|n| format!(" — {n}")).unwrap_or_default();
        st.finished.push(format!("[{mark}] {label}{note}"));
        st.finished.extend(report);
        drop(st);
        self.wake.notify_one();
    }

    /// Draw until `stop` is called; run this on its own thread. Redraws immediately when a
    /// file starts or finishes, and otherwise every 250 ms to turn the spinner.
    pub fn run(&self) {
        let tick = Duration::from_millis(250);
        let mut last_tick = Instant::now();
        let mut st = self.state.lock().unwrap();
        loop {
            self.draw(&mut st);
            if st.stop {
                break;
            }
            let wait = tick.saturating_sub(last_tick.elapsed());
            st = self.wake.wait_timeout(st, wait).unwrap().0;
            if last_tick.elapsed() >= tick {
                st.frame += 1;
                last_tick = Instant::now();
            }
        }
    }

    pub fn stop(&self) {
        self.state.lock().unwrap().stop = true;
        self.wake.notify_one();
    }

    fn draw(&self, st: &mut State) {
        let mut out = String::new();
        if self.fancy && st.drawn > 0 {
            // Back up over the previous status lines and clear them.
            out += &format!("\x1b[{}A\r\x1b[J", st.drawn);
        }
        for line in st.finished.drain(..) {
            out += &line;
            out.push('\n');
        }
        if self.fancy {
            let spin = SPINNER[st.frame % SPINNER.len()];
            // Auto-wrap off while drawing, so a long name can't take two rows and throw off
            // the line count used to redraw.
            out += "\x1b[?7l";
            for (_, label) in &st.active {
                out += &format!("[{spin}] {label}\n");
            }
            out += "\x1b[?7h";
            st.drawn = st.active.len();
        }
        if !out.is_empty() {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(out.as_bytes());
            let _ = err.flush();
        }
    }
}
