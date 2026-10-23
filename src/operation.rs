//! Bounded presentation state for a running file or Git action.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub struct OperationProgress {
    pub title: String,
    pub context: String,
    pub started: Instant,
    pub lines: VecDeque<String>,
    pub scrollback: usize,
    pub cancelling: bool,
    last_redraw: Instant,
}

impl OperationProgress {
    pub fn new(title: String, context: String) -> Self {
        Self {
            title,
            context,
            started: Instant::now(),
            lines: VecDeque::new(),
            scrollback: 0,
            cancelling: false,
            last_redraw: Instant::now(),
        }
    }

    pub fn record(&mut self, text: &str) {
        for line in crate::git_actions::visible(text.as_bytes())
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let line: String = line.chars().take(2048).collect();
            if self.lines.back() == Some(&line) {
                continue;
            }
            if self.scrollback > 0 {
                self.scrollback = self.scrollback.saturating_add(1);
            }
            self.lines.push_back(line);
            if self.lines.len() > 200 {
                self.lines.pop_front();
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        if self.last_redraw.elapsed() < Duration::from_millis(200) {
            return false;
        }
        self.last_redraw = Instant::now();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_bounded_deduplicated_and_safe_to_display() {
        let mut progress = OperationProgress::new("test".into(), "fixture".into());
        progress.record("fetching\nfetching");
        assert_eq!(progress.lines.len(), 1);
        for n in 0..250 {
            progress.record(&format!("step {n}"));
        }
        assert_eq!(progress.lines.len(), 200);
        assert_eq!(progress.lines.front().unwrap(), "step 50");
        progress.record("https://user:secret@host/repo\x1b[31m");
        assert!(!progress.lines.back().unwrap().contains("secret"));
        assert!(!progress.lines.back().unwrap().contains('\x1b'));
        progress.record(&"é".repeat(4096));
        assert_eq!(progress.lines.back().unwrap().chars().count(), 2048);
    }
}
