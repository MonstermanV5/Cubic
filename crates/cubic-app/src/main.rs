use std::{process::ExitCode, str::FromStr, time::Duration};

use cubic_auth::{
    AuthBackend, AuthClient, AuthClientOptions, AuthenticatedMinecraftAccount, CredentialStore,
    LoopbackAuthorization, MicrosoftClientId, MinecraftSessionJoiner, PlayerCertificateClient,
    StoredAccount, SystemCredentialStore, XalAuthClient, XalDeviceIdentity,
};
use cubic_network::{
    AuthenticatedLoginOptions, ChatSessionHandle, ChatSessionOptions, DevelopmentLoginOptions,
    DevelopmentUsername, ServerAddress, StatusQueryOptions, WorldRenderHandle, authenticated_login,
    development_login, query_server_status, run_authenticated_chat_session,
    run_development_chat_session, run_development_world_session,
};
use cubic_ui::ChatSessionPort;
use cubic_version::{GameData, MinecraftVersionId};
use cubic_world::BlockVisualProfile;

mod logging;

const XAL_SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

fn main() -> ExitCode {
    if let Err(error) = logging::initialize() {
        eprintln!("failed to initialize diagnostics: {error}");
        logging::initialize_stderr_only();
    }

    tracing::info!("{}", cubic_core::startup_message());

    let command = match parse_command(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!(
                "{message}\n\nUsage:\n  cubic-app\n  cubic-app status <host[:port]> [--protocol <number>]\n  cubic-app dev-login <host[:port]> [--username <name>]\n  cubic-app chat <host[:port]> [--username <name> | --backend cubic-entra|xal]\n  cubic-app world <host[:port]> [--username <name>]\n  cubic-app auth login [--backend cubic-entra|xal]\n  cubic-app auth status [--backend cubic-entra|xal]\n  cubic-app auth logout [--backend cubic-entra|xal]\n  cubic-app online-login <host[:port]> [--backend cubic-entra|xal]\n  cubic-app bootstrap-version <version-id> [--client-jar]"
            );
            return ExitCode::FAILURE;
        }
    };

    match command {
        Command::Graphical => run_graphical(),
        Command::Status { address, protocol } => run_status(&address, protocol),
        Command::DevLogin { address, username } => run_development_login(&address, &username),
        Command::Chat {
            address,
            username,
            backend,
        } => run_chat(address, username, backend),
        Command::World { address, username } => run_world(address, username),
        Command::Auth(action) => run_auth(action),
        Command::OnlineLogin { address, backend } => run_online_login(&address, backend),
        Command::BootstrapVersion {
            version,
            client_jar,
        } => run_bootstrap_version(&version, client_jar),
    }
}

fn run_bootstrap_version(version: &MinecraftVersionId, ensure_client: bool) -> ExitCode {
    let Some(data_root) = cubic_platform::persistent_data_directory() else {
        tracing::error!("platform data directory is unavailable");
        return ExitCode::FAILURE;
    };
    let cache_root = data_root.join("cache").join("minecraft");
    let bootstrap = match cubic_resources::OfficialVersionBootstrap::new(&cache_root) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "could not initialize official version bootstrap");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "could not create version-bootstrap runtime");
            return ExitCode::FAILURE;
        }
    };
    let result = match runtime.block_on(bootstrap.bootstrap(version)) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, version = %version, "official version bootstrap failed");
            return ExitCode::FAILURE;
        }
    };
    let mut client_cached = result.client_jar_cached;
    if ensure_client {
        match runtime.block_on(bootstrap.ensure_client_jar(&result.metadata)) {
            Ok(artifact) => {
                client_cached = true;
                tracing::info!(path = %artifact.path().display(), "verified official client JAR cached without execution");
            }
            Err(error) => {
                tracing::error!(%error, "official client JAR acquisition failed");
                return ExitCode::FAILURE;
            }
        }
    }
    println!("Cubic Official Version Bootstrap\n");
    println!("Requested version: {version}");
    println!("Resolved version: {}", result.metadata.id);
    println!("Version type: {:?}", result.metadata.kind);
    println!("Source: {:?}", result.source);
    println!("Asset index: {}", result.metadata.asset_index.id);
    println!("Indexed assets: {}", result.assets.len());
    println!("Client JAR size: {}", result.metadata.client.size);
    println!("Client JAR SHA-1: {}", result.metadata.client.sha1);
    println!("Client JAR cached: {client_cached}");
    println!("Cache root: {}", result.cache_root.display());
    println!("Bootstrap: success");
    ExitCode::SUCCESS
}

