//! Native application lifecycle and window integration.

use std::{
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use cubic_render::{FrameStatus, Renderer, RendererInitError};
use cubic_ui::{ChatMode, ChatSessionPort};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

#[cfg(target_os = "ios")]
mod ios;
mod xal_sign_in;

#[cfg(target_os = "ios")]
pub use ios::run_from_native_host;
pub use xal_sign_in::{XalSignInWindowError, capture_xal_authorization};

const WINDOW_TITLE: &str = "Cubic";
const INITIAL_WIDTH: f64 = 1280.0;
const INITIAL_HEIGHT: f64 = 720.0;
const CHAT_CJK_FONT_KEY: &str = "cubic-system-cjk";
#[cfg(target_os = "windows")]
const MAX_SYSTEM_FONT_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SystemFontCandidate {
    file_name: &'static str,
    face_index: u32,
    display_name: &'static str,
}

#[cfg(target_os = "windows")]
const CJK_FONT_CANDIDATES: &[SystemFontCandidate] = &[
    SystemFontCandidate {
        file_name: "msyh.ttc",
        face_index: 0,
        display_name: "Microsoft YaHei",
    },
    SystemFontCandidate {
        file_name: "YuGothR.ttc",
        face_index: 0,
        display_name: "Yu Gothic",
    },
    SystemFontCandidate {
        file_name: "malgun.ttf",
        face_index: 0,
        display_name: "Malgun Gothic",
    },
];

struct LoadedSystemFont {
    bytes: Vec<u8>,
    face_index: u32,
    display_name: &'static str,
}

/// Starts Cubic's native event loop on the calling thread.
pub fn run() -> Result<(), PlatformError> {
    let event_loop = EventLoop::new().map_err(PlatformError::CreateEventLoop)?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut application = CubicApplication::default();
    event_loop
        .run_app(&mut application)
        .map_err(PlatformError::RunEventLoop)?;

    if let Some(error) = application.startup_error {
        Err(PlatformError::Startup(error))
    } else {
        Ok(())
    }
}

/// Starts the event-driven Chat Mode window on the calling thread.
pub fn run_chat(port: Box<dyn ChatSessionPort>) -> Result<(), PlatformError> {
    let event_loop = EventLoop::new().map_err(PlatformError::CreateEventLoop)?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut application = ChatApplication::new(port);
    event_loop
        .run_app(&mut application)
        .map_err(PlatformError::RunEventLoop)?;
    if let Some(error) = application.startup_error {
        Err(PlatformError::Startup(error))
    } else {
        Ok(())
    }
}

struct ChatApplication {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui: Option<egui_winit::State>,
    chat: ChatMode,
    startup_error: Option<StartupError>,
    occluded: bool,
}

impl ChatApplication {
    fn new(port: Box<dyn ChatSessionPort>) -> Self {
        Self {
            window: None,
            renderer: None,
            egui: None,
            chat: ChatMode::new(port),
            startup_error: None,
            occluded: false,
        }
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), StartupError> {
        let attributes = Window::default_attributes()
            .with_title("Cubic — Chat Mode")
            .with_resizable(true)
            .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(StartupError::CreateWindow)?,
        );
        let renderer = pollster::block_on(Renderer::new(Arc::clone(&window)))
            .map_err(StartupError::InitializeRenderer)?;
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::dark());
        install_chat_font_fallback(&context);
        let egui = egui_winit::State::new(
            context,
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(4_096),
        );
        self.window = Some(Arc::clone(&window));
        self.renderer = Some(renderer);
        self.egui = Some(egui);
        window.request_redraw();
        Ok(())
    }

    fn request_redraw(&self) {
        if !self.occluded
            && self.renderer.as_ref().is_some_and(Renderer::is_renderable)
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(renderer), Some(egui)) =
            (&self.window, &mut self.renderer, &mut self.egui)
        else {
            return;
        };
        let input = egui.take_egui_input(window);
        let context = egui.egui_ctx().clone();
        let mut output = context.run_ui(input, |ui| self.chat.show(ui));
        egui.handle_platform_output_with_event_loop(window, event_loop, output.platform_output);
        let pixels_per_point = context.pixels_per_point();
        let paint_jobs = context.tessellate(output.shapes, pixels_per_point);
        let textures = std::mem::take(&mut output.textures_delta);
        if let Err(error) = renderer.render_ui(&paint_jobs, textures, pixels_per_point) {
            tracing::error!(%error, "fatal Chat Mode rendering error");
            event_loop.exit();
        }
    }
}

