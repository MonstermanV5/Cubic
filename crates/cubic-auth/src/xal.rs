use std::{
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use rand::RngCore;
use rand_core_06::OsRng;
use reqwest::{Client, header::HeaderMap, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::{
    AuthBackend, AuthClientOptions, AuthError, AuthStage, AuthenticatedMinecraftAccount,
    MinecraftProfile, MinecraftProfileId, MinecraftSessionJoiner, SecretString,
    XalDeviceCredential,
};

const XAL_INTEROP_CLIENT_ID: &str = "00000000402b5328";
const XAL_INTEROP_TITLE_ID: &str = "1794566092";
const XAL_REDIRECT_URI: &str = "https://login.live.com/oauth20_desktop.srf";
const XAL_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
const XAL_DEVICE_AUTH: &str = "https://device.auth.xboxlive.com/device/authenticate";
const XAL_SISU_AUTHENTICATE: &str = "https://sisu.xboxlive.com/authenticate";
const XAL_SISU_AUTHORIZE: &str = "https://sisu.xboxlive.com/authorize";
const XAL_XSTS_AUTHORIZE: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const XAL_OAUTH_TOKEN: &str = "https://login.live.com/oauth20_token.srf";
const MINECRAFT_LAUNCHER_LOGIN: &str = "https://api.minecraftservices.com/launcher/login";
const MINECRAFT_ENTITLEMENTS: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const MINECRAFT_PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_BROWSER_RESULT_BYTES: usize = 8 * 1024;
const WINDOWS_EPOCH_OFFSET_SECONDS: u64 = 11_644_473_600;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

/// Persistent proof-of-possession identity for the experimental XAL provider.
///
/// This type intentionally has no `Debug` or `Display` implementation.
pub struct XalDeviceIdentity {
    id: [u8; 16],
    key: SigningKey,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct XalProofKey {
    kty: &'static str,
    x: String,
    y: String,
    crv: &'static str,
    alg: &'static str,
    #[serde(rename = "use")]
    key_use: &'static str,
}

impl XalDeviceIdentity {
    pub fn generate() -> Self {
        let mut id = [0_u8; 16];
        rand::rng().fill_bytes(&mut id);
        id[6] = (id[6] & 0x0f) | 0x40;
        id[8] = (id[8] & 0x3f) | 0x80;
        Self {
            id,
            key: SigningKey::random(&mut OsRng),
        }
    }

    pub fn from_credential(value: &XalDeviceCredential) -> Result<Self, AuthError> {
        let id = parse_uuid(value.device_id.expose())?;
        let key_bytes = Zeroizing::new(
            STANDARD
                .decode(value.private_key.expose())
                .map_err(|_| AuthError::InvalidXalDevice)?,
        );
        let key = SigningKey::from_slice(&key_bytes).map_err(|_| AuthError::InvalidXalDevice)?;
        Ok(Self { id, key })
    }

    pub fn to_credential(&self) -> Result<XalDeviceCredential, AuthError> {
        Ok(XalDeviceCredential {
            device_id: SecretString::new(format_uuid(self.id, false)),
            private_key: SecretString::new(STANDARD.encode(self.key.to_bytes())),
        })
    }

    fn proof_key(&self) -> Result<XalProofKey, AuthError> {
        let point = VerifyingKey::from(&self.key).to_encoded_point(false);
        let x = point.x().ok_or(AuthError::InvalidXalDevice)?;
        let y = point.y().ok_or(AuthError::InvalidXalDevice)?;
        Ok(XalProofKey {
            kty: "EC",
            x: URL_SAFE_NO_PAD.encode(x),
            y: URL_SAFE_NO_PAD.encode(y),
            crv: "P-256",
            alg: "ES256",
            key_use: "sig",
        })
    }

    fn braced_upper_id(&self) -> String {
        format!("{{{}}}", format_uuid(self.id, true))
    }
}

/// Pending system-browser authorization returned by SISU.
///
/// Its verifier, state, session identifier, and device token are deliberately non-public and
/// redacted through their secret wrappers.
pub struct XalInteractiveAuthorization {
    authorization_url: Url,
    verifier: SecretString,
    expected_state: SecretString,
    session_id: SecretString,
    device_token: SecretString,
}

impl XalInteractiveAuthorization {
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    /// Returns an independent validator suitable for a platform navigation callback.
    #[must_use]
    pub fn redirect_validator(&self) -> XalRedirectValidator {
        XalRedirectValidator {
            expected_state: self.expected_state.clone(),
        }
    }
}

/// A captured XAL authorization code. Formatting never reveals the code.
#[derive(Debug)]
pub struct XalAuthorizationCode {
    code: SecretString,
}

/// Validates the one sensitive top-level navigation produced by XAL OAuth.
///
/// This type intentionally exposes no state value and has no `Debug` or `Display`
/// implementation.
pub struct XalRedirectValidator {
    expected_state: SecretString,
}

impl XalRedirectValidator {
    /// Returns a code for the exact desktop callback, or `None` for an intermediate navigation.
    pub fn capture_if_redirect(
        &self,
        value: &str,
    ) -> Result<Option<XalAuthorizationCode>, AuthError> {
        let url = Url::parse(value).map_err(|_| AuthError::InvalidXalCallback)?;
        if !is_desktop_redirect(&url) {
            return Ok(None);
        }
        parse_browser_result(value, self.expected_state.expose())
            .map(|code| Some(XalAuthorizationCode { code }))
    }
}

#[derive(Clone, Debug)]
pub struct XalAuthClient {
    client: Client,
}

impl XalAuthClient {
    pub fn new(options: AuthClientOptions) -> Result<Self, AuthError> {
        let client = Client::builder()
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .redirect(Policy::none())
            .user_agent("Cubic/0.1 (experimental XAL interoperability)")
            .build()
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::XboxDevice,
                source,
            })?;
        Ok(Self { client })
    }

    pub async fn begin_interactive(
        &self,
        device: &XalDeviceIdentity,
    ) -> Result<XalInteractiveAuthorization, AuthError> {
        let now = SystemTime::now();
        let device_request = device_request(device)?;
        let device_response: SignedResponse<XboxTokenResponse> = self
            .send_signed_json(
                AuthStage::XboxDevice,
                XAL_DEVICE_AUTH,
                "/device/authenticate",
                &device_request,
                device,
                now,
                true,
            )
            .await?;
        validate_token(&device_response.body, AuthStage::XboxDevice)?;

        let verifier = random_urlsafe(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()));
        let state = random_urlsafe(32);
        let request = sisu_authenticate_request(
            device_response.body.token.expose(),
            &challenge,
            state.expose(),
        );
        let response: SignedResponse<SisuAuthenticateResponse> = self
            .send_signed_json(
                AuthStage::SisuAuthenticate,
                XAL_SISU_AUTHENTICATE,
                "/authenticate",
                &request,
                device,
                device_response.date,
                true,
            )
            .await?;
        let session_id = required_header(
            &response.headers,
            "x-sessionid",
            AuthStage::SisuAuthenticate,
        )?;
        let authorization_url = Url::parse(&response.body.msa_oauth_redirect).map_err(|_| {
            AuthError::InvalidResponse {
                stage: AuthStage::SisuAuthenticate,
            }
        })?;
        validate_authorization_url(&authorization_url)?;
        Ok(XalInteractiveAuthorization {
            authorization_url,
            verifier,
            expected_state: state,
            session_id: SecretString::new(session_id),
            device_token: device_response.body.token,
        })
    }

    pub async fn complete_interactive(
        &self,
        device: &XalDeviceIdentity,
        authorization: XalInteractiveAuthorization,
        authorization_code: XalAuthorizationCode,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        let oauth = self
            .exchange_oauth_code(
                authorization_code.code.expose(),
                authorization.verifier.expose(),
            )
            .await?;
        self.finish_xal_chain(
            device,
            &authorization.device_token,
            Some(&authorization.session_id),
            oauth,
            None,
        )
        .await
    }

    pub async fn refresh(
        &self,
        device: &XalDeviceIdentity,
        previous_refresh_token: &SecretString,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        let oauth = self.refresh_oauth(previous_refresh_token).await?;
        let device_request = device_request(device)?;
        let device_response: SignedResponse<XboxTokenResponse> = self
            .send_signed_json(
                AuthStage::XboxDevice,
                XAL_DEVICE_AUTH,
                "/device/authenticate",
                &device_request,
                device,
                SystemTime::now(),
                true,
            )
            .await?;
        validate_token(&device_response.body, AuthStage::XboxDevice)?;
        self.finish_xal_chain(
            device,
            &device_response.body.token,
            None,
            oauth,
            Some(previous_refresh_token.clone()),
        )
        .await
    }

    async fn exchange_oauth_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<TimedOAuthToken, AuthError> {
        let fields = [
            ("client_id", XAL_INTEROP_CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", XAL_REDIRECT_URI),
            ("scope", XAL_SCOPE),
        ];
        self.send_oauth_form(&fields).await
    }

    async fn refresh_oauth(
        &self,
        refresh_token: &SecretString,
    ) -> Result<TimedOAuthToken, AuthError> {
        let fields = [
            ("client_id", XAL_INTEROP_CLIENT_ID),
            ("refresh_token", refresh_token.expose()),
            ("grant_type", "refresh_token"),
            ("redirect_uri", XAL_REDIRECT_URI),
            ("scope", XAL_SCOPE),
        ];
        self.send_oauth_form(&fields).await
    }

    async fn send_oauth_form(&self, fields: &[(&str, &str)]) -> Result<TimedOAuthToken, AuthError> {
        let response = self
            .client
            .post(XAL_OAUTH_TOKEN)
            .header("Accept", "application/json")
            .form(fields)
            .send()
            .await
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::MicrosoftOAuth,
                source,
            })?;
        let date = response_date(response.headers());
        let value = read_json_response(response, AuthStage::MicrosoftOAuth).await?;
        Ok(TimedOAuthToken { date, value })
    }

    async fn finish_xal_chain(
        &self,
        device: &XalDeviceIdentity,
        device_token: &SecretString,
        session_id: Option<&SecretString>,
        oauth: TimedOAuthToken,
        previous_refresh: Option<SecretString>,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        if oauth.value.access_token.is_empty() {
            return Err(AuthError::InvalidResponse {
                stage: AuthStage::MicrosoftOAuth,
            });
        }
        let refresh_token = select_refresh_token(oauth.value.refresh_token, previous_refresh)?;
        let sisu_request = sisu_authorize_request(
            oauth.value.access_token.expose(),
            device_token.expose(),
            session_id.map(SecretString::expose),
            device.proof_key()?,
        );
        let sisu: SignedResponse<SisuAuthorizeResponse> = self
            .send_signed_json(
                AuthStage::SisuAuthorize,
                XAL_SISU_AUTHORIZE,
                "/authorize",
                &sisu_request,
                device,
                oauth.date,
                false,
            )
            .await?;
        validate_token(&sisu.body.user_token, AuthStage::SisuAuthorize)?;
        validate_token(&sisu.body.title_token, AuthStage::SisuAuthorize)?;

        let xsts_request = xsts_request(
            sisu.body.user_token.token.expose(),
            device_token.expose(),
            sisu.body.title_token.token.expose(),
        );
        let xsts: SignedResponse<XboxTokenResponse> = self
            .send_signed_json(
                AuthStage::XboxXsts,
                XAL_XSTS_AUTHORIZE,
                "/xsts/authorize",
                &xsts_request,
                device,
                sisu.date,
                true,
            )
            .await?;
        let user_hash = xsts.body.user_hash(AuthStage::XboxXsts)?;
        let launcher_request = LauncherLoginRequest {
            platform: "PC_LAUNCHER",
            xtoken: format!("XBL3.0 x={user_hash};{}", xsts.body.token.expose()),
        };
        let minecraft: MinecraftTokenResponse = self
            .send_json(
                AuthStage::MinecraftLauncher,
                self.client
                    .post(MINECRAFT_LAUNCHER_LOGIN)
                    .json(&launcher_request),
            )
            .await?;
        if minecraft.access_token.is_empty() {
            return Err(AuthError::InvalidResponse {
                stage: AuthStage::MinecraftLauncher,
            });
        }
        let bearer = format!("Bearer {}", minecraft.access_token.expose());
        let entitlements: EntitlementsResponse = self
            .send_json(
                AuthStage::MinecraftEntitlements,
                self.client
                    .get(MINECRAFT_ENTITLEMENTS)
                    .header("Authorization", &bearer),
            )
            .await?;
        validate_entitlements(&entitlements)?;
        let raw_profile: RawMinecraftProfile = self
            .send_json(
                AuthStage::MinecraftProfile,
                self.client
                    .get(MINECRAFT_PROFILE)
                    .header("Authorization", bearer),
            )
            .await?;
        let profile = MinecraftProfile {
            id: MinecraftProfileId::from_str(&raw_profile.id)?,
            name: raw_profile.name,
        }
        .validate()?;
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs(minecraft.expires_in))
            .ok_or(AuthError::InvalidResponse {
                stage: AuthStage::MinecraftLauncher,
            })?;
        Ok(AuthenticatedMinecraftAccount {
            backend: AuthBackend::XalInterop,
            profile,
            minecraft_access_token: minecraft.access_token,
            expires_at,
            refresh_token,
        })
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        stage: AuthStage,
        request: reqwest::RequestBuilder,
    ) -> Result<T, AuthError> {
        let response = request
            .send()
            .await
            .map_err(|source| AuthError::Transport { stage, source })?;
        read_json_response(response, stage).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_signed_json<T: DeserializeOwned, B: Serialize>(
        &self,
        stage: AuthStage,
        endpoint: &str,
        path: &str,
        request: &B,
        device: &XalDeviceIdentity,
        timestamp: SystemTime,
        contract_header: bool,
    ) -> Result<SignedResponse<T>, AuthError> {
        let body = serde_json::to_vec(request).map_err(|_| AuthError::InvalidResponse { stage })?;
        let signature = sign_request(device, timestamp, "POST", path, None, &body)?;
        let mut builder = self
            .client
            .post(endpoint)
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Accept", "application/json")
            .header("Signature", signature)
            .body(body);
        if contract_header {
            builder = builder.header("x-xbl-contract-version", "1");
        }
        let response = builder
            .send()
            .await
            .map_err(|source| AuthError::Transport { stage, source })?;
        let date = response_date(response.headers());
        let headers = response.headers().clone();
        let body = read_json_response(response, stage).await?;
        Ok(SignedResponse {
            headers,
            date,
            body,
        })
    }
}

