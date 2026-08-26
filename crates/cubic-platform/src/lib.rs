//! Native application lifecycle and window integration.

use std::{error::Error, fmt, sync::Arc};

use cubic_render::{FrameStatus, Renderer, RendererInitError};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

#[cfg(target_os = "ios")]
mod ios;

#[cfg(target_os = "ios")]
pub use ios::run_from_native_host;

const WINDOW_TITLE: &str = "Cubic";
const INITIAL_WIDTH: f64 = 1280.0;
const INITIAL_HEIGHT: f64 = 720.0;

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
