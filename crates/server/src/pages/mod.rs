//! Pages: dashboards and small agentic apps people build inside Metro by
//! prompting, written in Page TSX.
//!
//! Page TSX looks like TSX but no page code ever runs: the server parses it
//! with a strict grammar ([`tsx`]), checks it against the block catalogue
//! and resolves room and agent names ([`compile`]), and stores the checked
//! JSON tree ([`spec`]) that the desktop app draws with Metro's own
//! components.

pub mod compile;
pub mod spec;
pub mod tsx;

#[cfg(test)]
mod tests;