fn configured_auth_client() -> Result<AuthClient, String> {
    let raw = std::env::var("CUBIC_MSA_CLIENT_ID")
        .map_err(|_| "CUBIC_MSA_CLIENT_ID is not configured".to_owned())?;
    let client_id = MicrosoftClientId::new(raw).map_err(|error| error.to_string())?;
    AuthClient::new(client_id, AuthClientOptions::default()).map_err(|error| error.to_string())
}

fn run_online_login(address: &ServerAddress, backend: AuthBackend) -> ExitCode {
    let store = SystemCredentialStore;
    let stored = match store.load_account(backend) {
        Ok(Some(value)) => value,
        Ok(None) => {
            tracing::error!(%backend, "not signed in with this backend; run `cubic-app auth login --backend ...` first");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            tracing::error!(%error, "could not read Cubic credentials");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "could not create online-login runtime");
            return ExitCode::FAILURE;
        }
    };
    match backend {
        AuthBackend::CubicEntra => {
            let client = match configured_auth_client() {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "authentication configuration is invalid");
                    return ExitCode::FAILURE;
                }
            };
            let account = match runtime.block_on(client.refresh(&stored.refresh_token)) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "stored Cubic-Entra authentication could not be refreshed; run `auth login` again");
                    return ExitCode::FAILURE;
                }
            };
            complete_online_login(address, backend, &store, &runtime, &client, account)
        }
        AuthBackend::XalInterop => {
            let credential = match store.load_xal_device() {
                Ok(Some(value)) => value,
                Ok(None) => {
                    tracing::error!(
                        "XAL device identity is missing; run `auth login --backend xal`"
                    );
                    return ExitCode::FAILURE;
                }
                Err(error) => {
                    tracing::error!(%error, "could not read the XAL device identity");
                    return ExitCode::FAILURE;
                }
            };
            let device = match XalDeviceIdentity::from_credential(&credential) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "stored XAL device identity is invalid");
                    return ExitCode::FAILURE;
                }
            };
            let client = match XalAuthClient::new(AuthClientOptions::default()) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "could not initialize the XAL client");
                    return ExitCode::FAILURE;
                }
            };
            let account = match runtime.block_on(client.refresh(&device, &stored.refresh_token)) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "stored XAL authentication could not be refreshed; run `auth login --backend xal` again");
                    return ExitCode::FAILURE;
                }
            };
            complete_online_login(address, backend, &store, &runtime, &client, account)
        }
    }
}

fn complete_online_login<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    backend: AuthBackend,
    store: &dyn CredentialStore,
    runtime: &tokio::runtime::Runtime,
    joiner: &J,
    account: AuthenticatedMinecraftAccount,
) -> ExitCode {
    let rotated = StoredAccount {
        backend,
        refresh_token: account.refresh_token.clone(),
        profile: account.profile.clone(),
    };
    if let Err(error) = store.save_account(backend, &rotated) {
        tracing::error!(%error, "refreshed credentials could not be stored safely");
        return ExitCode::FAILURE;
    }
    match runtime.block_on(authenticated_login(
        address,
        &account,
        joiner,
        &AuthenticatedLoginOptions::default(),
    )) {
        Ok(result) => {
            println!("Cubic Authenticated Login\n");
            println!("Address: {}", result.address);
            println!("Backend: {backend}");
            println!("Profile: {}", result.profile_name);
            println!("UUID: {:032x}", result.profile_uuid.as_u128());
            println!(
                "Compression: {}",
                if result.compression_enabled {
                    "enabled"
                } else {
                    "not requested"
                }
            );
            println!("State: {}", result.state);
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, address = %address, "authenticated online login failed");
            ExitCode::FAILURE
        }
    }
}