impl MinecraftSessionJoiner for XalAuthClient {
    async fn join_server(
        &self,
        account: &AuthenticatedMinecraftAccount,
        server_hash: &str,
    ) -> Result<(), AuthError> {
        crate::http::join_server_with(&self.client, account, server_hash).await
    }
}

struct SignedResponse<T> {
    headers: HeaderMap,
    date: SystemTime,
    body: T,
}

struct TimedOAuthToken {
    date: SystemTime,
    value: OAuthTokenResponse,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: SecretString,
    refresh_token: Option<SecretString>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxTokenResponse {
    token: SecretString,
    #[serde(default)]
    display_claims: Option<XboxDisplayClaims>,
}

impl XboxTokenResponse {
    fn user_hash(&self, stage: AuthStage) -> Result<&str, AuthError> {
        let value = self
            .display_claims
            .as_ref()
            .ok_or(AuthError::InvalidResponse { stage })?
            .xui
            .first()
            .map(|claim| claim.uhs.as_str())
            .ok_or(AuthError::InvalidResponse { stage })?;
        if value.is_empty()
            || value.len() > 64
            || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(AuthError::InvalidResponse { stage });
        }
        Ok(value)
    }
}

#[derive(Default, Deserialize)]
struct XboxDisplayClaims {
    #[serde(default)]
    xui: Vec<XboxUserClaim>,
}

