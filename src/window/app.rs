//! The winit event loop that drives a [`WindowApp`].
//!
//! Everything winit-shaped lives here: window creation, the
//! `ApplicationHandler` impl, and the translation from winit's events to
//! [`Event`]. The rest of [`crate::window`] is backend-agnostic.

use std::cell::Cell;
#[cfg(not(target_arch = "wasm32"))]
use std::marker::PhantomData;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
#[cfg(not(target_arch = "wasm32"))]
use winit::event_loop::{ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::geometry::{Point, Size};
use crate::window::renderer::HostRenderer;
use crate::window::surface::WindowSurface;
#[cfg(not(target_arch = "wasm32"))]
use crate::window::MessageApp;
use crate::window::BASE_DPI;
use crate::window::{Event, EventCtx, Frame, MouseButton, WindowApp, WindowConfig, WindowError};

/// Open the window and pump events until the app exits or something fails.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run<A: WindowApp>(config: WindowConfig, app: A) -> Result<(), WindowError> {
    let event_loop = EventLoop::new().map_err(|e| WindowError::EventLoop(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut driver = Driver {
        config,
        app,
        state: None,
        cursor: None,
        error: None,
    };
    event_loop
        .run_app(&mut driver)
        .map_err(|e| WindowError::EventLoop(e.to_string()))?;

    match driver.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

// Same desktop-only gate as `run`: wasm has no worker threads to message
// from, and `EventLoopProxy` here is the desktop import.
#[cfg(not(target_arch = "wasm32"))]
/// An event loop prepared for cross-thread messages, before the app runs.
///
/// winit requires the [`EventLoopProxy`] to come from the event loop before
/// `run_app` consumes it, and `run` blocks on the main thread until the
/// window closes. The handle splits the two: build it, hand [`Self::sender`]
/// clones to worker threads, then call [`Self::run`] on the main thread.
pub struct WindowHandle<T: Send + 'static> {
    event_loop: EventLoop<T>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + 'static> WindowHandle<T> {
    /// Create the event loop and its user-event channel.
    ///
    /// Must be called on the main thread, like everything else that touches
    /// the platform event loop.
    pub fn new() -> Result<Self, WindowError> {
        let event_loop = EventLoop::with_user_event()
            .build()
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;
        event_loop.set_control_flow(ControlFlow::Wait);
        Ok(Self { event_loop })
    }

    /// A sender half of the message channel, cheap to clone and `Send`.
    pub fn sender(&self) -> WindowSender<T> {
        WindowSender {
            proxy: self.event_loop.create_proxy(),
        }
    }

    /// Open the window and pump events until the app exits or something fails.
    ///
    /// Blocks the calling thread, which must be the main thread.
    pub fn run<A: MessageApp<T>>(self, config: WindowConfig, app: A) -> Result<(), WindowError> {
        let mut driver = MsgDriver {
            driver: Driver {
                config,
                app,
                state: None,
                cursor: None,
                error: None,
            },
            _msg: PhantomData,
        };
        self.event_loop
            .run_app(&mut driver)
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;

        match driver.driver.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Sends messages to a running window from any thread.
///
/// Sending wakes the event loop out of `ControlFlow::Wait`; the app receives
/// the message through [`MessageApp::message`] and a redraw is requested
/// afterwards, so a worker thread can push new data and see it drawn without
/// any further coordination.
#[cfg(not(target_arch = "wasm32"))]
pub struct WindowSender<T: 'static> {
    proxy: EventLoopProxy<T>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: 'static> Clone for WindowSender<T> {
    fn clone(&self) -> Self {
        Self {
            proxy: self.proxy.clone(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + 'static> WindowSender<T> {
    /// Queue a message for the window. Fails if the event loop has exited.
    pub fn send(&self, message: T) -> Result<(), WindowError> {
        self.proxy
            .send_event(message)
            .map_err(|e| WindowError::EventLoop(e.to_string()))
    }
}

/// Everything that only exists once the window has been created.
struct State {
    window: Arc<Window>,
    surface: WindowSurface,
    renderer: HostRenderer,
    dpi: f64,
}

struct Driver<A: WindowApp> {
    config: WindowConfig,
    app: A,
    state: Option<State>,
    cursor: Option<Point>,
    error: Option<WindowError>,
}

impl<A: WindowApp> Driver<A> {
    fn init(&self, event_loop: &ActiveEventLoop) -> Result<State, WindowError> {
        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|e| WindowError::Window(e.to_string()))?,
        );

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        // Same backend selection as the headless path: GL alongside PRIMARY so
        // a host without Vulkan still finds an adapter, `WGPU_BACKENDS` still
        // overrides.
        descriptor.backends =
            wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
        let instance = wgpu::Instance::new(descriptor);
        let target = instance
            .create_surface(window.clone())
            .map_err(|e| WindowError::Surface(e.to_string()))?;

        let physical = window.inner_size();
        let surface = WindowSurface::new(
            &instance,
            target,
            physical.width,
            physical.height,
            self.config.present_mode,
            // `None` asks for no intermediate texture: this backend writes the
            // acquired swap-chain texture itself.
            (!self.config.backend.can_present_directly())
                .then(|| self.config.backend.target_usage()),
        )?;

        let mut renderer = HostRenderer::new(
            self.config.backend,
            surface.device(),
            surface.queue(),
            self.config.picking,
        )?;
        if self.config.backend.can_present_directly() {
            renderer.set_target_format(surface.format());
        }

        let dpi = BASE_DPI * window.scale_factor();
        Ok(State {
            window,
            surface,
            renderer,
            dpi,
        })
    }

    /// Draw, rasterise and present one frame.
    fn redraw(&mut self) -> Result<(), WindowError> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let (width, height) = state.surface.size();
        let size = Size::new(width as f64, height as f64);

        state.renderer.scene().clear();
        {
            let mut frame = Frame {
                scene: state.renderer.scene(),
                size,
                dpi: state.dpi,
            };
            self.app.draw(&mut frame);
        }

        let background = self.config.background;
        let renderer = &mut state.renderer;
        let window = &state.window;
        state.surface.draw_frame(
            |view| {
                renderer
                    .render_to_texture(view, width, height, background)
                    .map_err(WindowError::from)
            },
            || window.pre_present_notify(),
        )?;

        if self.config.continuous_redraw {
            state.window.request_redraw();
        }
        Ok(())
    }

    /// Build an `EventCtx`, run the callback against the app, then honour the
    /// redraw and exit requests it made.
    fn with_ctx(
        &mut self,
        event_loop: &ActiveEventLoop,
        f: impl FnOnce(&mut A, &mut EventCtx<'_>),
    ) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let (width, height) = state.surface.size();
        let mut exit = false;
        let redraw = Cell::new(false);
        let mut ctx = EventCtx {
            index: state.renderer.pick_index(),
            redraw: &redraw,
            cursor: self.cursor,
            size: Size::new(width as f64, height as f64),
            dpi: state.dpi,
            exit: &mut exit,
        };
        f(&mut self.app, &mut ctx);
        if redraw.get() {
            state.window.request_redraw();
        }
        if exit {
            event_loop.exit();
        }
    }

    /// Hand one translated event to the app, honouring an exit request.
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        self.with_ctx(event_loop, |app, ctx| app.event(ctx, event));
    }

    /// Record the failure and unwind the loop; `run` surfaces it to the caller.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: WindowError) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl<A: WindowApp> Driver<A> {
    fn on_resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Resume can fire more than once; the window is built on the first.
        if self.state.is_some() {
            return;
        }
        match self.init(event_loop) {
            Ok(state) => self.state = Some(state),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn on_window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::Resized(physical) => {
                let Some(state) = self.state.as_mut() else {
                    return;
                };
                state.surface.resize(physical.width, physical.height);
                let (width, height) = state.surface.size();
                let dpi = state.dpi;
                state.window.request_redraw();
                self.dispatch(
                    event_loop,
                    Event::Resized {
                        size: Size::new(width as f64, height as f64),
                        dpi,
                    },
                );
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let Some(state) = self.state.as_mut() else {
                    return;
                };
                state.dpi = BASE_DPI * scale_factor;
                // The matching `Resized` follows on every platform, but the
                // dpi change alone already invalidates the drawn frame.
                let (width, height) = state.surface.size();
                let dpi = state.dpi;
                state.window.request_redraw();
                self.dispatch(
                    event_loop,
                    Event::Resized {
                        size: Size::new(width as f64, height as f64),
                        dpi,
                    },
                );
            }

            WindowEvent::RedrawRequested => {
                if let Err(error) = self.redraw() {
                    self.fail(event_loop, error);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x, position.y);
                self.cursor = Some(point);
                self.dispatch(event_loop, Event::CursorMoved { position: point });
            }

            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.dispatch(event_loop, Event::CursorLeft);
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let button = translate_button(button);
                let event = match state {
                    ElementState::Pressed => Event::MouseDown { button },
                    ElementState::Released => Event::MouseUp { button },
                };
                self.dispatch(event_loop, event);
            }

            WindowEvent::CloseRequested => {
                self.dispatch(event_loop, Event::CloseRequested);
            }

            _ => {}
        }
    }

    fn on_about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.config.continuous_redraw {
            return;
        }
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }
}