fn run_auth(action: AuthAction) -> ExitCode {
    let store = SystemCredentialStore;
    match action {
        AuthAction::Status { backend } => run_auth_status(&store, backend),
        AuthAction::Logout { backend } => match store.delete_account(backend) {
            Ok(()) => {
                println!("Cubic authentication credentials removed for {backend}.");
                ExitCode::SUCCESS
            }
            Err(error) => {
                tracing::error!(%error, "could not remove Cubic credentials");
                ExitCode::FAILURE
            }
        },
        AuthAction::Login { backend } => match backend {
            AuthBackend::CubicEntra => run_interactive_entra_auth(&store),
            AuthBackend::XalInterop => run_interactive_xal_auth(&store),
        },
    }
}

fn run_auth_status(store: &dyn CredentialStore, selected: Option<AuthBackend>) -> ExitCode {
    let backends: &[AuthBackend] = selected.as_ref().map_or(
        &[AuthBackend::CubicEntra, AuthBackend::XalInterop],
        std::slice::from_ref,
    );
    for backend in backends {
        match store.load_account(*backend) {
            Ok(Some(account)) => {
                println!("Backend: {backend}");
                println!("Signed in: yes");
                println!("Minecraft profile: {}", account.profile.name);
                println!("UUID: {}", account.profile.id);
                println!("Token status: refresh required before online use\n");
            }
            Ok(None) => {
                println!("Backend: {backend}");
                println!("Signed in: no\n");
            }
            Err(error) => {
                tracing::error!(%error, %backend, "could not read Cubic credentials");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_interactive_entra_auth(store: &dyn CredentialStore) -> ExitCode {
    let client_id = match std::env::var("CUBIC_MSA_CLIENT_ID") {
        Ok(value) => match MicrosoftClientId::new(value) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(%error, "invalid CUBIC_MSA_CLIENT_ID");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => {
            tracing::error!("CUBIC_MSA_CLIENT_ID is not configured");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not create authentication runtime");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        let pending = LoopbackAuthorization::begin(&client_id).await?;
        let url = pending.authorization_url().as_str();
        println!(
            "Open this Microsoft authorization URL if the system browser does not open:\n{url}"
        );
        open_system_browser(url);
        let code = pending.wait().await?;
        let client = AuthClient::new(client_id, AuthClientOptions::default())?;
        client.authenticate_code(code).await
    });
    match result {
        Ok(account) => {
            let stored = StoredAccount {
                backend: AuthBackend::CubicEntra,
                refresh_token: account.refresh_token,
                profile: account.profile,
            };
            if let Err(error) = store.save_account(AuthBackend::CubicEntra, &stored) {
                tracing::error!(%error, "authentication succeeded but secure credential storage failed");
                return ExitCode::FAILURE;
            }
            println!("Cubic authentication succeeded.");
            println!("Minecraft profile: {}", stored.profile.name);
            println!("UUID: {}", stored.profile.id);
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "Cubic authentication failed");
            ExitCode::FAILURE
        }
    }
}

fn run_interactive_xal_auth(store: &dyn CredentialStore) -> ExitCode {
    println!("EXPERIMENTAL: XAL/SISU interoperability is for development testing only.");
    let device = match store.load_xal_device() {
        Ok(Some(value)) => match XalDeviceIdentity::from_credential(&value) {
            Ok(device) => device,
            Err(error) => {
                tracing::error!(%error, "stored XAL device identity is invalid");
                return ExitCode::FAILURE;
            }
        },
        Ok(None) => {
            let device = XalDeviceIdentity::generate();
            let credential = match device.to_credential() {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "could not encode the XAL device identity");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(error) = store.save_xal_device(&credential) {
                tracing::error!(%error, "could not persist the XAL device identity securely");
                return ExitCode::FAILURE;
            }
            device
        }
        Err(error) => {
            tracing::error!(%error, "could not read the XAL device identity");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not create authentication runtime");
            return ExitCode::FAILURE;
        }
    };
    let client = match XalAuthClient::new(AuthClientOptions::default()) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "could not initialize the XAL client");
            return ExitCode::FAILURE;
        }
    };
    let pending = match runtime.block_on(client.begin_interactive(&device)) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "XAL device/SISU authentication could not begin");
            return ExitCode::FAILURE;
        }
    };
    println!("Opening the dedicated Microsoft sign-in window...");
    let authorization_code =
        match cubic_platform::capture_xal_authorization(&pending, XAL_SIGN_IN_TIMEOUT) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(%error, "experimental XAL sign-in window failed");
                return ExitCode::FAILURE;
            }
        };
    let account =
        match runtime.block_on(client.complete_interactive(&device, pending, authorization_code)) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(%error, "experimental XAL authentication failed");
                return ExitCode::FAILURE;
            }
        };
    let stored = StoredAccount {
        backend: AuthBackend::XalInterop,
        refresh_token: account.refresh_token,
        profile: account.profile,
    };
    if let Err(error) = store.save_account(AuthBackend::XalInterop, &stored) {
        tracing::error!(%error, "authentication succeeded but secure credential storage failed");
        return ExitCode::FAILURE;
    }
    println!("Experimental XAL authentication succeeded.");
    println!("Minecraft profile: {}", stored.profile.name);
    println!("UUID: {}", stored.profile.id);
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn open_system_browser(url: &str) {
    if let Err(error) = webbrowser::open(url) {
        tracing::warn!(%error, "could not open the system browser automatically");
    }
}