#[derive(Deserialize)]
struct XboxUserClaim {
    uhs: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SisuAuthenticateResponse {
    msa_oauth_redirect: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SisuAuthorizeResponse {
    title_token: XboxTokenResponse,
    user_token: XboxTokenResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct DeviceRequest {
    properties: DeviceProperties,
    relying_party: &'static str,
    token_type: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct DeviceProperties {
    auth_method: &'static str,
    id: String,
    device_type: &'static str,
    version: &'static str,
    proof_key: XalProofKey,
}

fn device_request(device: &XalDeviceIdentity) -> Result<DeviceRequest, AuthError> {
    Ok(DeviceRequest {
        properties: DeviceProperties {
            auth_method: "ProofOfPossession",
            id: device.braced_upper_id(),
            device_type: "Win32",
            version: "10.16.0",
            proof_key: device.proof_key()?,
        },
        relying_party: "http://auth.xboxlive.com",
        token_type: "JWT",
    })
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SisuAuthenticateRequest<'a> {
    app_id: &'static str,
    device_token: &'a str,
    offers: [&'static str; 1],
    query: SisuQuery<'a>,
    redirect_uri: &'static str,
    sandbox: &'static str,
    token_type: &'static str,
    title_id: &'static str,
}

#[derive(Serialize)]
struct SisuQuery<'a> {
    code_challenge: &'a str,
    code_challenge_method: &'static str,
    state: &'a str,
    prompt: &'static str,
}

fn sisu_authenticate_request<'a>(
    device_token: &'a str,
    challenge: &'a str,
    state: &'a str,
) -> SisuAuthenticateRequest<'a> {
    SisuAuthenticateRequest {
        app_id: XAL_INTEROP_CLIENT_ID,
        device_token,
        offers: [XAL_SCOPE],
        query: SisuQuery {
            code_challenge: challenge,
            code_challenge_method: "S256",
            state,
            prompt: "select_account",
        },
        redirect_uri: XAL_REDIRECT_URI,
        sandbox: "RETAIL",
        token_type: "code",
        title_id: XAL_INTEROP_TITLE_ID,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SisuAuthorizeRequest<'a> {
    access_token: String,
    app_id: &'static str,
    device_token: &'a str,
    proof_key: XalProofKey,
    sandbox: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    site_name: &'static str,
    relying_party: &'static str,
    use_modern_gamertag: bool,
}

fn sisu_authorize_request<'a>(
    access_token: &str,
    device_token: &'a str,
    session_id: Option<&'a str>,
    proof_key: XalProofKey,
) -> SisuAuthorizeRequest<'a> {
    SisuAuthorizeRequest {
        access_token: format!("t={access_token}"),
        app_id: XAL_INTEROP_CLIENT_ID,
        device_token,
        proof_key,
        sandbox: "RETAIL",
        session_id,
        site_name: "user.auth.xboxlive.com",
        relying_party: "http://xboxlive.com",
        use_modern_gamertag: true,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct XstsRequest<'a> {
    relying_party: &'static str,
    token_type: &'static str,
    properties: XstsProperties<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct XstsProperties<'a> {
    sandbox_id: &'static str,
    user_tokens: [&'a str; 1],
    device_token: &'a str,
    title_token: &'a str,
}

fn xsts_request<'a>(
    user_token: &'a str,
    device_token: &'a str,
    title_token: &'a str,
) -> XstsRequest<'a> {
    XstsRequest {
        relying_party: "rp://api.minecraftservices.com/",
        token_type: "JWT",
        properties: XstsProperties {
            sandbox_id: "RETAIL",
            user_tokens: [user_token],
            device_token,
            title_token,
        },
    }
}

#[derive(Serialize)]
struct LauncherLoginRequest {
    platform: &'static str,
    xtoken: String,
}

#[derive(Deserialize)]
struct MinecraftTokenResponse {
    access_token: SecretString,
    expires_in: u64,
}

#[derive(Deserialize)]
struct EntitlementsResponse {
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawMinecraftProfile {
    id: String,
    name: String,
}

fn validate_token(token: &XboxTokenResponse, stage: AuthStage) -> Result<(), AuthError> {
    if token.token.is_empty() {
        return Err(AuthError::InvalidResponse { stage });
    }
    Ok(())
}

fn validate_entitlements(entitlements: &EntitlementsResponse) -> Result<(), AuthError> {
    if entitlements.items.is_empty() {
        return Err(AuthError::NoJavaEntitlement);
    }
    Ok(())
}

fn select_refresh_token(
    rotated: Option<SecretString>,
    previous: Option<SecretString>,
) -> Result<SecretString, AuthError> {
    rotated
        .or(previous)
        .filter(|value| !value.is_empty())
        .ok_or(AuthError::InvalidResponse {
            stage: AuthStage::MicrosoftOAuth,
        })
}

fn required_header(
    headers: &HeaderMap,
    name: &'static str,
    stage: AuthStage,
) -> Result<String, AuthError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 1024)
        .ok_or(AuthError::MissingHeader { stage, name })?;
    Ok(value.to_owned())
}

