use super::*;
use crate::history::Command;
use crate::shell::Foreground;
use std::time::{Duration, Instant};

// Only the official transcript supplies footprint text. A temporary fingerprint
// associates the new rollout with a submission from this particular terminal.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PromptWitness {
    prefix_bytes: Option<usize>,
    hash: blake3::Hash,
}

impl PromptWitness {
    fn new(text: &str, continued: bool) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty()).then(|| Self {
            prefix_bytes: continued.then_some(text.len()),
            hash: blake3::hash(text.as_bytes()),
        })
    }

    pub fn matches(&self, text: &str) -> bool {
        let bytes = text.trim().as_bytes();
        bytes
            .get(..self.prefix_bytes.unwrap_or(bytes.len()))
            .is_some_and(|sample| blake3::hash(sample) == self.hash)
    }
}

struct Watch {
    send: mpsc::Sender<Command>,
    marker: String,
    run: String,
    submitted: Option<(u64, Instant)>,
    awaiting_prompt: bool,
    slash: String,
    paste: Option<PromptWitness>,
}

#[derive(Default)]
pub(super) struct SessionSwitch {
    watch: Mutex<Option<Watch>>,
    awaiting_screen: AtomicBool,
}

impl SessionSwitch {
    pub(super) fn watch(&self, target: Option<(mpsc::Sender<Command>, String, String)>) {
        *self.watch.lock().unwrap() = target.map(|(send, marker, run)| Watch {
            send,
            marker,
            run,
            submitted: None,
            awaiting_prompt: false,
            slash: String::new(),
            paste: None,
        });
        self.awaiting_screen.store(false, Ordering::Release);
    }

    pub(super) fn input<T>(&self, term: &Term<T>, bytes: &[u8]) {
        let mut watch = self.watch.lock().unwrap();
        let Some(w) = &mut *watch else { return };
        let cursor = term.grid().cursor.point;
        let start = composer_start(term, cursor.line);
        let empty = start.is_some_and(|start| cursor.column.0 == start);
        if !matches!(bytes, b"\r" | b"\x1b[13;1u") {
            // Escape cancels the workspace picker. A later Ctrl+L can redraw
            // the same header in the old thread and must not confirm a switch.
            if matches!(bytes, b"\x1b" | b"\x1b[27;1u" | b"\x03" | b"\x0c") {
                w.submitted = None;
                self.awaiting_screen.store(false, Ordering::Release);
            }
            let input = bytes
                .strip_prefix(b"\x1b[200~")
                .and_then(|b| b.strip_suffix(b"\x1b[201~"))
                .unwrap_or(bytes);
            w.paste =
                if w.awaiting_prompt && empty && input.len() > 1 && !input.starts_with(b"\x1b") {
                    std::str::from_utf8(input)
                        .ok()
                        .and_then(|s| PromptWitness::new(s, false))
                } else {
                    None
                };
            // A rapid paste or several keystrokes can precede the TUI repaint.
            // Retain only prefixes of the two supported commands.
            if empty && input.starts_with(b"/") {
                w.slash.clear();
            }
            if (empty || !w.slash.is_empty())
                && w.slash.len() + input.len() <= 6
                && let Ok(s) = std::str::from_utf8(input)
            {
                w.slash.push_str(s);
                if ["/clear", "/new"]
                    .iter()
                    .any(|cmd| cmd.starts_with(&w.slash))
                {
                    return;
                }
            }
            w.slash.clear();
            return;
        }
        let typed_switch = matches!(w.slash.as_str(), "/clear" | "/new");
        w.slash.clear();
        if !typed_switch
            && !w.awaiting_prompt
            && !start.is_some_and(|start| {
                start < term.columns() && term.grid()[cursor.line][Column(start)].c == '/'
            })
        {
            return;
        }
        let draft = (0..=cursor.line.0).rev().find_map(|y| {
            let line = Line(y);
            composer_start(term, line).map(|start| (line, start))
        });
        let Some((line, start)) = draft else { return };
        let row = row_text(term, line, start);
        let input = row.trim();
        if typed_switch || matches!(input.split_whitespace().next(), Some("/clear" | "/new")) {
            w.submitted = Some((crate::hook_wire::timestamp_micros(), Instant::now()));
            self.awaiting_screen.store(true, Ordering::Release);
        } else if w.awaiting_prompt
            && !input.starts_with('/')
            && let Some(witness) = w.paste.take().or_else(|| {
                if empty && line == cursor.line {
                    return None;
                }
                PromptWitness::new(input, line != cursor.line)
            })
        {
            let _ = w.send.send(Command::SubmittedPrompt(
                w.marker.clone(),
                w.run.clone(),
                (crate::hook_wire::timestamp_micros() / 1000) as i64,
                witness,
            ));
        }
    }

    pub(super) fn frame<T>(&self, term: &Term<T>, agent: Foreground) {
        if agent != Foreground::Codex || !self.awaiting_screen.load(Ordering::Acquire) {
            return;
        }
        let mut watch = self.watch.lock().unwrap();
        let Some(w) = &mut *watch else { return };
        let Some((sent, at)) = w.submitted else {
            return;
        };
        let cursor = term.grid().cursor.point;
        // The picker and the submitted command both precede the fresh header. A
        // plain clear-screen, a draft or a cancelled picker is not a thread switch.
        let fresh = composer_start(term, cursor.line).is_some_and(|start| cursor.column.0 <= start)
            && (0..cursor.line.0).rev().find_map(|y| {
                let line = Line(y);
                if composer_start(term, line).is_some() {
                    return Some(false);
                }
                let row = row_text(term, line, 0);
                row.contains("OpenAI Codex (v").then_some(true)
            }) == Some(true);
        if fresh {
            let _ = w
                .send
                .send(Command::FreshSession(w.marker.clone(), w.run.clone(), sent));
            w.awaiting_prompt = true;
        }
        if fresh || at.elapsed() > Duration::from_secs(120) {
            w.submitted = None;
            self.awaiting_screen.store(false, Ordering::Release);
        }
    }

    pub(super) fn awaiting_identity(&self, pending: bool) {
        if let Some(w) = &mut *self.watch.lock().unwrap() {
            w.awaiting_prompt = pending;
            if !pending {
                w.paste = None;
            }
        }
    }
}

fn composer_start<T>(term: &Term<T>, line: Line) -> Option<usize> {
    let row = &term.grid()[line];
    let marker = (0..term.columns().min(6)).find(|&col| row[Column(col)].c != ' ')?;
    (row[Column(marker)].c == '›'
        && marker + 1 < term.columns()
        && row[Column(marker + 1)].c == ' ')
        .then_some(marker + 2)
}
