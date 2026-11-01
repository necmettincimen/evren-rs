//! Ajan döngüsü: oturum durumu, adım döngüsü, korumalar, bağlam sıkıştırma,
//! planlayıcı yönlendirmesi ve görev izi (Python `agent.py` paritesi).

pub mod compaction;
pub mod context;
pub mod guards;
pub mod modes;
pub mod route;
pub mod session;
pub mod todos;
pub mod token_manager;
pub mod trace;
pub mod usage_log;

#[path = "loop.rs"]
mod agent_loop;

pub use agent_loop::Cancelled;
pub use session::{AgentEvent, AgentSession, EventSender};
