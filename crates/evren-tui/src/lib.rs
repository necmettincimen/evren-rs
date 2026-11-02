//! Satır tabanlı etkileşimli arayüz (Python `repl.py`, `ui.py`, `interrupt.py`).

pub mod catalog_cmds;
pub mod commands;
pub mod docs;
pub mod events;
pub mod extras;
pub mod handlers;
pub mod input;
pub mod iteration;
pub mod markdown;
pub mod oneshot;
pub mod repl;
pub mod shell;
pub mod ssh_cmds;
pub mod ui;
pub mod workflow;

pub use oneshot::{run_agent_once, AgentOnce};
pub use repl::{run, ReplOptions};
