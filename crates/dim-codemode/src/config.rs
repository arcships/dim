/// Host-only bounds of one Code Mode program. These are not tool parameters,
/// not model-visible limits, and not part of the prompt.
#[derive(Debug, Clone)]
pub struct CodeModeConfig {
    pub max_code_bytes: usize,
    pub max_runtime_ms: u64,
    pub max_calls: u64,
    pub max_parallel_calls: usize,
    pub max_memory_bytes: usize,
    pub max_programs: usize,
    pub max_output_bytes: usize,
    pub max_single_result_bytes: usize,
}

impl Default for CodeModeConfig {
    fn default() -> Self {
        Self {
            max_code_bytes: 256 * 1024,
            max_runtime_ms: 600_000,
            max_calls: 4096,
            max_parallel_calls: 16,
            max_memory_bytes: 1024 * 1024 * 1024,
            max_programs: 2,
            max_output_bytes: 16 * 1024 * 1024,
            max_single_result_bytes: 4 * 1024 * 1024,
        }
    }
}
