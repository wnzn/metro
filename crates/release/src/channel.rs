//! The build's release channel, stamped at compile time.
//!
//! Only the official release jobs set `METRO_RELEASE_CHANNEL=release` when
//! they compile Metro. Preview builds set `preview`. Anything else (a local
//! `cargo build`, a fork) is a source build: it never checks for updates and
//! never prompts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Release,
    Preview,
    Source,
}

impl Channel {
    /// This build's channel.
    pub const fn current() -> Channel {
        Self::from_stamp(option_env!("METRO_RELEASE_CHANNEL"))
    }

    pub const fn from_stamp(stamp: Option<&str>) -> Channel {
        match stamp {
            Some(value) => {
                let bytes = value.as_bytes();
                if eq(bytes, b"release") {
                    Channel::Release
                } else if eq(bytes, b"preview") {
                    Channel::Preview
                } else {
                    Channel::Source
                }
            }
            None => Channel::Source,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Channel::Release => "release",
            Channel::Preview => "preview",
            Channel::Source => "source",
        }
    }
}

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps() {
        assert_eq!(Channel::from_stamp(Some("release")), Channel::Release);
        assert_eq!(Channel::from_stamp(Some("preview")), Channel::Preview);
        assert_eq!(Channel::from_stamp(Some("Release")), Channel::Source);
        assert_eq!(Channel::from_stamp(Some("release ")), Channel::Source);
        assert_eq!(Channel::from_stamp(None), Channel::Source);
    }
}
