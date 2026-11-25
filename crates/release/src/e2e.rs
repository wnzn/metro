//! End-to-end test builds (feature `e2e`, never in a release build).
//!
//! With `METRO_TEST_RELEASE_BASE=http://127.0.0.1:<port>` set, a binary built
//! with this feature treats itself as a release build, fetches releases
//! from that loopback fixture server only, and verifies them with
//! [`crate::testing::TestVerifier`]. Every use prints a warning.

use crate::fetch::Source;
use crate::release::ReleaseVerifier;

pub const ENV_BASE: &str = "METRO_TEST_RELEASE_BASE";

/// The fixture source and test verifier, when the variable is set.
pub fn release_source() -> Option<(Source, Box<dyn ReleaseVerifier>)> {
    let base = std::env::var(ENV_BASE).ok()?;
    let Some(source) = Source::loopback_fixture(&base) else {
        eprintln!("TEST BUILD: {ENV_BASE} must be http://127.0.0.1:<port>; ignored");
        return None;
    };
    eprintln!("TEST BUILD: releases come from {base} and are checked with the test verifier");
    Some((source, Box::new(crate::testing::TestVerifier)))
}

/// Whether the test release source is active.
pub fn active() -> bool {
    std::env::var(ENV_BASE).is_ok_and(|b| Source::loopback_fixture(&b).is_some())
}

// A release-stamped build can never carry the test hooks.
const _: () = assert!(
    !matches!(crate::Channel::current(), crate::Channel::Release),
    "the `e2e` test feature must never be built with METRO_RELEASE_CHANNEL=release"
);
