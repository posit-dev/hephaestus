//! Named raster images for [`ImageGeom`](crate::plot::geom::ImageGeom).
//!
//! An [`ImageRegistry`] is a name-keyed in-memory map from a string to a
//! decoded [`Image`]. A geom's `"image"` channel carries names rather than
//! pixels, so a discrete scale can map a category to an image the same way it
//! maps one to a colour, and one registry entry reused across many rows is one
//! image as far as a backend's texture atlas is concerned.
//!
//! Decoding is the caller's responsibility. `crate::image` reads the four
//! raster formats this crate also writes; a caller holding RGBA8 bytes from
//! anywhere else builds an [`Image`] directly.
//!
//! # Example
//!
//! ```
//! use hephaestus::brush::{Image, ImageAlphaType, ImageFormat};
//! use hephaestus::plot::ImageRegistry;
//!
//! // A 1x1 opaque red pixel.
//! let px = Image {
//!     data: hephaestus::brush::Blob::new(std::sync::Arc::new(vec![255, 0, 0, 255])),
//!     format: ImageFormat::Rgba8,
//!     alpha_type: ImageAlphaType::Alpha,
//!     width: 1,
//!     height: 1,
//! };
//!
//! let mut registry = ImageRegistry::new();
//! registry.insert("red", px);
//! assert!(registry.contains("red"));
//! ```

use std::collections::HashMap;

use crate::brush::Image;

/// In-memory map from name to [`Image`].
///
/// Typical usage: build one at setup, register every image the plot needs,
/// hand it to [`Plot::image_registry`](crate::plot::Plot::image_registry), and
/// let the geom look names up at draw time.
///
/// [`Image`] is blob-backed, so cloning a registry — or an entry out of one —
/// shares the pixels rather than copying them.
#[derive(Debug, Default, Clone)]
pub struct ImageRegistry {
    images: HashMap<String, Image>,
}

impl ImageRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared empty registry, built once per process. Draw contexts that no
    /// caller supplied a registry to read from this rather than allocating one
    /// per call.
    pub fn shared_empty() -> &'static ImageRegistry {
        static SHARED: std::sync::OnceLock<ImageRegistry> = std::sync::OnceLock::new();
        SHARED.get_or_init(ImageRegistry::new)
    }

    /// Insert an image under the given name. Returns the previous image if one
    /// existed.
    pub fn insert(&mut self, name: impl Into<String>, image: Image) -> Option<Image> {
        self.images.insert(name.into(), image)
    }

    /// Look up an image by name.
    pub fn get(&self, name: &str) -> Option<&Image> {
        self.images.get(name)
    }

    /// Whether an image with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.images.contains_key(name)
    }

    /// Remove and return the image with the given name, if any.
    pub fn remove(&mut self, name: &str) -> Option<Image> {
        self.images.remove(name)
    }

    /// Iterate over the registered image names. Order is unspecified.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.images.keys().map(|s| s.as_str())
    }

    /// Number of registered images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether the registry has no entries.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{Blob, ImageAlphaType, ImageFormat};
    use std::sync::Arc;

    fn pixel(r: u8, g: u8, b: u8) -> Image {
        Image {
            data: Blob::new(Arc::new(vec![r, g, b, 255])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn a_new_registry_is_empty() {
        let r = ImageRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.get("anything").is_none());
    }

    #[test]
    fn insert_then_get_round_trips() {
        let mut r = ImageRegistry::new();
        assert!(r.insert("red", pixel(255, 0, 0)).is_none());
        assert!(r.contains("red"));
        assert_eq!(r.len(), 1);
        assert_eq!(r.get("red").expect("registered").width, 1);
    }

    #[test]
    fn insert_returns_the_displaced_image() {
        let mut r = ImageRegistry::new();
        r.insert("k", pixel(255, 0, 0));
        let previous = r.insert("k", pixel(0, 255, 0)).expect("displaced");
        assert_eq!(previous.data.as_ref()[0], 255);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn remove_takes_the_entry_out() {
        let mut r = ImageRegistry::new();
        r.insert("k", pixel(1, 2, 3));
        assert!(r.remove("k").is_some());
        assert!(r.remove("k").is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn names_lists_every_key() {
        let mut r = ImageRegistry::new();
        r.insert("a", pixel(0, 0, 0));
        r.insert("b", pixel(0, 0, 0));
        let mut got: Vec<&str> = r.names().collect();
        got.sort_unstable();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn the_shared_empty_registry_is_one_allocation() {
        let a = ImageRegistry::shared_empty();
        let b = ImageRegistry::shared_empty();
        assert!(std::ptr::eq(a, b));
        assert!(a.is_empty());
    }

    #[test]
    fn cloning_an_entry_shares_the_pixels() {
        let mut r = ImageRegistry::new();
        r.insert("k", pixel(9, 9, 9));
        let a = r.get("k").expect("registered").clone();
        let b = r.get("k").expect("registered").clone();
        assert_eq!(a.data.id(), b.data.id());
    }
}
