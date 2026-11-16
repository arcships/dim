//! One executable for the product: the terminal client, which starts this
//! same file as its App Server daemon with the hidden `app-server`
//! subcommand, and as the OCR helper with the hidden `ocr-helper`
//! subcommand.

#![allow(linker_messages)]

/// The Dim account's OAuth client IDs, injected at build time so that they
/// never enter the repository. With both set the executable is the account
/// build; with neither it is the API-key build.
const CLIENT_IDS: Option<(&str, &str)> = match (
    present(option_env!("DIM_OAUTH_CLIENT_ID_CN")),
    present(option_env!("DIM_OAUTH_CLIENT_ID_GLOBAL")),
) {
    (Some(cn), Some(global)) => Some((cn, global)),
    (None, None) => None,
    _ => panic!("set DIM_OAUTH_CLIENT_ID_CN and DIM_OAUTH_CLIENT_ID_GLOBAL together"),
};

/// CI exports an unset secret as an empty variable.
const fn present(value: Option<&'static str>) -> Option<&'static str> {
    match value {
        Some(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// The product's native plugins, added to the App Server before assembly.
fn install(builder: &mut dim_agent_runtime::AgentBuilder) {
    dim_session_title::install(builder);
    dim_agent_scheduler::install(builder);
    dim_event_hub::install(builder);
    dim_goal::install(builder);
    dim_report::install(builder);
    dim_modality_aimux::install(builder);
    // Host tasks run this executable with the hidden `ocr-helper`
    // subcommand.
    dim_modality::install_with(
        builder,
        dim_modality::InstallOptions {
            ocr_helper: std::env::current_exe().ok(),
            ..dim_modality::InstallOptions::default()
        },
    );
}

/// The commit this source build was made from, which `build.rs` reads at build
/// time. A release build gets nothing here, so its output cannot change.
const GIT_COMMIT: Option<&str> = match option_env!("DIM_GIT_COMMIT") {
    Some(commit) if !commit.is_empty() => Some(commit),
    _ => None,
};

fn main() {
    // Both roles this executable serves read one installed identity: the CLI's
    // `--version`, the daemon's report `Dim` line and its MCP client
    // declaration, so a log, a report and a screenshot name one commit.
    dim_agent_capabilities::build::install(GIT_COMMIT);
    #[cfg(target_os = "linux")]
    unsafe {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        mallopt(-3, 64 * 1024);
        mallopt(-8, 1);
    }
    if std::env::args().nth(1).as_deref() == Some("ocr-helper") {
        let mut stdout = std::io::stdout();
        let mut stderr = std::io::stderr();
        std::process::exit(dim_ocr::helper_main(
            std::env::args_os().skip(2),
            &mut stdout,
            &mut stderr,
        ));
    }
    if std::env::args().nth(1).as_deref() == Some("app-server") {
        let provider = CLIENT_IDS.map(|(cn, global)| {
            dim_oauth::row(dim_oauth::ClientIds {
                cn: cn.into(),
                global: global.into(),
            })
        });
        if let Err(error) = dim_app_server::cli::process_entry(
            std::env::args().skip(2),
            provider,
            install,
            dim_cli::hold_installation,
        ) {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
        return;
    }
    std::process::exit(dim_cli::run_cli());
}
