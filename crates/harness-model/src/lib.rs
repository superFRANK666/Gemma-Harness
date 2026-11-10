//! Local-model transports and protocol diagnostics. Tool execution belongs to the caller.
mod arguments;
mod functiongemma;
mod gguf;
mod image_probe;
mod normalize;
mod profile;
mod provider;
mod schema;
mod sse;
mod types;

pub use arguments::parse_tool_arguments;
pub use gguf::{inspect_gguf, inspect_template_issues};
pub use profile::{context_budget, target_model_profile, ContextBudget, ModelProfile};
pub use provider::Provider;
pub use schema::normalize_tool_schema;
pub use types::*;
