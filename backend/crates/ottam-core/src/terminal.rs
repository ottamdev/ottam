mod environment;
mod input;
mod manager;
mod output_queue;
mod pty;
mod scrollback;

#[cfg(test)]
mod tests;

pub use manager::TerminalManager;
pub use output_queue::{terminal_output_queue_max_bytes, TerminalEventSender, TerminalOutputQueue};
pub(crate) use pty::{
    TerminalIdentity, TerminalSpawnSpec, TerminalSpawner, TerminalWorkspaceIdentity,
};
