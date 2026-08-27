use std::{
    str::FromStr,
    time::{Duration, SystemTime},
};

use reqwest::{Client, StatusCode, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    AuthBackend, AuthError, AuthStage, AuthenticatedMinecraftAccount, MicrosoftClientId,
    MinecraftProfile, MinecraftProfileId, MinecraftSessionJoiner, OAuthAuthorizationCode,
    SecretString,
};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MICROSOFT_TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBOX_USER_AUTH: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XBOX_XSTS_AUTH: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MINECRAFT_LOGIN: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MINECRAFT_ENTITLEMENTS: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const MINECRAFT_PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";
const SESSION_JOIN: &str = "https://sessionserver.mojang.com/session/minecraft/join";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthClientOptions {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for AuthClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AuthClient {
    client_id: MicrosoftClientId,
    client: Client,
}

impl AuthClient {
    pub fn new(
        client_id: MicrosoftClientId,
        options: AuthClientOptions,
    ) -> Result<Self, AuthError> {
        let client = Client::builder()
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .redirect(Policy::none())
            .user_agent("Cubic/0.1 (native Minecraft client)")
            .build()
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::MicrosoftOAuth,
                source,
            })?;
        Ok(Self { client_id, client })
    }

    pub async fn authenticate_code(
        &self,
        authorization: OAuthAuthorizationCode,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        let microsoft = self.exchange_code(authorization).await?;
        self.minecraft_chain(microsoft, None).await
    }

    pub async fn refresh(
        &self,
        refresh_token: &SecretString,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        let fields = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
            ("scope", crate::oauth::OAUTH_SCOPE),
        ];
        let microsoft: MicrosoftTokenResponse = self
            .send_json(
                AuthStage::MicrosoftOAuth,
                self.client.post(MICROSOFT_TOKEN).form(&fields),
            )
            .await?;
        self.minecraft_chain(microsoft, Some(refresh_token.clone()))
            .await
    }

    pub async fn join_server(
        &self,
        account: &AuthenticatedMinecraftAccount,
        server_hash: &str,
    ) -> Result<(), AuthError> {
        join_server_with(&self.client, account, server_hash).await
    }

    async fn exchange_code(
        &self,
        authorization: OAuthAuthorizationCode,
    ) -> Result<MicrosoftTokenResponse, AuthError> {
        let fields = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", authorization.code.expose()),
            ("redirect_uri", authorization.redirect_uri.as_str()),
            ("code_verifier", authorization.verifier.expose()),
            ("scope", crate::oauth::OAUTH_SCOPE),
        ];
        self.send_json(
            AuthStage::MicrosoftOAuth,
            self.client.post(MICROSOFT_TOKEN).form(&fields),
        )
        .await
    }

    async fn minecraft_chain(
        &self,
        microsoft: MicrosoftTokenResponse,
        previous_refresh_token: Option<SecretString>,
    ) -> Result<AuthenticatedMinecraftAccount, AuthError> {
        if microsoft.access_token.is_empty() {
            return Err(AuthError::InvalidResponse {
                stage: AuthStage::MicrosoftOAuth,
            });
        }
        let refresh_token = microsoft
            .refresh_token
            .or(previous_refresh_token)
            .filter(|token| !token.is_empty())
            .ok_or(AuthError::InvalidResponse {
                stage: AuthStage::MicrosoftOAuth,
            })?;
        let xbox_request = xbox_user_request(microsoft.access_token.expose());
        let xbox: XboxTokenResponse = self
            .send_json(
                AuthStage::XboxUserToken,
                self.client
                    .post(XBOX_USER_AUTH)
                    .header("x-xbl-contract-version", "1")
                    .json(&xbox_request),
            )
            .await?;
        let xsts_request = xsts_request(xbox.token.expose());
        let xsts: XboxTokenResponse = self
            .send_json(
                AuthStage::XboxXsts,
                self.client
                    .post(XBOX_XSTS_AUTH)
                    .header("x-xbl-contract-version", "1")
                    .json(&xsts_request),
            )
            .await?;
        let user_hash = xsts.user_hash()?;
        let login = MinecraftLoginRequest {
            identity_token: format!("XBL3.0 x={user_hash};{}", xsts.token.expose()),
        };
        let minecraft: MinecraftTokenResponse = self
            .send_json(
                AuthStage::MinecraftToken,
                self.client.post(MINECRAFT_LOGIN).json(&login),
            )
            .await?;
        let bearer = format!("Bearer {}", minecraft.access_token.expose());
        let entitlements: EntitlementsResponse = self
            .send_json(
                AuthStage::MinecraftEntitlements,
                self.client
                    .get(MINECRAFT_ENTITLEMENTS)
                    .header("Authorization", &bearer),
            )
            .await?;
        if entitlements.items.is_empty() {
            return Err(AuthError::NoJavaEntitlement);
        }
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
                stage: AuthStage::MinecraftToken,
            })?;
        Ok(AuthenticatedMinecraftAccount {
            backend: AuthBackend::CubicEntra,
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
        let mut response = request
            .send()
            .await
            .map_err(|source| AuthError::Transport { stage, source })?;
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
            let message = bounded_service_message(&body);
            if is_invalid_app_registration(stage, status, &message) {
                return Err(AuthError::InvalidAppRegistration);
            }
            if stage == AuthStage::XboxXsts
                && let Ok(error) = serde_json::from_slice::<XboxErrorResponse>(&body)
            {
                return Err(map_xbox_error(error.xerr));
            }
            return Err(AuthError::Http {
                stage,
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_slice(&body).map_err(|_| AuthError::InvalidResponse { stage })
    }
}

impl MinecraftSessionJoiner for AuthClient {
    async fn join_server(
        &self,
        account: &AuthenticatedMinecraftAccount,
        server_hash: &str,
    ) -> Result<(), AuthError> {
        AuthClient::join_server(self, account, server_hash).await
    }
}

pub(crate) async fn join_server_with(
    client: &Client,
    account: &AuthenticatedMinecraftAccount,
    server_hash: &str,
) -> Result<(), AuthError> {
    let request = SessionJoinRequest {
        access_token: account.minecraft_access_token.expose(),
        selected_profile: account.profile.id.compact(),
        server_id: server_hash,
    };
    let mut response = client
        .post(SESSION_JOIN)
        .json(&request)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: AuthStage::MinecraftSessionJoin,
            source,
        })?;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| AuthError::Transport {
            stage: AuthStage::MinecraftSessionJoin,
            source,
        })?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(AuthError::ResponseTooLarge {
                stage: AuthStage::MinecraftSessionJoin,
                limit: MAX_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Err(AuthError::Http {
        stage: AuthStage::MinecraftSessionJoin,
        status: status.as_u16(),
        message: bounded_service_message(&body),
    })
}

fn is_invalid_app_registration(stage: AuthStage, status: StatusCode, message: &str) -> bool {
    stage == AuthStage::MinecraftToken
        && status == StatusCode::FORBIDDEN
        && message
            .to_ascii_lowercase()
            .contains("invalid app registration")
}

fn bounded_service_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut value: String = text
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect();
    if value.is_empty() {
        value.push_str("service returned no safe error detail");
    }
    value
}