fn response_date(headers: &HeaderMap) -> SystemTime {
    headers
        .get(reqwest::header::DATE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .unwrap_or_else(SystemTime::now)
}

async fn read_json_response<T: DeserializeOwned>(
    mut response: reqwest::Response,
    stage: AuthStage,
) -> Result<T, AuthError> {
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| AuthError::Transport { stage, source })?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(AuthError::ResponseTooLarge {
                stage,
                limit: MAX_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        if stage == AuthStage::XboxXsts
            && let Ok(error) = serde_json::from_slice::<XboxErrorResponse>(&body)
        {
            return Err(map_xbox_error(error.xerr));
        }
        return Err(AuthError::Http {
            stage,
            status: status.as_u16(),
            message: bounded_service_message(&body),
        });
    }
    serde_json::from_slice(&body).map_err(|_| AuthError::InvalidResponse { stage })
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxErrorResponse {
    xerr: u64,
}

fn map_xbox_error(code: u64) -> AuthError {
    let guidance = match code {
        0x8015_DC09 | 0x8015_DC13 => "create or repair the Xbox profile for this Microsoft account",
        0x8015_DC0B => "Xbox services are unavailable for this account's region",
        0x8015_DC0E => "a child account must be added to a Microsoft family by an adult",
        0x8015_DC1F | 0x8015_DC22 => {
            "the supplied Xbox token expired and authentication must be refreshed"
        }
        0x8015_DC26 | 0x8015_DC27 => "the supplied Xbox token is invalid",
        _ => "review the Xbox account and retry authentication",
    };
    AuthError::XboxAccount { code, guidance }
}

fn bounded_service_message(body: &[u8]) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let safe = parsed.as_ref().and_then(|value| {
        ["error", "error_description", "message", "Message"]
            .into_iter()
            .find_map(|field| value.get(field).and_then(serde_json::Value::as_str))
    });
    let Some(safe) = safe else {
        return "service returned no safe structured error detail".to_owned();
    };
    let value: String = safe
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect();
    if value.is_empty() {
        "service returned no safe structured error detail".to_owned()
    } else {
        value
    }
}

fn parse_browser_result(value: &str, expected_state: &str) -> Result<SecretString, AuthError> {
    let value = value.trim();
    if value.len() > MAX_BROWSER_RESULT_BYTES {
        return Err(AuthError::InvalidXalCallback);
    }
    let url = Url::parse(value).map_err(|_| AuthError::InvalidXalCallback)?;
    if !is_desktop_redirect(&url) {
        return Err(AuthError::InvalidXalCallback);
    }
    let mut code: Option<SecretString> = None;
    let mut state: Option<SecretString> = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(SecretString::new(value.into_owned())),
            "state" if state.is_none() => state = Some(SecretString::new(value.into_owned())),
            "error" if error.is_none() => error = Some(value.into_owned()),
            "code" | "state" | "error" => return Err(AuthError::InvalidXalCallback),
            _ => {}
        }
    }
    if state.as_ref().map(SecretString::expose) != Some(expected_state) {
        return Err(AuthError::StateMismatch);
    }
    if let Some(error) = error {
        if code.is_some() {
            return Err(AuthError::InvalidXalCallback);
        }
        let code: String = error
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .take(64)
            .collect();
        return Err(AuthError::OAuthRejected {
            code: if code.is_empty() {
                "unknown_error".to_owned()
            } else {
                code
            },
        });
    }
    code.filter(|value| !value.is_empty())
        .ok_or(AuthError::InvalidXalCallback)
}

