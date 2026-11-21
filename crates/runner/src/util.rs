//! Small helpers: truncation, randomness, backoff, interruptible sleep.

use std::time::{Duration, Instant};

/// Truncate `s` to at most `max` bytes on a char boundary, appending a marker
/// that states how much was dropped.
pub fn truncate_utf8(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let total = s.len();
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(&format!(
        "\n[metro-runner: output truncated, {total} bytes total, {cut} shown]"
    ));
    s
}

/// Lossy UTF-8 decode of a captured byte buffer.
pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 64 random bits from the kernel (getrandom(2)); falls back to time/pid mixing.
pub fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    // SAFETY: buf is valid for writes of 8 bytes.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n == 8 {
        return u64::from_ne_bytes(buf);
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    t ^ (u64::from(std::process::id()) << 32) ^ 0x9e37_79b9_7f4a_7c15
}

/// Exponential backoff with "equal jitter": delay in [cap/2, cap] where cap
/// doubles from `base` up to `max`.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn next_delay(&mut self) -> Duration {
        let factor = 1u32.checked_shl(self.attempt.min(20)).unwrap_or(u32::MAX);
        let cap = self.base.saturating_mul(factor).min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        let half = cap / 2;
        let span = cap.saturating_sub(half).as_millis() as u64;
        let jitter = if span == 0 {
            0
        } else {
            random_u64() % (span + 1)
        };
        half + Duration::from_millis(jitter)
    }
}

/// Sleep for `d`, waking early when shutdown is requested. Returns false if
/// interrupted by shutdown.
pub fn sleep_interruptible(d: Duration) -> bool {
    let end = Instant::now() + d;
    loop {
        if crate::shutdown_requested() {
            return false;
        }
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "aé".repeat(10); // 'é' is 2 bytes
        let t = truncate_utf8(s.clone(), 2);
        assert!(t.starts_with('a'));
        assert!(t.contains("truncated"));
        assert_eq!(truncate_utf8("abc".into(), 3), "abc");
    }

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        for _ in 0..30 {
            let d = b.next_delay();
            assert!(d <= Duration::from_secs(1));
        }
        let d = b.next_delay();
        assert!(d >= Duration::from_millis(500), "{d:?}");
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(100));
    }
}
