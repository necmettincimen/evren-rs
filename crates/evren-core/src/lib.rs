//! EVREN CLI çekirdeği. Python `src/` modüllerinin davranış eşdeğeri;
//! ayrıntılar `docs/plan.md` içindedir.

pub mod catalog;
#[allow(clippy::type_complexity)]
mod catalog_data;
pub mod config;
pub mod docs_merge;
pub mod docs_state;
pub mod error;
pub mod git;
pub mod language;
pub mod memory;
pub mod models;
pub mod output_summary;
pub mod project_docs;
pub mod project_scan;
pub mod redact;
pub mod secret_vault;
pub mod settings;
pub mod ssh_hosts;
pub mod texts;
pub mod tools;
pub mod tracking;
pub mod workflow_state;

pub use error::EvrenError;

/// `evren --version` çıktısındaki sürüm (Python: `evren-cli 3.11.0`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