fn is_desktop_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("login.live.com")
        && url.path() == "/oauth20_desktop.srf"
}

fn validate_authorization_url(url: &Url) -> Result<(), AuthError> {
    if url.scheme() == "https"
        && url.host_str() == Some("login.live.com")
        && url.path() == "/oauth20_authorize.srf"
    {
        Ok(())
    } else {
        Err(AuthError::InvalidResponse {
            stage: AuthStage::SisuAuthenticate,
        })
    }
}

fn sign_request(
    device: &XalDeviceIdentity,
    timestamp: SystemTime,
    method: &str,
    path_and_query: &str,
    authorization: Option<&str>,
    body: &[u8],
) -> Result<String, AuthError> {
    let filetime = windows_filetime(timestamp)?;
    let input = signing_input(
        filetime,
        method,
        path_and_query,
        authorization.unwrap_or_default(),
        body,
    );
    let signature: Signature = device
        .key
        .try_sign(&input)
        .map_err(|_| AuthError::XalSigning)?;
    let mut output = Vec::with_capacity(76);
    output.extend_from_slice(&1_u32.to_be_bytes());
    output.extend_from_slice(&filetime.to_be_bytes());
    output.extend_from_slice(&signature.to_bytes());
    Ok(STANDARD.encode(output))
}

