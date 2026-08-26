use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(error) = tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .try_init()
    {
        eprintln!("failed to initialize diagnostics: {error}");
    }

    tracing::info!("{}", cubic_core::startup_message());

    match cubic_platform::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "Cubic stopped because application initialization failed");
            ExitCode::FAILURE
        }
    }
}
