#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;
pub mod core_error;
mod diagnostics;
pub mod diff;
pub mod git;
pub mod logging;
mod mcp;
pub mod net;
pub mod server;
pub mod storage;
pub mod terminal;

pub use server::serve_stdio;
