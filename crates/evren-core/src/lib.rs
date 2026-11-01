//! EVREN CLI çekirdeği. Python `src/` modüllerinin davranış eşdeğeri;
//! ayrıntılar `docs/plan.md` içindedir.

pub mod config;
pub mod docs_state;
pub mod error;
pub mod models;
pub mod secret_vault;
pub mod settings;
pub mod ssh_hosts;
pub mod texts;
pub mod tools;

pub use error::EvrenError;

/// `evren --version` çıktısındaki sürüm (Python: `evren-cli 3.11.0`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
