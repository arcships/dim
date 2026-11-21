const RELEASE: bool = matches!(option_env!("DIM_RELEASE_BUILD"), Some(value) if !value.is_empty());

fn main() {
    dim_agent_capabilities::build::install(None, None, RELEASE);
    #[cfg(target_os = "linux")]
    unsafe {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        mallopt(-3, 64 * 1024);
        mallopt(-8, 1);
    }
    if let Err(error) =
        dim_app_server::cli::process_entry(std::env::args().skip(1), None, |_| {}, || Ok(()))
    {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
