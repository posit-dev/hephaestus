//! The winit event loop that drives a [`WindowApp`].
//!
//! Everything winit-shaped lives here: window creation, the
//! `ApplicationHandler` impl, and the translation from winit's events to
//! [`Event`]. The rest of [`crate::window`] is backend-agnostic.

use std::cell::Cell;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
#[cfg(not(target_arch = "wasm32"))]
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::geometry::{Point, Size};
use crate::window::renderer::HostRenderer;
use crate::window::surface::WindowSurface;
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

/// Everything that only exists once the window has been created.
struct State {
    window: Arc<Window>,
    surface: WindowSurface,
    renderer: HostRenderer,
    /// When the hitmap was last refreshed, for `WindowConfig::pick_interval`.
    last_pick: Option<std::time::Instant>,
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
            last_pick: None,
        })
    }

    /// Draw, rasterise and present one frame.
    fn redraw(&mut self) -> Result<(), WindowError> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let (width, height) = state.surface.size();
        let size = Size::new(width as f64, height as f64);

        // Decide before drawing: on the sparse-strip backend the pick pass is
        // skipped during the *replay*, not just at rasterisation, so this has
        // to be known before any draw reaches the scene.
        if let Some(interval) = self.config.pick_interval {
            let now = std::time::Instant::now();
            let due = state
                .last_pick
                .is_none_or(|last| now.duration_since(last) >= interval);
            state.renderer.set_refresh_pick(due);
            if due {
                state.last_pick = Some(now);
            }
        }

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

    /// Hand one translated event to the app, honouring an exit request.
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let (width, height) = state.surface.size();
        let mut exit = false;
        let redraw = Cell::new(false);
        let mut ctx = EventCtx {
            renderer: &state.renderer,
            redraw: &redraw,
            cursor: self.cursor,
            size: Size::new(width as f64, height as f64),
            dpi: state.dpi,
            exit: &mut exit,
        };
        self.app.event(&mut ctx, event);
        if redraw.get() {
            state.window.request_redraw();
        }
        if exit {
            event_loop.exit();
        }
    }

    /// Record the failure and unwind the loop; `run` surfaces it to the caller.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: WindowError) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl<A: WindowApp> ApplicationHandler for Driver<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Resume can fire more than once; the window is built on the first.
        if self.state.is_some() {
            return;
        }
        match self.init(event_loop) {
            Ok(state) => self.state = Some(state),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.config.continuous_redraw {
            return;
        }
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
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
