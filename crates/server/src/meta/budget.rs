//! Per-turn limits on Metro-in-Chat tools. A call over a limit is refused
//! before it does anything; nothing is ever half done.

use std::sync::{Arc, Mutex};

/// Meta tool calls in one Chat turn.
pub const MAX_CALLS: usize = 16;
/// `space_send` calls in one Chat turn.
pub const MAX_SENDS: usize = 3;
/// `space_send` calls waiting for a run at once, per turn.
pub const MAX_WAITING: usize = 1;
/// `spaces_search` calls in one Chat turn.
pub const MAX_SEARCHES: usize = 3;
/// Meta tool output handed to the model in one Chat turn, in bytes.
pub const MAX_OUTPUT_BYTES: usize = 96 * 1024;
/// Below this much output left, a call is refused rather than cut to nothing.
const MIN_OUTPUT_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Search,
    Send,
}

#[derive(Default)]
struct Used {
    calls: usize,
    sends: usize,
    searches: usize,
    waiting: usize,
    output: usize,
}

/// One Chat turn's budget, shared by all its meta tools.
#[derive(Clone, Default)]
pub struct Budget(Arc<Mutex<Used>>);

/// A waiting slot; released on drop.
pub struct Waiting(Budget);

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut u = self.0.lock();
        u.waiting = u.waiting.saturating_sub(1);
    }
}

impl Budget {
    fn lock(&self) -> std::sync::MutexGuard<'_, Used> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Count one call of `kind`, or say which limit it would pass.
    pub fn admit(&self, kind: Kind) -> Result<(), String> {
        let mut u = self.lock();
        if u.calls >= MAX_CALLS {
            return Err(format!(
                "this reply already used its {MAX_CALLS} Space tool calls; answer with what you have"
            ));
        }
        if MAX_OUTPUT_BYTES.saturating_sub(u.output) < MIN_OUTPUT_BYTES {
            return Err(
                "this reply already read as much Space content as it may; answer with what you have"
                    .to_owned(),
            );
        }
        match kind {
            Kind::Send if u.sends >= MAX_SENDS => {
                return Err(format!(
                    "this reply already sent {MAX_SENDS} messages to Spaces; ask the user before sending more"
                ));
            }
            Kind::Search if u.searches >= MAX_SEARCHES => {
                return Err(format!(
                    "this reply already searched {MAX_SEARCHES} times; answer with what you found"
                ));
            }
            _ => {}
        }
        u.calls += 1;
        match kind {
            Kind::Send => u.sends += 1,
            Kind::Search => u.searches += 1,
            Kind::Read => {}
        }
        Ok(())
    }

    /// A waiting slot for delegate-and-wait, if none is taken.
    pub fn wait_slot(&self) -> Option<Waiting> {
        let mut u = self.lock();
        if u.waiting >= MAX_WAITING {
            return None;
        }
        u.waiting += 1;
        Some(Waiting(self.clone()))
    }

    /// Output bytes still allowed for one result, at most `max`; counts them.
    pub fn take_output(&self, wanted: usize, max: usize) -> usize {
        let mut u = self.lock();
        let n = wanted
            .min(max)
            .min(MAX_OUTPUT_BYTES.saturating_sub(u.output));
        u.output += n;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_sends_searches_and_output_are_bounded() {
        let b = Budget::default();
        for _ in 0..MAX_SEARCHES {
            b.admit(Kind::Search).unwrap();
        }
        assert!(b.admit(Kind::Search).unwrap_err().contains("searched"));
        for _ in 0..MAX_SENDS {
            b.admit(Kind::Send).unwrap();
        }
        assert!(b.admit(Kind::Send).unwrap_err().contains("sent"));
        while b.admit(Kind::Read).is_ok() {}
        assert_eq!(b.lock().calls, MAX_CALLS);
        assert!(b.admit(Kind::Read).unwrap_err().contains("tool calls"));

        let b = Budget::default();
        assert_eq!(b.take_output(64 * 1024, 64 * 1024), 64 * 1024);
        assert_eq!(b.take_output(64 * 1024, 64 * 1024), 32 * 1024);
        assert_eq!(b.take_output(10, 64 * 1024), 0);
        assert!(b.admit(Kind::Read).unwrap_err().contains("as much"));
    }

    #[test]
    fn one_waiting_send_at_a_time() {
        let b = Budget::default();
        let slot = b.wait_slot().unwrap();
        assert!(b.wait_slot().is_none());
        drop(slot);
        assert!(b.wait_slot().is_some());
    }
}