fn install_chat_font_fallback(context: &egui::Context) {
    let Some(font) = load_system_cjk_font() else {
        tracing::warn!("no supported system CJK font was found; using egui's built-in fonts only");
        return;
    };
    let display_name = font.display_name;
    context.set_fonts(font_definitions_with_cjk_fallback(
        egui::FontDefinitions::default(),
        font,
    ));
    tracing::info!(font = display_name, "installed system CJK font fallback");
}

fn font_definitions_with_cjk_fallback(
    mut definitions: egui::FontDefinitions,
    font: LoadedSystemFont,
) -> egui::FontDefinitions {
    let mut data = egui::FontData::from_owned(font.bytes);
    data.index = font.face_index;
    definitions
        .font_data
        .insert(CHAT_CJK_FONT_KEY.to_owned(), Arc::new(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        definitions
            .families
            .entry(family)
            .or_default()
            .push(CHAT_CJK_FONT_KEY.to_owned());
    }
    definitions
}

#[cfg(target_os = "windows")]
fn load_system_cjk_font() -> Option<LoadedSystemFont> {
    let fonts_directory = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)?
        .join("Fonts");
    CJK_FONT_CANDIDATES.iter().find_map(|candidate| {
        read_bounded_system_font(&fonts_directory.join(candidate.file_name)).map(|bytes| {
            LoadedSystemFont {
                bytes,
                face_index: candidate.face_index,
                display_name: candidate.display_name,
            }
        })
    })
}

#[cfg(target_os = "windows")]
fn read_bounded_system_font(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_SYSTEM_FONT_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SYSTEM_FONT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= MAX_SYSTEM_FONT_BYTES).then_some(bytes)
}

#[cfg(not(target_os = "windows"))]
fn load_system_cjk_font() -> Option<LoadedSystemFont> {
    None
}

impl ApplicationHandler for ChatApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none()
            && self.startup_error.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            tracing::error!(%error, "Chat Mode startup failed");
            self.startup_error = Some(error);
            event_loop.exit();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.renderer = None;
        self.egui = None;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        if let Some(egui) = &mut self.egui {
            let response = egui.on_window_event(window, &event);
            if response.repaint {
                window.request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                self.chat.disconnect();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
                self.request_redraw();
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    self.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.chat.poll()
            || self
                .renderer
                .as_ref()
                .is_some_and(Renderer::has_pending_ui_textures)
        {
            self.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(200),
        ));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.chat.disconnect();
        tracing::info!("Cubic Chat Mode stopped cleanly");
    }
}

#[derive(Default)]
struct CubicApplication {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    startup_error: Option<StartupError>,
    occluded: bool,
}

impl CubicApplication {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), StartupError> {
        let window = match &self.window {
            Some(window) => Arc::clone(window),
            None => {
                let attributes = Window::default_attributes()
                    .with_title(WINDOW_TITLE)
                    .with_resizable(true)
                    .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
                let window = Arc::new(
                    event_loop
                        .create_window(attributes)
                        .map_err(StartupError::CreateWindow)?,
                );
                self.window = Some(Arc::clone(&window));
                window
            }
        };