#[cfg(not(windows))]
fn open_system_browser(_url: &str) {
    tracing::warn!("automatic browser opening is not implemented for this target");
}

fn run_chat(
    address: ServerAddress,
    username: DevelopmentUsername,
    backend: Option<AuthBackend>,
) -> ExitCode {
    let options = ChatSessionOptions::default();
    let (handle, runner) = ChatSessionHandle::bounded(&options);
    let network_address = address.clone();
    let network_username = username.clone();
    let network = std::thread::Builder::new()
        .name("cubic-chat-network".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(runtime) => match backend {
                    Some(backend) => runtime.block_on(run_authenticated_chat_backend(
                        &network_address,
                        backend,
                        runner,
                    )),
                    None => runtime
                        .block_on(run_development_chat_session(
                            &network_address,
                            &network_username,
                            &options,
                            runner,
                        ))
                        .map_err(|error| error.to_string()),
                },
                Err(error) => {
                    tracing::error!(%error, "could not create Chat Mode runtime");
                    return;
                }
            };
            if let Err(error) = result {
                tracing::error!(%error, "Chat Mode network task stopped");
            }
        });
    let network = match network {
        Ok(network) => network,
        Err(error) => {
            tracing::error!(%error, "could not start Chat Mode network thread");
            return ExitCode::FAILURE;
        }
    };

    match backend {
        Some(backend) => {
            tracing::info!(address = %address, %backend, "opening authenticated Chat Mode")
        }
        None => {
            tracing::info!(address = %address, username = %username, "opening development Chat Mode")
        }
    }
    let result = cubic_platform::run_chat(Box::new(NetworkChatPort { handle }));
    if network.join().is_err() {
        tracing::error!("Chat Mode network thread panicked");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "Cubic Chat Mode stopped because initialization failed");
            ExitCode::FAILURE
        }
    }
}

