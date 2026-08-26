use std::{process::ExitCode, str::FromStr};

use cubic_network::{ServerAddress, StatusQueryOptions, query_server_status};

fn main() -> ExitCode {
    if let Err(error) = tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .try_init()
    {
        eprintln!("failed to initialize diagnostics: {error}");
    }

    tracing::info!("{}", cubic_core::startup_message());

    let command = match parse_command(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!(
                "{message}\n\nUsage:\n  cubic-app\n  cubic-app status <host[:port]> [--protocol <number>]"
            );
            return ExitCode::FAILURE;
        }
    };

    match command {
        Command::Graphical => run_graphical(),
        Command::Status { address, protocol } => run_status(&address, protocol),
    }
}

fn run_graphical() -> ExitCode {
    match cubic_platform::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "Cubic stopped because application initialization failed");
            ExitCode::FAILURE
        }
    }
}

fn run_status(address: &ServerAddress, protocol: i32) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not create status-query runtime");
            return ExitCode::FAILURE;
        }
    };
    let options = StatusQueryOptions {
        handshake_protocol_version: protocol,
        ..StatusQueryOptions::default()
    };
    match runtime.block_on(query_server_status(address, &options)) {
        Ok(status) => {
            let response = &status.response;
            let motd = response
                .motd_preview()
                .map(bounded_preview)
                .unwrap_or_else(|| "<complex JSON component preserved>".to_owned());
            println!("Cubic Server Status\n");
            println!("Address: {}", status.address);
            println!("Version: {}", response.version.name);
            println!("Protocol: {}", response.version.protocol);
            println!(
                "Players: {} / {}",
                response.players.online, response.players.max
            );
            println!("Ping: {} ms", status.latency.as_millis());
            println!("MOTD: {motd}");
            println!(
                "Favicon: {}",
                if response.favicon.is_some() {
                    "present (not decoded)"
                } else {
                    "not supplied"
                }
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, address = %address, "server status query failed");
            ExitCode::FAILURE
        }
    }
}

fn bounded_preview(value: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 512;
    let mut preview: String = value.chars().take(MAX_PREVIEW_CHARS).collect();
    if value.chars().count() > MAX_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Graphical,
    Status {
        address: ServerAddress,
        protocol: i32,
    },
}

fn parse_command(arguments: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(Command::Graphical);
    };
    if command != "status" {
        return Err(format!("unknown command {command:?}"));
    }
    let target = arguments
        .next()
        .ok_or_else(|| "status command requires a server address".to_owned())?;
    let address = ServerAddress::from_str(&target).map_err(|error| error.to_string())?;
    let mut protocol = StatusQueryOptions::default().handshake_protocol_version;
    if let Some(option) = arguments.next() {
        if option != "--protocol" {
            return Err(format!("unknown status option {option:?}"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| "--protocol requires a signed 32-bit number".to_owned())?;
        protocol = value
            .parse::<i32>()
            .map_err(|_| "--protocol requires a signed 32-bit number".to_owned())?;
    }
    if let Some(extra) = arguments.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    Ok(Command::Status { address, protocol })
}

#[cfg(test)]
mod tests {
    use super::{Command, bounded_preview, parse_command};

    #[test]
    fn no_arguments_preserve_graphical_mode() {
        assert_eq!(parse_command(Vec::new()), Ok(Command::Graphical));
    }

    #[test]
    fn status_command_supports_explicit_protocol() {
        let command = parse_command([
            "status".to_owned(),
            "localhost:25570".to_owned(),
            "--protocol".to_owned(),
            "-1".to_owned(),
        ])
        .unwrap();
        let Command::Status { address, protocol } = command else {
            panic!("expected status command");
        };
        assert_eq!(address.host(), "localhost");
        assert_eq!(address.port(), 25_570);
        assert_eq!(protocol, -1);
    }

    #[test]
    fn status_command_rejects_unknown_or_incomplete_arguments() {
        assert!(parse_command(["status".to_owned()]).is_err());
        assert!(parse_command(["other".to_owned()]).is_err());
        assert!(
            parse_command([
                "status".to_owned(),
                "localhost".to_owned(),
                "--protocol".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn printed_motd_preview_is_bounded() {
        let preview = bounded_preview(&"x".repeat(600));
        assert_eq!(preview.chars().count(), 513);
        assert!(preview.ends_with('…'));
    }
}
