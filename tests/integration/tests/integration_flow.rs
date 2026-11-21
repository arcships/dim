#[path = "local_support/compaction.rs"]
mod compaction_support;
mod local_support;
#[cfg(unix)]
#[path = "local_support/process.rs"]
mod process_support;
#[path = "local_support/task_store.rs"]
mod task_store;

mod context_input;
mod context_projection;
mod environment_clients;
mod environment_settings;
mod extensions;
mod host_prompt_overrides;
mod interrupt;
#[cfg(unix)]
mod local_execution;
mod local_product;
#[cfg(unix)]
mod minimal_execution;
mod named_parts;
mod part_registry;
mod prompt_boundaries;
mod prompt_guide;
mod prompt_lifecycle;
mod prompt_sources;
mod session_lifecycle;
mod skills;
mod timeline_batch;
mod timeline_tail;
mod tool_binding;
mod wait_protocol;