async fn run_authenticated_chat_backend(
    address: &ServerAddress,
    backend: AuthBackend,
    runner: cubic_network::ChatSessionRunner,
) -> Result<(), String> {
    let store = SystemCredentialStore;
    let stored = store
        .load_account(backend)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!("not signed in with {backend}; run `auth login --backend ...` first")
        })?;
    match backend {
        AuthBackend::CubicEntra => {
            let client = configured_auth_client()?;
            let account = client
                .refresh(&stored.refresh_token)
                .await
                .map_err(|error| error.to_string())?;
            complete_authenticated_chat(address, backend, &store, &client, account, runner).await
        }
        AuthBackend::XalInterop => {
            let credential = store
                .load_xal_device()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "XAL device identity is missing; run `auth login --backend xal`".to_owned()
                })?;
            let device = XalDeviceIdentity::from_credential(&credential)
                .map_err(|error| error.to_string())?;
            let client = XalAuthClient::new(AuthClientOptions::default())
                .map_err(|error| error.to_string())?;
            let account = client
                .refresh(&device, &stored.refresh_token)
                .await
                .map_err(|error| error.to_string())?;
            complete_authenticated_chat(address, backend, &store, &client, account, runner).await
        }
    }
}

fn run_world(address: ServerAddress, username: DevelopmentUsername) -> ExitCode {
    let Some(data_root) = cubic_platform::persistent_data_directory() else {
        tracing::error!("platform data directory is unavailable");
        return ExitCode::FAILURE;
    };
    let version = match MinecraftVersionId::new("26.1.2") {
        Ok(version) => version,
        Err(error) => {
            tracing::error!(%error, "built-in world profile is invalid");
            return ExitCode::FAILURE;
        }
    };
    let data = match GameData::load(&data_root.join("generated").join("game-data"), &version) {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "World Mode requires the Phase 11 generated game-data artifact");
            return ExitCode::FAILURE;
        }
    };
    let visual = match BlockVisualProfile::from_game_data(&data) {
        Ok(visual) => visual,
        Err(error) => {
            tracing::error!(%error, "could not classify renderable block states");
            return ExitCode::FAILURE;
        }
    };
    let options = ChatSessionOptions::default();
    let (chat_handle, chat_runner) = ChatSessionHandle::bounded(&options);
    let (world_handle, world_runner) = WorldRenderHandle::new();
    let network_address = address.clone();
    let network_username = username.clone();
    let network = std::thread::Builder::new()
        .name("cubic-world-network".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    if let Err(error) = runtime.block_on(run_development_world_session(
                        &network_address,
                        &network_username,
                        &options,
                        chat_runner,
                        world_runner,
                    )) {
                        tracing::error!(%error, "World Mode network task stopped");
                    }
                }
                Err(error) => tracing::error!(%error, "could not create World Mode runtime"),
            }
        });
    let network = match network {
        Ok(network) => network,
        Err(error) => {
            tracing::error!(%error, "could not start World Mode network thread");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(address = %address, username = %username, "opening development World Mode");
    let result = cubic_platform::run_world(
        Box::new(NetworkWorldPort {
            chat: chat_handle,
            world: world_handle,
        }),
        visual,
    );
    if network.join().is_err() {
        tracing::error!("World Mode network thread panicked");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "Cubic World Mode stopped because initialization failed");
            ExitCode::FAILURE
        }
    }
}

struct NetworkWorldPort {
    chat: ChatSessionHandle,
    world: WorldRenderHandle,
}

impl cubic_platform::WorldSessionPort for NetworkWorldPort {
    fn take_world_update(&mut self) -> Option<cubic_world::WorldRenderUpdate> {
        while self.chat.try_next_event().is_some() {}
        let _critical = self.chat.take_critical_event();
        self.world.take_update()
    }

    fn disconnect(&self) {
        let _result = self.chat.disconnect();
    }
}