fn windows_filetime(timestamp: SystemTime) -> Result<u64, AuthError> {
    let unix = timestamp
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::InvalidXalTimestamp)?;
    unix.as_secs()
        .checked_add(WINDOWS_EPOCH_OFFSET_SECONDS)
        .and_then(|seconds| seconds.checked_mul(FILETIME_TICKS_PER_SECOND))
        .and_then(|ticks| ticks.checked_add(u64::from(unix.subsec_nanos() / 100)))
        .ok_or(AuthError::InvalidXalTimestamp)
}

fn signing_input(
    filetime: u64,
    method: &str,
    path_and_query: &str,
    authorization: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut input = Vec::with_capacity(
        4 + 1
            + 8
            + 1
            + method.len()
            + 1
            + path_and_query.len()
            + 1
            + authorization.len()
            + 1
            + body.len()
            + 1,
    );
    input.extend_from_slice(&1_u32.to_be_bytes());
    input.push(0);
    input.extend_from_slice(&filetime.to_be_bytes());
    input.push(0);
    input.extend_from_slice(method.as_bytes());
    input.push(0);
    input.extend_from_slice(path_and_query.as_bytes());
    input.push(0);
    input.extend_from_slice(authorization.as_bytes());
    input.push(0);
    input.extend_from_slice(body);
    input.push(0);
    input
}

fn random_urlsafe(bytes: usize) -> SecretString {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    SecretString::new(URL_SAFE_NO_PAD.encode(value))
}

fn parse_uuid(value: &str) -> Result<[u8; 16], AuthError> {
    let compact: String = value
        .chars()
        .filter(|character| *character != '-')
        .collect();
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AuthError::InvalidXalDevice);
    }
    let mut bytes = [0_u8; 16];
    for (index, output) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        let pair = compact
            .get(start..start + 2)
            .ok_or(AuthError::InvalidXalDevice)?;
        *output = u8::from_str_radix(pair, 16).map_err(|_| AuthError::InvalidXalDevice)?;
    }
    Ok(bytes)
}

