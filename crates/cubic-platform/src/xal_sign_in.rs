use std::time::Duration;

use cubic_auth::{XalAuthorizationCode, XalInteractiveAuthorization};
use thiserror::Error;

/// A failure while hosting the experimental XAL Microsoft sign-in navigation.
#[derive(Debug, Error)]
pub enum XalSignInWindowError {
    #[error("the experimental XAL sign-in window is currently supported only on Windows")]
    UnsupportedPlatform,
    #[error("the SISU authorization URL is not the expected Microsoft HTTPS endpoint")]
    InvalidInitialUrl,
    #[error("the experimental XAL sign-in window requires the Microsoft Edge WebView2 Runtime")]
    WebView2Unavailable,
    #[error("could not create an isolated temporary WebView2 profile")]
    PrivateProfile(#[source] std::io::Error),
    #[error("could not create the XAL sign-in event loop")]
    CreateEventLoop(#[source] winit::error::EventLoopError),
    #[error("the XAL sign-in event loop failed")]
    RunEventLoop(#[source] winit::error::EventLoopError),
    #[error("could not create the XAL sign-in window")]
    CreateWindow(#[source] winit::error::OsError),
    #[error("Microsoft sign-in was cancelled")]
    Cancelled,
    #[error("Microsoft sign-in timed out after {0:?}")]
    Timeout(Duration),
    #[error(
        "the sign-in page attempted a top-level navigation outside the approved identity hosts"
    )]
    NavigationBlocked,
    #[error("Microsoft returned an invalid XAL authorization redirect")]
    Authorization(#[source] cubic_auth::AuthError),
    #[error("the XAL sign-in window stopped without an authorization result")]
    MissingResult,
}

/// Opens the narrowly scoped XAL sign-in navigation host and captures its authorization code.
#[cfg(target_os = "windows")]
pub fn capture_xal_authorization(
    authorization: &XalInteractiveAuthorization,
    timeout: Duration,
) -> Result<XalAuthorizationCode, XalSignInWindowError> {
    windows::capture(authorization, timeout)
}

/// Other platforms require a native authentication-session host in future Phase 9 work.
#[cfg(not(target_os = "windows"))]
pub fn capture_xal_authorization(
    _authorization: &XalInteractiveAuthorization,
    _timeout: Duration,
) -> Result<XalAuthorizationCode, XalSignInWindowError> {
    Err(XalSignInWindowError::UnsupportedPlatform)
}

#[cfg(target_os = "windows")]
mod windows {
    use std::time::{Duration, Instant};

    use cubic_auth::{
        AuthError, XalAuthorizationCode, XalInteractiveAuthorization, XalRedirectValidator,
    };
    use url::Url;
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        window::{Window, WindowId},
    };
    use wry::{
        NewWindowResponse, PermissionResponse, WebContext, WebView, WebViewBuilder,
        WebViewBuilderExtWindows,
    };
    use zeroize::Zeroize;

    use super::XalSignInWindowError;

    const WINDOW_TITLE: &str = "Cubic — Microsoft Sign In";
    const INITIAL_WIDTH: f64 = 900.0;
    const INITIAL_HEIGHT: f64 = 680.0;
    const AUTHORIZATION_PATH: &str = "/oauth20_authorize.srf";
    const REDIRECT_PATH: &str = "/oauth20_desktop.srf";
    const IDENTITY_HOSTS: &[&str] = &[
        "login.live.com",
        "account.live.com",
        "login.microsoftonline.com",
        "account.microsoft.com",
    ];

    enum SignInEvent {
        Captured(XalAuthorizationCode),
        AuthenticationFailed(AuthError),
        NavigationBlocked,
    }

    pub(super) fn capture(
        authorization: &XalInteractiveAuthorization,
        timeout: Duration,
    ) -> Result<XalAuthorizationCode, XalSignInWindowError> {
        let initial_url = authorization.authorization_url();
        if !is_initial_authorization_url(initial_url) {
            return Err(XalSignInWindowError::InvalidInitialUrl);
        }
        let event_loop = EventLoop::<SignInEvent>::with_user_event()
            .build()
            .map_err(XalSignInWindowError::CreateEventLoop)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(XalSignInWindowError::Timeout(timeout))?;
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        let proxy = event_loop.create_proxy();
        let mut application = SignInApplication {
            initial_url: initial_url.as_str().to_owned(),
            validator: Some(authorization.redirect_validator()),
            proxy,
            lifecycle: CaptureLifecycle::new(deadline, timeout),
            window: None,
            webview: None,
            web_context: None,
            profile_directory: None,
            result: None,
        };
        event_loop
            .run_app(&mut application)
            .map_err(XalSignInWindowError::RunEventLoop)?;
        application
            .result
            .unwrap_or(Err(XalSignInWindowError::MissingResult))
    }

    struct SignInApplication {
        initial_url: String,
        validator: Option<XalRedirectValidator>,
        proxy: EventLoopProxy<SignInEvent>,
        lifecycle: CaptureLifecycle,
        window: Option<Window>,
        webview: Option<WebView>,
        web_context: Option<WebContext>,
        profile_directory: Option<tempfile::TempDir>,
        result: Option<Result<XalAuthorizationCode, XalSignInWindowError>>,
    }

    impl SignInApplication {
        fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), XalSignInWindowError> {
            let window = event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(WINDOW_TITLE)
                        .with_resizable(true)
                        .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT)),
                )
                .map_err(XalSignInWindowError::CreateWindow)?;
            let validator = self
                .validator
                .take()
                .ok_or(XalSignInWindowError::MissingResult)?;
            let proxy = self.proxy.clone();
            let profile_directory = tempfile::Builder::new()
                .prefix("cubic-xal-webview-")
                .tempdir()
                .map_err(XalSignInWindowError::PrivateProfile)?;
            let mut web_context = WebContext::new(Some(profile_directory.path().to_path_buf()));
            let webview = WebViewBuilder::new_with_web_context(&mut web_context)
                .with_url(&self.initial_url)
                .with_incognito(true)
                .with_devtools(false)
                .with_clipboard(false)
                .with_hotkeys_zoom(false)
                .with_general_autofill_enabled(false)
                .with_permission_handler(|_| PermissionResponse::Deny)
                .with_download_started_handler(|_, _| false)
                .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
                .with_default_context_menus(false)
                .with_browser_accelerator_keys(false)
                .with_navigation_handler(move |mut navigation| {
                    let allow = handle_navigation(&proxy, &validator, &navigation);
                    navigation.zeroize();
                    allow
                })
                .build(&window)
                .map_err(|_| XalSignInWindowError::WebView2Unavailable)?;
            self.window = Some(window);
            self.webview = Some(webview);
            self.web_context = Some(web_context);
            self.profile_directory = Some(profile_directory);
            Ok(())
        }

        fn finish(
            &mut self,
            event_loop: &ActiveEventLoop,
            result: Result<XalAuthorizationCode, XalSignInWindowError>,
        ) {
            if self.lifecycle.complete() {
                self.result = Some(result);
                self.webview = None;
                self.window = None;
                self.web_context = None;
                self.profile_directory = None;
                event_loop.exit();
            }
        }

        fn cancel(&mut self, event_loop: &ActiveEventLoop) {
            if matches!(
                self.lifecycle.cancel(),
                Some(LifecycleTransition::Cancelled)
            ) {
                self.finish_after_transition(event_loop, Err(XalSignInWindowError::Cancelled));
            }
        }

        fn expire(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
            if let Some(LifecycleTransition::TimedOut(timeout)) = self.lifecycle.tick(now) {
                self.finish_after_transition(
                    event_loop,
                    Err(XalSignInWindowError::Timeout(timeout)),
                );
            }
        }

        fn finish_after_transition(
            &mut self,
            event_loop: &ActiveEventLoop,
            result: Result<XalAuthorizationCode, XalSignInWindowError>,
        ) {
            self.result = Some(result);
            self.webview = None;
            self.window = None;
            self.web_context = None;
            self.profile_directory = None;
            event_loop.exit();
        }
    }

    impl ApplicationHandler<SignInEvent> for SignInApplication {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.webview.is_none()
                && self.result.is_none()
                && let Err(error) = self.initialize(event_loop)
            {
                self.finish(event_loop, Err(error));
            }
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SignInEvent) {
            let result = match event {
                SignInEvent::Captured(code) => Ok(code),
                SignInEvent::AuthenticationFailed(error) => {
                    Err(XalSignInWindowError::Authorization(error))
                }
                SignInEvent::NavigationBlocked => Err(XalSignInWindowError::NavigationBlocked),
            };
            self.finish(event_loop, result);
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: WindowId,
            event: WindowEvent,
        ) {
            if self
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
                && matches!(event, WindowEvent::CloseRequested)
            {
                self.cancel(event_loop);
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.expire(event_loop, Instant::now());
            if self.result.is_none() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.lifecycle.deadline));
            }
        }

        fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
            if self.result.is_none() {
                self.result = Some(Err(XalSignInWindowError::Cancelled));
            }
        }
    }

    struct CaptureLifecycle {
        deadline: Instant,
        timeout: Duration,
        finished: bool,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LifecycleTransition {
        Cancelled,
        TimedOut(Duration),
    }

    impl CaptureLifecycle {
        fn new(deadline: Instant, timeout: Duration) -> Self {
            Self {
                deadline,
                timeout,
                finished: false,
            }
        }

        fn complete(&mut self) -> bool {
            if self.finished {
                false
            } else {
                self.finished = true;
                true
            }
        }

        fn cancel(&mut self) -> Option<LifecycleTransition> {
            self.complete().then_some(LifecycleTransition::Cancelled)
        }

        fn tick(&mut self, now: Instant) -> Option<LifecycleTransition> {
            if !self.finished && now >= self.deadline {
                self.finished = true;
                Some(LifecycleTransition::TimedOut(self.timeout))
            } else {
                None
            }
        }
    }

    fn handle_navigation(
        proxy: &EventLoopProxy<SignInEvent>,
        validator: &XalRedirectValidator,
        navigation: &str,
    ) -> bool {
        let Ok(url) = Url::parse(navigation) else {
            let _result = proxy.send_event(SignInEvent::NavigationBlocked);
            return false;
        };
        if is_desktop_redirect(&url) {
            let event = match validator.capture_if_redirect(navigation) {
                Ok(Some(code)) => SignInEvent::Captured(code),
                Ok(None) => SignInEvent::NavigationBlocked,
                Err(error) => SignInEvent::AuthenticationFailed(error),
            };
            let _result = proxy.send_event(event);
            return false;
        }
        if is_allowed_identity_navigation(&url) {
            true
        } else {
            let _result = proxy.send_event(SignInEvent::NavigationBlocked);
            false
        }
    }

    fn is_initial_authorization_url(url: &Url) -> bool {
        is_exact_https_host(url, "login.live.com") && url.path() == AUTHORIZATION_PATH
    }

    fn is_desktop_redirect(url: &Url) -> bool {
        is_exact_https_host(url, "login.live.com") && url.path() == REDIRECT_PATH
    }

    fn is_allowed_identity_navigation(url: &Url) -> bool {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url
                .host_str()
                .is_some_and(|host| IDENTITY_HOSTS.contains(&host))
    }

    fn is_exact_https_host(url: &Url, host: &str) -> bool {
        url.scheme() == "https"
            && url.host_str() == Some(host)
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
    }

    #[cfg(test)]
    mod tests {
        use std::time::{Duration, Instant};

        use url::Url;

        use super::{
            CaptureLifecycle, IDENTITY_HOSTS, LifecycleTransition, is_allowed_identity_navigation,
            is_initial_authorization_url,
        };

        #[test]
        fn initial_url_is_the_exact_sisu_microsoft_authorization_endpoint() {
            assert!(is_initial_authorization_url(
                &Url::parse("https://login.live.com/oauth20_authorize.srf?state=synthetic")
                    .unwrap()
            ));
            for value in [
                "http://login.live.com/oauth20_authorize.srf",
                "https://attacker.login.live.com/oauth20_authorize.srf",
                "https://login.live.com.attacker.example/oauth20_authorize.srf",
                "https://login.live.com/wrong",
                "https://login.live.com:444/oauth20_authorize.srf",
            ] {
                assert!(!is_initial_authorization_url(&Url::parse(value).unwrap()));
            }
        }

        #[test]
        fn identity_navigation_allowlist_uses_exact_https_hosts() {
            for host in IDENTITY_HOSTS {
                let url = Url::parse(&format!("https://{host}/identity/path")).unwrap();
                assert!(is_allowed_identity_navigation(&url));
            }
            for value in [
                "http://login.live.com/identity/path",
                "https://attacker.login.live.com/identity/path",
                "https://login.live.com.attacker.example/identity/path",
                "https://example.com/identity/path",
                "https://user@login.live.com/identity/path",
                "https://login.live.com:444/identity/path",
            ] {
                assert!(!is_allowed_identity_navigation(&Url::parse(value).unwrap()));
            }
        }

        #[test]
        fn cancellation_and_timeout_are_distinct_results() {
            let timeout = Duration::from_secs(180);
            let now = Instant::now();
            let mut cancelled = CaptureLifecycle::new(now + timeout, timeout);
            assert_eq!(cancelled.cancel(), Some(LifecycleTransition::Cancelled));
            assert_eq!(cancelled.cancel(), None);
            assert_eq!(cancelled.tick(now + timeout), None);

            let mut timed_out = CaptureLifecycle::new(now + timeout, timeout);
            assert_eq!(timed_out.tick(now + timeout / 2), None);
            assert_eq!(
                timed_out.tick(now + timeout),
                Some(LifecycleTransition::TimedOut(timeout))
            );
            assert!(!timed_out.complete());
        }
    }
}