impl<A: WindowApp> ApplicationHandler for Driver<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.on_resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.on_window_event(event_loop, id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.on_about_to_wait(event_loop);
    }
}

/// The same driver, with a user-event channel layered on top.
///
/// Kept as a separate type rather than a generic parameter on [`Driver`] so
/// the plain `ApplicationHandler` impl above stays non-generic: two impls of
/// the same trait with overlapping type parameters would not compile.
#[cfg(not(target_arch = "wasm32"))]
struct MsgDriver<A: WindowApp, T> {
    driver: Driver<A>,
    _msg: PhantomData<fn() -> T>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + 'static, A: MessageApp<T>> ApplicationHandler<T> for MsgDriver<A, T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.driver.on_resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.driver.on_window_event(event_loop, id, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, message: T) {
        // A message invalidates whatever is on screen, so redraw whether or
        // not the handler asked: it usually has nothing else to key off.
        self.driver.with_ctx(event_loop, |app, ctx| {
            app.message(ctx, message);
            ctx.request_redraw();
        });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.driver.on_about_to_wait(event_loop);
    }
}

fn translate_button(button: winit::event::MouseButton) -> MouseButton {
    match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        winit::event::MouseButton::Other(n) => MouseButton::Other(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_winit_button_maps_to_its_counterpart() {
        assert_eq!(
            translate_button(winit::event::MouseButton::Left),
            MouseButton::Left
        );
        assert_eq!(
            translate_button(winit::event::MouseButton::Middle),
            MouseButton::Middle
        );
        assert_eq!(
            translate_button(winit::event::MouseButton::Forward),
            MouseButton::Forward
        );
    }

    #[test]
    fn unnamed_buttons_keep_their_platform_index() {
        assert_eq!(
            translate_button(winit::event::MouseButton::Other(9)),
            MouseButton::Other(9)
        );
    }
}