fn format_uuid(bytes: [u8; 16], uppercase: bool) -> String {
    let mut result = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            result.push('-');
        }
        if uppercase {
            result.push_str(&format!("{byte:02X}"));
        } else {
            result.push_str(&format!("{byte:02x}"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
    use serde_json::json;
    use url::Url;

    use super::{
        AuthStage, EntitlementsResponse, LauncherLoginRequest, XalAuthorizationCode,
        XalDeviceIdentity, device_request, parse_browser_result, required_header,
        select_refresh_token, sign_request, signing_input, sisu_authenticate_request,
        sisu_authorize_request, validate_authorization_url, validate_entitlements,
        windows_filetime, xsts_request,
    };
    use crate::{AuthError, SecretString};

    #[test]
    fn generated_device_key_round_trips_and_builds_jwk() {
        let first = XalDeviceIdentity::generate();
        let credential = first.to_credential().unwrap();
        assert!(!format!("{credential:?}").contains(credential.private_key.expose()));
        let restored = XalDeviceIdentity::from_credential(&credential).unwrap();
        assert_eq!(first.proof_key().unwrap(), restored.proof_key().unwrap());
        assert_eq!(first.id, restored.id);
    }

    #[test]
    fn device_request_has_exact_interop_shape() {
        let device = XalDeviceIdentity::generate();
        let value = serde_json::to_value(device_request(&device).unwrap()).unwrap();
        assert_eq!(value["Properties"]["AuthMethod"], "ProofOfPossession");
        assert_eq!(value["Properties"]["DeviceType"], "Win32");
        assert_eq!(value["Properties"]["ProofKey"]["kty"], "EC");
        assert_eq!(value["Properties"]["ProofKey"]["crv"], "P-256");
        assert_eq!(value["RelyingParty"], "http://auth.xboxlive.com");
    }

    #[test]
    fn sisu_authenticate_request_has_exact_interop_shape() {
        assert_eq!(
            serde_json::to_value(sisu_authenticate_request("device", "challenge", "state"))
                .unwrap(),
            json!({
                "AppId": "00000000402b5328",
                "DeviceToken": "device",
                "Offers": ["service::user.auth.xboxlive.com::MBI_SSL"],
                "Query": {
                    "code_challenge": "challenge",
                    "code_challenge_method": "S256",
                    "state": "state",
                    "prompt": "select_account"
                },
                "RedirectUri": "https://login.live.com/oauth20_desktop.srf",
                "Sandbox": "RETAIL",
                "TokenType": "code",
                "TitleId": "1794566092"
            })
        );
    }

    #[test]
    fn sisu_authorize_request_has_exact_interop_shape() {
        let device = XalDeviceIdentity::generate();
        let proof = device.proof_key().unwrap();
        let expected_proof = serde_json::to_value(&proof).unwrap();
        let value = serde_json::to_value(sisu_authorize_request(
            "oauth",
            "device",
            Some("session"),
            proof,
        ))
        .unwrap();
        assert_eq!(value["AccessToken"], "t=oauth");
        assert_eq!(value["AppId"], "00000000402b5328");
        assert_eq!(value["DeviceToken"], "device");
        assert_eq!(value["ProofKey"], expected_proof);
        assert_eq!(value["Sandbox"], "RETAIL");
        assert_eq!(value["SessionId"], "session");
        assert_eq!(value["SiteName"], "user.auth.xboxlive.com");
        assert_eq!(value["RelyingParty"], "http://xboxlive.com");
        assert_eq!(value["UseModernGamertag"], true);
    }

    #[test]
    fn launcher_login_request_has_exact_interop_shape() {
        assert_eq!(
            serde_json::to_value(LauncherLoginRequest {
                platform: "PC_LAUNCHER",
                xtoken: "XBL3.0 x=hash;token".to_owned(),
            })
            .unwrap(),
            json!({
                "platform": "PC_LAUNCHER",
                "xtoken": "XBL3.0 x=hash;token"
            })
        );
    }

    #[test]
    fn sisu_browser_url_is_restricted_to_microsoft_authorization_endpoint() {
        assert!(
            validate_authorization_url(
                &Url::parse("https://login.live.com/oauth20_authorize.srf?state=test").unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_authorization_url(
                &Url::parse("https://example.com/oauth20_authorize.srf?state=test").unwrap()
            )
            .is_err()
        );
        assert!(
            validate_authorization_url(
                &Url::parse("http://login.live.com/oauth20_authorize.srf?state=test").unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn signing_input_is_canonical_and_signature_is_raw_es256() {
        let device = XalDeviceIdentity::generate();
        let timestamp = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let filetime = windows_filetime(timestamp).unwrap();
        let expected = [
            1_u32.to_be_bytes().as_slice(),
            &[0],
            filetime.to_be_bytes().as_slice(),
            &[0],
            b"POST",
            &[0],
            b"/device/authenticate",
            &[0, 0],
            br#"{"hello":"world"}"#,
            &[0],
        ]
        .concat();
        assert_eq!(
            signing_input(
                filetime,
                "POST",
                "/device/authenticate",
                "",
                br#"{"hello":"world"}"#,
            ),
            expected
        );
        let encoded = sign_request(
            &device,
            timestamp,
            "POST",
            "/device/authenticate",
            None,
            br#"{"hello":"world"}"#,
        )
        .unwrap();
        let decoded = STANDARD.decode(encoded).unwrap();
        assert_eq!(decoded.len(), 76);
        assert_eq!(&decoded[..4], &1_u32.to_be_bytes());
        assert_eq!(&decoded[4..12], &filetime.to_be_bytes());
        let signature = Signature::from_slice(&decoded[12..]).unwrap();
        VerifyingKey::from(&device.key)
            .verify(&expected, &signature)
            .unwrap();
    }

    #[test]
    fn browser_result_requires_desktop_redirect_and_matching_state() {
        let code = parse_browser_result(
            "https://login.live.com/oauth20_desktop.srf?code=fake-code&state=expected",
            "expected",
        )
        .unwrap();
        assert_eq!(code.expose(), "fake-code");
        assert!(matches!(
            parse_browser_result(
                "https://login.live.com/oauth20_desktop.srf?code=x&state=wrong",
                "expected"
            ),
            Err(AuthError::StateMismatch)
        ));
        assert!(
            parse_browser_result("https://example.com/?code=x&state=expected", "expected").is_err()
        );
    }

    #[test]
    fn desktop_redirect_parser_rejects_missing_duplicate_and_misleading_inputs() {
        let invalid = [
            "https://login.live.com/oauth20_desktop.srf?code=x",
            "https://login.live.com/oauth20_desktop.srf?state=expected",
            "https://login.live.com/oauth20_desktop.srf?removed=true",
            "http://login.live.com/oauth20_desktop.srf?code=x&state=expected",
            "https://example.com/oauth20_desktop.srf?code=x&state=expected",
            "https://login.live.com.attacker.example/oauth20_desktop.srf?code=x&state=expected",
            "https://attacker.login.live.com/oauth20_desktop.srf?code=x&state=expected",
            "https://login.live.com/wrong?code=x&state=expected",
            "https://login.live.com/oauth20_desktop.srf?code=x&code=y&state=expected",
            "https://login.live.com/oauth20_desktop.srf?code=x&state=expected&state=again",
            "not a URL",
        ];
        for value in invalid {
            assert!(
                parse_browser_result(value, "expected").is_err(),
                "unexpectedly accepted a malformed redirect"
            );
        }
    }

    #[test]
    fn redirect_validator_ignores_intermediate_navigation_and_captures_callback() {
        let validator = super::XalRedirectValidator {
            expected_state: SecretString::new("expected"),
        };
        assert!(
            validator
                .capture_if_redirect("https://login.live.com/oauth20_authorize.srf?state=expected")
                .unwrap()
                .is_none()
        );
        let code = validator
            .capture_if_redirect(
                "https://login.live.com/oauth20_desktop.srf?code=fake-code&state=expected",
            )
            .unwrap()
            .unwrap();
        assert!(!format!("{code:?}").contains("fake-code"));
    }

    #[test]
    fn browser_oauth_error_is_structured_without_exposing_codes_in_debug() {
        let error = parse_browser_result(
            "https://login.live.com/oauth20_desktop.srf?error=access_denied&state=expected",
            "expected",
        )
        .unwrap_err();
        assert!(matches!(error, AuthError::OAuthRejected { .. }));
        let secret = SecretString::new("fake-code");
        assert!(!format!("{secret:?}").contains("fake-code"));
        let code = XalAuthorizationCode { code: secret };
        assert!(!format!("{code:?}").contains("fake-code"));
    }

    #[test]
    fn xsts_request_includes_user_device_and_title_tokens() {
        assert_eq!(
            serde_json::to_value(xsts_request("user", "device", "title")).unwrap(),
            json!({
                "RelyingParty": "rp://api.minecraftservices.com/",
                "TokenType": "JWT",
                "Properties": {
                    "SandboxId": "RETAIL",
                    "UserTokens": ["user"],
                    "DeviceToken": "device",
                    "TitleToken": "title"
                }
            })
        );
    }

    #[test]
    fn malformed_xbox_token_and_profile_inputs_are_rejected() {
        let token: super::XboxTokenResponse = serde_json::from_value(json!({
            "Token": "fake",
            "DisplayClaims": { "xui": [] }
        }))
        .unwrap();
        assert!(token.user_hash(AuthStage::XboxXsts).is_err());
        assert!(
            serde_json::from_value::<super::MinecraftTokenResponse>(json!({
                "expires_in": 60
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<super::SisuAuthenticateResponse>(json!({})).is_err());
        assert!(serde_json::from_value::<super::SisuAuthorizeResponse>(json!({})).is_err());
        assert!(
            serde_json::from_value::<super::MinecraftTokenResponse>(json!({
                "access_token": "fake"
            }))
            .is_err()
        );
    }

    #[test]
    fn missing_sisu_session_header_is_structured() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(matches!(
            required_header(&headers, "x-sessionid", AuthStage::SisuAuthenticate),
            Err(AuthError::MissingHeader {
                stage: AuthStage::SisuAuthenticate,
                name: "x-sessionid"
            })
        ));
    }

    #[test]
    fn entitlement_failure_is_explicit() {
        assert!(matches!(
            validate_entitlements(&EntitlementsResponse { items: Vec::new() }),
            Err(AuthError::NoJavaEntitlement)
        ));
        assert!(
            validate_entitlements(&EntitlementsResponse {
                items: vec![json!({ "name": "synthetic" })]
            })
            .is_ok()
        );
    }

    #[test]
    fn refresh_token_rotation_prefers_new_and_preserves_old_when_omitted() {
        let rotated = select_refresh_token(
            Some(SecretString::new("fake-rotated")),
            Some(SecretString::new("fake-old")),
        )
        .unwrap();
        assert_eq!(rotated.expose(), "fake-rotated");
        let retained = select_refresh_token(None, Some(SecretString::new("fake-old"))).unwrap();
        assert_eq!(retained.expose(), "fake-old");
        assert!(select_refresh_token(None, None).is_err());
    }
}