async fn complete_authenticated_chat<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    backend: AuthBackend,
    store: &dyn CredentialStore,
    joiner: &J,
    account: AuthenticatedMinecraftAccount,
    runner: cubic_network::ChatSessionRunner,
) -> Result<(), String> {
    store
        .save_account(
            backend,
            &StoredAccount {
                backend,
                refresh_token: account.refresh_token.clone(),
                profile: account.profile.clone(),
            },
        )
        .map_err(|error| error.to_string())?;
    let certificate = PlayerCertificateClient::new(AuthClientOptions::default())
        .map_err(|error| error.to_string())?
        .request(&account)
        .await
        .map_err(|error| error.to_string())?;
    run_authenticated_chat_session(
        address,
        &account,
        joiner,
        certificate,
        &AuthenticatedLoginOptions::default(),
        runner,
    )
    .await
    .map_err(|error| error.to_string())
}

struct NetworkChatPort {
    handle: ChatSessionHandle,
}

impl ChatSessionPort for NetworkChatPort {
    fn try_next_event(&mut self) -> Option<cubic_core::ChatEvent> {
        self.handle.try_next_event()
    }

    fn take_critical_event(&mut self) -> Option<cubic_core::ChatEvent> {
        self.handle.take_critical_event()
    }

    fn dropped_event_count(&mut self) -> usize {
        self.handle.dropped_event_count()
    }

    fn send_message(&mut self, message: String) -> Result<(), String> {
        self.handle
            .try_send_message(message)
            .map_err(|error| error.to_string())
    }

    fn disconnect(&mut self) {
        let _result = self.handle.disconnect();
    }
}