fn map_xbox_error(code: u64) -> AuthError {
    let guidance = match code {
        2_148_916_233 => "create an Xbox profile for this Microsoft account",
        2_148_916_235 => "Xbox services are unavailable for this account's region",
        2_148_916_238 => "a child account must be added to a Microsoft family by an adult",
        _ => "review the Xbox account and try interactive sign-in again",
    };
    AuthError::XboxAccount { code, guidance }
}

#[derive(Debug, Deserialize)]
struct MicrosoftTokenResponse {
    access_token: SecretString,
    refresh_token: Option<SecretString>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxTokenResponse {
    token: SecretString,
    display_claims: XboxDisplayClaims,
}

impl XboxTokenResponse {
    fn user_hash(&self) -> Result<&str, AuthError> {
        let value = self
            .display_claims
            .xui
            .first()
            .map(|claim| claim.uhs.as_str())
            .ok_or(AuthError::InvalidResponse {
                stage: AuthStage::XboxXsts,
            })?;
        if value.is_empty()
            || value.len() > 64
            || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(AuthError::InvalidResponse {
                stage: AuthStage::XboxXsts,
            });
        }
        Ok(value)
    }
}

#[derive(Debug, Deserialize)]
struct XboxDisplayClaims {
    xui: Vec<XboxUserClaim>,
}

#[derive(Debug, Deserialize)]
struct XboxUserClaim {
    uhs: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxErrorResponse {
    xerr: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct XboxRequest {
    properties: XboxProperties,
    relying_party: &'static str,
    token_type: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct XboxProperties {
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_method: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    site_name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rps_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_tokens: Option<Vec<String>>,
}

fn xbox_user_request(access_token: &str) -> XboxRequest {
    XboxRequest {
        properties: XboxProperties {
            auth_method: Some("RPS"),
            site_name: Some("user.auth.xboxlive.com"),
            rps_ticket: Some(format!("d={access_token}")),
            sandbox_id: None,
            user_tokens: None,
        },
        relying_party: "http://auth.xboxlive.com",
        token_type: "JWT",
    }
}

fn xsts_request(user_token: &str) -> XboxRequest {
    XboxRequest {
        properties: XboxProperties {
            auth_method: None,
            site_name: None,
            rps_ticket: None,
            sandbox_id: Some("RETAIL"),
            user_tokens: Some(vec![user_token.to_owned()]),
        },
        relying_party: "rp://api.minecraftservices.com/",
        token_type: "JWT",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MinecraftLoginRequest {
    identity_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionJoinRequest<'a> {
    access_token: &'a str,
    selected_profile: String,
    server_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct MinecraftTokenResponse {
    access_token: SecretString,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct EntitlementsResponse {
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawMinecraftProfile {
    id: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use reqwest::StatusCode;

    use super::{is_invalid_app_registration, map_xbox_error, xbox_user_request, xsts_request};
    use crate::{AuthError, AuthStage};

    #[test]
    fn xbox_user_request_is_an_independent_exact_vector() {
        assert_eq!(
            serde_json::to_value(xbox_user_request("fake-msa-token")).unwrap(),
            json!({
                "Properties": { "AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": "d=fake-msa-token" },
                "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT"
            })
        );
    }

    #[test]
    fn xsts_request_is_an_independent_exact_vector() {
        assert_eq!(
            serde_json::to_value(xsts_request("fake-xbox-token")).unwrap(),
            json!({
                "Properties": { "SandboxId": "RETAIL", "UserTokens": ["fake-xbox-token"] },
                "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT"
            })
        );
    }

    #[test]
    fn known_xsts_account_errors_have_actionable_categories() {
        assert!(matches!(
            map_xbox_error(2_148_916_233),
            AuthError::XboxAccount {
                code: 2_148_916_233,
                ..
            }
        ));
    }

    #[test]
    fn minecraft_invalid_app_registration_is_a_distinct_blocker() {
        assert!(is_invalid_app_registration(
            AuthStage::MinecraftToken,
            StatusCode::FORBIDDEN,
            "Invalid app registration, see https://aka.ms/AppRegInfo"
        ));
        assert!(!is_invalid_app_registration(
            AuthStage::XboxXsts,
            StatusCode::FORBIDDEN,
            "Invalid app registration"
        ));
    }
}
