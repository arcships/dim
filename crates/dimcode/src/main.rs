//! One executable for the product: the terminal client, which starts this
//! same file as its App Server daemon with the hidden `app-server`
//! subcommand, and as the OCR helper with the hidden `ocr-helper`
//! subcommand.

#![allow(
    linker_messages,
    reason = "Apple's ld prints the compact-unwind message only when the debug binary links, which a check build never does; an expectation would be unfulfilled there"
)]

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
    dim_image_understanding::install(builder);
}

/// The commit this source build was made from, which `build.rs` reads at build
/// time. A release build gets nothing here, so its output cannot change.
const GIT_COMMIT: Option<&str> = match option_env!("DIM_GIT_COMMIT") {
    Some(commit) if !commit.is_empty() => Some(commit),
    _ => None,
};

/// The stamp of the artifact this build produced, which `build.rs` reads from a
/// packaging caller. Two builds of one version carry different stamps; a source
/// build nobody stamped carries none.
const BUILD_ID: Option<&str> = match option_env!("DIM_BUILD_ID") {
    Some(build_id) if !build_id.is_empty() => Some(build_id),
    _ => None,
};

/// Whether the release flow produced this build, which `build.rs` forwards from
/// `DIM_RELEASE_BUILD`. Keeping it in the final binary crate means toggling
/// `DIM_RELEASE_BUILD` never recompiles upstream crates.
const RELEASE: bool = present(option_env!("DIM_RELEASE_BUILD")).is_some();

fn main() {
    // Both roles this executable serves read one installed identity: the CLI's
    // `--version`, the daemon's report `Dim` line and its MCP client
    // declaration, so a log, a report and a screenshot name one commit. The
    // client and the daemon also compare their stamps, so a client that meets a
    // daemon of another build of the same version can say so.
    dim_agent_capabilities::build::install(GIT_COMMIT, BUILD_ID, RELEASE);
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
    // The offline store commands run without the `app-server` prefix: they
    // touch a database no daemon serves and exit, so a bare `dimcode
    // --migrate-prompt-references ...` is the whole invocation the guard's
    // error asks for. It goes through the App Server's list, which keeps one
    // implementation and one usage text for both entry points.
    if std::env::args().nth(1).as_deref() == Some("--migrate-prompt-references") {
        if let Err(error) = dim_app_server::cli::run(std::env::args().skip(1)) {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
        return;
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