fn run_development_login(address: &ServerAddress, username: &DevelopmentUsername) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not create development-login runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(development_login(
        address,
        username,
        &DevelopmentLoginOptions::default(),
    )) {
        Ok(result) => {
            println!("Cubic Development Login\n");
            println!("Address: {}", result.address);
            println!("Version: {}", result.minecraft_version);
            println!("Protocol: {}", result.protocol_version);
            println!("Username: {}", result.username);
            println!("Login: success");
            println!("Configuration: complete");
            println!("State: {}", result.state);
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, address = %address, "development login failed");
            ExitCode::FAILURE
        }
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
    DevLogin {
        address: ServerAddress,
        username: DevelopmentUsername,
    },
    Chat {
        address: ServerAddress,
        username: DevelopmentUsername,
        backend: Option<AuthBackend>,
    },
    World {
        address: ServerAddress,
        username: DevelopmentUsername,
    },
    Auth(AuthAction),
    OnlineLogin {
        address: ServerAddress,
        backend: AuthBackend,
    },
    BootstrapVersion {
        version: MinecraftVersionId,
        client_jar: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthAction {
    Login { backend: AuthBackend },
    Status { backend: Option<AuthBackend> },
    Logout { backend: AuthBackend },
}

fn parse_command(arguments: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(Command::Graphical);
    };
    if command == "auth" {
        let action_name = arguments.next();
        let backend = parse_optional_backend(&mut arguments)?;
        let action = match action_name.as_deref() {
            Some("login") => AuthAction::Login {
                backend: backend.unwrap_or(AuthBackend::CubicEntra),
            },
            Some("status") => AuthAction::Status { backend },
            Some("logout") => AuthAction::Logout {
                backend: backend.unwrap_or(AuthBackend::CubicEntra),
            },
            Some(value) => return Err(format!("unknown auth action {value:?}")),
            None => return Err("auth command requires login, status, or logout".to_owned()),
        };
        if let Some(extra) = arguments.next() {
            return Err(format!("unexpected auth argument {extra:?}"));
        }
        return Ok(Command::Auth(action));
    }
    if command == "online-login" {
        let target = arguments
            .next()
            .ok_or_else(|| "online-login command requires a server address".to_owned())?;
        let address = ServerAddress::from_str(&target).map_err(|error| error.to_string())?;
        let backend = parse_optional_backend(&mut arguments)?.unwrap_or(AuthBackend::CubicEntra);
        if let Some(extra) = arguments.next() {
            return Err(format!("unexpected argument {extra:?}"));
        }
        return Ok(Command::OnlineLogin { address, backend });
    }
    if command == "bootstrap-version" {
        let raw = arguments
            .next()
            .ok_or_else(|| "bootstrap-version requires a Minecraft version ID".to_owned())?;
        let version = MinecraftVersionId::new(raw).map_err(|error| error.to_string())?;
        let client_jar = match arguments.next().as_deref() {
            None => false,
            Some("--client-jar") => true,
            Some(value) => return Err(format!("unknown bootstrap-version option {value:?}")),
        };
        if let Some(extra) = arguments.next() {
            return Err(format!("unexpected argument {extra:?}"));
        }
        return Ok(Command::BootstrapVersion {
            version,
            client_jar,
        });
    }
    if command == "dev-login" {
        return parse_development_login(arguments);
    }
    if command == "chat" {
        return parse_chat(arguments);
    }
    if command == "world" {
        return parse_server_and_username("world", arguments)
            .map(|(address, username)| Command::World { address, username });
    }
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

fn parse_optional_backend(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<Option<AuthBackend>, String> {
    let Some(option) = arguments.next() else {
        return Ok(None);
    };
    if option != "--backend" {
        return Err(format!("unexpected auth argument {option:?}"));
    }
    let value = arguments
        .next()
        .ok_or_else(|| "--backend requires cubic-entra or xal".to_owned())?;
    AuthBackend::from_str(&value)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn parse_chat(arguments: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments;
    let target = arguments
        .next()
        .ok_or_else(|| "chat command requires a server address".to_owned())?;
    let address = ServerAddress::from_str(&target).map_err(|error| error.to_string())?;
    let mut username = DevelopmentUsername::new("CubicTest").map_err(|error| error.to_string())?;
    let mut backend = None;
    if let Some(option) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{option} requires a value"))?;
        match option.as_str() {
            "--username" => {
                username = DevelopmentUsername::new(value).map_err(|error| error.to_string())?;
            }
            "--backend" => {
                backend = Some(AuthBackend::from_str(&value).map_err(|error| error.to_string())?);
            }
            _ => return Err(format!("unknown chat option {option:?}")),
        }
    }
    if let Some(extra) = arguments.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    Ok(Command::Chat {
        address,
        username,
        backend,
    })
}

fn parse_development_login(arguments: impl Iterator<Item = String>) -> Result<Command, String> {
    parse_server_and_username("dev-login", arguments)
        .map(|(address, username)| Command::DevLogin { address, username })
}

fn parse_server_and_username(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<(ServerAddress, DevelopmentUsername), String> {
    let target = arguments
        .next()
        .ok_or_else(|| format!("{command} command requires a server address"))?;
    let address = ServerAddress::from_str(&target).map_err(|error| error.to_string())?;
    let mut username = DevelopmentUsername::new("CubicTest").map_err(|error| error.to_string())?;
    if let Some(option) = arguments.next() {
        if option != "--username" {
            return Err(format!("unknown dev-login option {option:?}"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| "--username requires a value".to_owned())?;
        username = DevelopmentUsername::new(value).map_err(|error| error.to_string())?;
    }
    if let Some(extra) = arguments.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    Ok((address, username))
}

#[cfg(test)]
mod tests {
    use super::{AuthAction, AuthBackend, Command, bounded_preview, parse_command};

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
    fn bootstrap_version_is_typed_and_client_download_is_explicit() {
        let command = parse_command([
            "bootstrap-version".to_owned(),
            "26.1.2".to_owned(),
            "--client-jar".to_owned(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::BootstrapVersion { version, client_jar: true } if version.as_str() == "26.1.2"
        ));
        assert!(parse_command(["bootstrap-version".to_owned(), "../escape".to_owned()]).is_err());
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
    fn development_login_defaults_and_accepts_an_explicit_username() {
        let default = parse_command(["dev-login".to_owned(), "localhost".to_owned()]).unwrap();
        let Command::DevLogin { address, username } = default else {
            panic!("expected development login command");
        };
        assert_eq!(address.port(), 25_565);
        assert_eq!(username.as_str(), "CubicTest");

        let explicit = parse_command([
            "dev-login".to_owned(),
            "localhost:25570".to_owned(),
            "--username".to_owned(),
            "Cubic_7".to_owned(),
        ])
        .unwrap();
        let Command::DevLogin { username, .. } = explicit else {
            panic!("expected development login command");
        };
        assert_eq!(username.as_str(), "Cubic_7");
    }

    #[test]
    fn development_login_rejects_invalid_arguments() {
        assert!(parse_command(["dev-login".to_owned()]).is_err());
        assert!(
            parse_command([
                "dev-login".to_owned(),
                "localhost".to_owned(),
                "--username".to_owned(),
                "bad name".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn chat_command_uses_the_same_bounded_development_identity() {
        let command = parse_command([
            "chat".to_owned(),
            "localhost:25565".to_owned(),
            "--username".to_owned(),
            "CubicChat".to_owned(),
        ])
        .unwrap();
        let Command::Chat {
            address,
            username,
            backend,
        } = command
        else {
            panic!("expected chat command")
        };
        assert_eq!(address.port(), 25_565);
        assert_eq!(username.as_str(), "CubicChat");
        assert_eq!(backend, None);
        assert!(parse_command(["chat".to_owned()]).is_err());

        let authenticated = parse_command([
            "chat".to_owned(),
            "localhost:25565".to_owned(),
            "--backend".to_owned(),
            "xal".to_owned(),
        ])
        .unwrap();
        assert!(matches!(
            authenticated,
            Command::Chat {
                backend: Some(AuthBackend::XalInterop),
                ..
            }
        ));
    }

    #[test]
    fn world_command_is_separate_and_uses_the_bounded_development_identity() {
        let command = parse_command([
            "world".to_owned(),
            "localhost:25565".to_owned(),
            "--username".to_owned(),
            "CubicTest2".to_owned(),
        ])
        .unwrap();
        assert!(matches!(command, Command::World { address, username }
            if address.port() == 25_565 && username.as_str() == "CubicTest2"));
        assert!(parse_command(["world".to_owned()]).is_err());
    }

    #[test]
    fn printed_motd_preview_is_bounded() {
        let preview = bounded_preview(&"x".repeat(600));
        assert_eq!(preview.chars().count(), 513);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn authentication_commands_are_explicit_and_do_not_accept_secrets() {
        assert_eq!(
            parse_command(["auth".to_owned(), "status".to_owned()]),
            Ok(Command::Auth(AuthAction::Status { backend: None }))
        );
        assert!(
            parse_command(["auth".to_owned(), "login".to_owned(), "secret".to_owned()]).is_err()
        );
        let command =
            parse_command(["online-login".to_owned(), "localhost:25565".to_owned()]).unwrap();
        assert!(matches!(command, Command::OnlineLogin { .. }));
        assert_eq!(
            parse_command([
                "auth".to_owned(),
                "login".to_owned(),
                "--backend".to_owned(),
                "xal".to_owned(),
            ]),
            Ok(Command::Auth(AuthAction::Login {
                backend: AuthBackend::XalInterop
            }))
        );
        assert_eq!(
            parse_command([
                "auth".to_owned(),
                "status".to_owned(),
                "--backend".to_owned(),
                "xal".to_owned(),
            ]),
            Ok(Command::Auth(AuthAction::Status {
                backend: Some(AuthBackend::XalInterop)
            }))
        );
        assert!(matches!(
            parse_command([
                "online-login".to_owned(),
                "localhost:25565".to_owned(),
                "--backend".to_owned(),
                "xal".to_owned(),
            ]),
            Ok(Command::OnlineLogin {
                backend: AuthBackend::XalInterop,
                ..
            })
        ));
        assert_eq!(
            parse_command([
                "auth".to_owned(),
                "logout".to_owned(),
                "--backend".to_owned(),
                "xal".to_owned(),
            ]),
            Ok(Command::Auth(AuthAction::Logout {
                backend: AuthBackend::XalInterop
            }))
        );
    }
}
