//! Input and lifecycle events delivered to a [`WindowApp`](super::WindowApp).
//!
//! These types are hephaestus's own rather than the windowing backend's, so
//! the backend stays an implementation detail of [`crate::window`].

use crate::geometry::{Point, Size};

/// Something the window reported between frames.
///
/// Positions are in device pixels, the same coordinate space as
/// [`Frame::size`](super::Frame::size) and
/// [`EventCtx::pick_at`](super::EventCtx::pick_at).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// The drawing surface changed size, its scale factor changed, or both.
    ///
    /// Delivered before the frame at the new size is drawn.
    Resized {
        /// New drawing surface size in device pixels.
        size: Size,
        /// New dots per inch for the surface.
        dpi: f64,
    },

    /// The cursor moved to a new position over the window.
    CursorMoved {
        /// Cursor position in device pixels.
        position: Point,
    },

    /// The cursor left the window.
    CursorLeft,

    /// A mouse button went down.
    ///
    /// The cursor position is [`EventCtx::cursor`](super::EventCtx::cursor);
    /// the platform reports the button and the position separately.
    MouseDown {
        /// Which button.
        button: MouseButton,
    },

    /// A mouse button came back up.
    MouseUp {
        /// Which button.
        button: MouseButton,
    },

    /// The user asked to close the window.
    ///
    /// The window stays open until the app calls
    /// [`EventCtx::exit`](super::EventCtx::exit).
    CloseRequested,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The middle button, usually the scroll wheel.
    Middle,
    /// The "back" side button.
    Back,
    /// The "forward" side button.
    Forward,
    /// Any further button, identified by its platform index.
    Other(u16),
}
