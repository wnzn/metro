//! Pages: dashboards and small agentic apps people build inside Metro by
//! prompting, written in Page TSX.
//!
//! Page TSX looks like TSX but no page code ever runs: the server parses it
//! with a strict grammar ([`tsx`]), checks it against the block catalogue
//! and resolves room and agent names ([`compile`]), and stores the checked
//! JSON tree ([`spec`]) that the desktop app draws with Metro's own
//! components.

pub mod access;
pub mod actions;
pub mod api;
pub mod compile;
pub mod data;
pub mod generate;
pub mod limits;
pub mod spec;
pub mod store;
pub mod tsx;

pub use api::router;
pub use store::{hooks, init};

#[cfg(test)]
mod tests;
