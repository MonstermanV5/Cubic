use std::{fmt, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use url::Url;

use crate::{AuthError, MicrosoftClientId, SecretString};

const CALLBACK_PATH: &str = "/cubic/oauth/callback";
const CALLBACK_REQUEST_LIMIT: usize = 8 * 1024;
const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const OAUTH_SCOPE: &str = "XboxLive.signin offline_access";

#[derive(Debug)]
pub struct OAuthAuthorizationCode {
    pub(crate) code: SecretString,
    pub(crate) redirect_uri: String,
    pub(crate) verifier: SecretString,
}

#[derive(Debug)]
pub struct LoopbackAuthorization {
    listener: TcpListener,
    expected_state: SecretString,
    verifier: SecretString,
    redirect_uri: String,
    authorization_url: Url,
    callback_timeout: Duration,
}

#[derive(Clone, Eq, PartialEq)]
pub enum OAuthCallback {
    Code { code: String, state: String },
    Error { code: String, state: Option<String> },
}

impl fmt::Debug for OAuthCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code { .. } => {
                formatter.write_str("Code { code: [REDACTED], state: [REDACTED] }")
            }
            Self::Error { code, .. } => formatter
                .debug_struct("Error")
                .field("code", code)
                .field("state", &"[REDACTED]")
                .finish(),
        }
    }
}

impl LoopbackAuthorization {
    pub async fn begin(client_id: &MicrosoftClientId) -> Result<Self, AuthError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(AuthError::CallbackBind)?;
        let port = listener
            .local_addr()
            .map_err(AuthError::CallbackBind)?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let verifier = random_urlsafe(32);
        let state = random_urlsafe(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url =
            Url::parse("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize")
                .map_err(|_| AuthError::MalformedCallback)?;
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", client_id.as_str())
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", OAUTH_SCOPE)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        Ok(Self {
            listener,
            expected_state: SecretString::new(state),
            verifier: SecretString::new(verifier),
            redirect_uri,
            authorization_url,
            callback_timeout: DEFAULT_CALLBACK_TIMEOUT,
        })
    }

    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    pub async fn wait(self) -> Result<OAuthAuthorizationCode, AuthError> {
        let callback_timeout = self.callback_timeout;
        match timeout(callback_timeout, self.wait_inner()).await {
            Ok(result) => result,
            Err(_) => Err(AuthError::CallbackTimeout {
                timeout: callback_timeout,
            }),
        }
    }

    async fn wait_inner(self) -> Result<OAuthAuthorizationCode, AuthError> {
        let (mut stream, peer) = self
            .listener
            .accept()
            .await
            .map_err(AuthError::CallbackBind)?;
        if !peer.ip().is_loopback() {
            return Err(AuthError::MalformedCallback);
        }
        let mut request = vec![0_u8; CALLBACK_REQUEST_LIMIT];
        let read = stream
            .read(&mut request)
            .await
            .map_err(AuthError::CallbackBind)?;
        let request = std::str::from_utf8(request.get(..read).ok_or(AuthError::MalformedCallback)?)
            .map_err(|_| AuthError::MalformedCallback)?;
        let target = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.split_once(' ').map(|(target, _)| target))
            .ok_or(AuthError::MalformedCallback)?;
        let callback = parse_callback(target)?;
        let body = "Cubic authentication received. You may close this tab.";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _result = stream.write_all(response.as_bytes()).await;
        match callback {
            OAuthCallback::Code { code, state } => {
                if state != self.expected_state.expose() {
                    return Err(AuthError::StateMismatch);
                }
                Ok(OAuthAuthorizationCode {
                    code: SecretString::new(code),
                    redirect_uri: self.redirect_uri,
                    verifier: self.verifier,
                })
            }
            OAuthCallback::Error { code, state } => {
                if state.as_deref() != Some(self.expected_state.expose()) {
                    return Err(AuthError::StateMismatch);
                }
                Err(AuthError::OAuthRejected { code })
            }
        }
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

pub(crate) fn parse_callback(target: &str) -> Result<OAuthCallback, AuthError> {
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| AuthError::MalformedCallback)?;
    if url.path() != CALLBACK_PATH {
        return Err(AuthError::MalformedCallback);
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Ok(OAuthCallback::Error { code: error, state });
    }
    Ok(OAuthCallback::Code {
        code: code.ok_or(AuthError::MalformedCallback)?,
        state: state.ok_or(AuthError::MalformedCallback)?,
    })
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    use super::{OAuthCallback, parse_callback, random_urlsafe};

    #[test]
    fn s256_rfc7636_vector_matches() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn random_verifier_and_state_have_pkce_safe_entropy() {
        let first = random_urlsafe(32);
        let second = random_urlsafe(32);
        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
    }

    #[test]
    fn callbacks_require_the_narrow_path_and_preserve_errors() {
        let callback = parse_callback("/cubic/oauth/callback?code=example&state=expected").unwrap();
        assert_eq!(
            callback,
            OAuthCallback::Code {
                code: "example".into(),
                state: "expected".into()
            }
        );
        let debug = format!("{callback:?}");
        assert!(!debug.contains("example"));
        assert!(!debug.contains("expected"));
        assert!(parse_callback("/other?code=x&state=y").is_err());
        assert_eq!(
            parse_callback("/cubic/oauth/callback?error=access_denied&state=s").unwrap(),
            OAuthCallback::Error {
                code: "access_denied".into(),
                state: Some("s".into())
            }
        );
    }
}