        let renderer = pollster::block_on(Renderer::new(Arc::clone(&window)))
            .map_err(StartupError::InitializeRenderer)?;
        self.renderer = Some(renderer);
        window.request_redraw();
        Ok(())
    }

    fn fail_startup(&mut self, event_loop: &ActiveEventLoop, error: StartupError) {
        tracing::error!(%error, "application startup failed");
        self.startup_error = Some(error);
        event_loop.exit();
    }

    fn request_next_frame(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let Some(renderer) = &self.renderer else {
            return;
        };

        if renderer.is_renderable() && !self.occluded {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler for CubicApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none()
            && self.startup_error.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail_startup(event_loop, error);
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.renderer = None;
        tracing::info!("application suspended; presentation surface released");
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
            .is_none_or(|window| window.id() != window_id)
        {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
                self.request_next_frame();
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    self.request_next_frame();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(renderer) = &mut self.renderer else {
                    return;
                };

                match renderer.render() {
                    Ok(FrameStatus::Presented | FrameStatus::Reconfigured) => {
                        self.request_next_frame();
                    }
                    Ok(FrameStatus::Skipped) => self.request_next_frame(),
                    Err(error) => {
                        tracing::error!(%error, "fatal rendering error");
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        tracing::info!("Cubic stopped cleanly");
    }
}

/// Error returned by the native application bootstrap.
#[derive(Debug)]
pub enum PlatformError {
    /// The operating system event loop could not be created.
    CreateEventLoop(winit::error::EventLoopError),
    /// The event loop itself terminated with an error.
    RunEventLoop(winit::error::EventLoopError),
    /// Window or renderer initialization failed after the event loop started.
    Startup(StartupError),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateEventLoop(error) => {
                write!(formatter, "could not create event loop: {error}")
            }
            Self::RunEventLoop(error) => write!(formatter, "event loop failed: {error}"),
            Self::Startup(error) => write!(formatter, "startup failed: {error}"),
        }
    }
}

impl Error for PlatformError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateEventLoop(error) | Self::RunEventLoop(error) => Some(error),
            Self::Startup(error) => Some(error),
        }
    }
}

/// Failure to create the initial native window or renderer.
#[derive(Debug)]
pub enum StartupError {
    /// The native window could not be created.
    CreateWindow(winit::error::OsError),
    /// GPU initialization failed.
    InitializeRenderer(RendererInitError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateWindow(error) => {
                write!(formatter, "could not create Cubic window: {error}")
            }
            Self::InitializeRenderer(error) => {
                write!(formatter, "could not initialize renderer: {error}")
            }
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateWindow(error) => Some(error),
            Self::InitializeRenderer(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_font_is_appended_without_replacing_existing_fallbacks() {
        let defaults = egui::FontDefinitions::default();
        let proportional_before = defaults
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        let configured = font_definitions_with_cjk_fallback(
            defaults,
            LoadedSystemFont {
                bytes: vec![1, 2, 3],
                face_index: 7,
                display_name: "synthetic test font",
            },
        );

        let proportional = configured
            .families
            .get(&egui::FontFamily::Proportional)
            .expect("proportional family must exist");
        assert_eq!(
            &proportional[..proportional_before.len()],
            proportional_before
        );
        assert_eq!(
            proportional.last().map(String::as_str),
            Some(CHAT_CJK_FONT_KEY)
        );
        let data = configured
            .font_data
            .get(CHAT_CJK_FONT_KEY)
            .expect("fallback font data must be registered");
        assert_eq!(data.font.as_ref(), [1, 2, 3]);
        assert_eq!(data.index, 7);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cjk_candidates_have_deterministic_priority() {
        assert_eq!(
            CJK_FONT_CANDIDATES
                .iter()
                .map(|candidate| candidate.display_name)
                .collect::<Vec<_>>(),
            ["Microsoft YaHei", "Yu Gothic", "Malgun Gothic"]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_windows_fallback_covers_common_han() {
        let Some(font) = load_system_cjk_font() else {
            return;
        };
        let context = egui::Context::default();
        context.set_fonts(font_definitions_with_cjk_fallback(
            egui::FontDefinitions::default(),
            font,
        ));
        let mut output = context.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        let font_id = egui::FontId::proportional(14.0);
        context.fonts_mut(|fonts| {
            assert!(fonts.has_glyphs(&font_id, "漢字"));
        });
    }
}
