//! Named raster images, keyed by whatever string a caller names them with.
//!
//! An [`ImageRegistry`] is a name-keyed in-memory map from a string to a
//! decoded [`Image`]. An [`ImageGeom`](crate::plot::geom::ImageGeom)'s
//! `"image"` channel carries names rather than pixels, so a discrete scale can
//! map a category to an image the same way it maps one to a colour, and one
//! registry entry reused across many rows is one image as far as a backend's
//! texture atlas is concerned. A markdown `![](name)` tag resolves the same
//! way, which is what puts images in rich text.
//!
//! # Registered names and locations
//!
//! [`ImageRegistry::get`] answers for registered entries alone.
//! [`ImageRegistry::resolve`] falls back to reading the name as a location —
//! a filesystem path, or an `http(s)` URL with the `image-url` feature — and
//! caches what it finds, so `![](logo.png)` needs no registration at all.
//! Which files that can read follows from the codecs compiled in: a name this
//! build cannot decode resolves to nothing.
//!
//! Registration wins over the location, which is what lets a build with no
//! filesystem — a plot rebuilt from a document in a browser — serve the same
//! names a native build read from disk.
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
use std::sync::Mutex;

use crate::brush::Image;

/// In-memory map from name to [`Image`].
///
/// Typical usage: build one at setup, register every image the plot needs,
/// hand it to [`Plot::image_registry`](crate::plot::Plot::image_registry), and
/// let the geom look names up at draw time. Names that are locations need no
/// setup at all — see [`Self::resolve`].
///
/// [`Image`] is blob-backed, so cloning a registry — or an entry out of one —
/// shares the pixels rather than copying them.
#[derive(Debug, Default)]
pub struct ImageRegistry {
    images: HashMap<String, Image>,
    /// Names already read from a location, with `None` recording one that
    /// could not be read. Separate from `images` so registration stays the
    /// only thing [`ImageRegistry::get`] and [`ImageRegistry::len`] report,
    /// and behind a lock so resolution works through a shared borrow.
    loaded: Mutex<HashMap<String, Option<Image>>>,
}

impl Clone for ImageRegistry {
    fn clone(&self) -> Self {
        Self {
            images: self.images.clone(),
            loaded: Mutex::new(self.loaded.lock().map(|m| m.clone()).unwrap_or_default()),
        }
    }
}

impl ImageRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared registry, built once per process. Draw contexts that no caller
    /// supplied a registry to read from this rather than allocating one per
    /// call. It holds no registered entries, so a name only resolves through
    /// it if it names a location.
    pub fn shared_empty() -> &'static ImageRegistry {
        static SHARED: std::sync::OnceLock<ImageRegistry> = std::sync::OnceLock::new();
        SHARED.get_or_init(ImageRegistry::new)
    }

    /// Insert an image under the given name. Returns the previous image if one
    /// existed.
    pub fn insert(&mut self, name: impl Into<String>, image: Image) -> Option<Image> {
        self.images.insert(name.into(), image)
    }

    /// Look up a registered image by name. Locations are not consulted — use
    /// [`Self::resolve`] for that.
    pub fn get(&self, name: &str) -> Option<&Image> {
        self.images.get(name)
    }

    /// Look up an image by name, reading it from the location the name spells
    /// if nothing is registered under it.
    ///
    /// A registered entry always wins. Otherwise the name is read as a
    /// filesystem path — or, with the `image-url` feature, fetched when it
    /// starts with `http://` or `https://` — and decoded with whichever
    /// codecs this build has. Both the image and the failure to find one are
    /// remembered, so a name costs at most one read per process.
    pub fn resolve(&self, name: &str) -> Option<Image> {
        if let Some(image) = self.images.get(name) {
            return Some(image.clone());
        }
        let mut loaded = self.loaded.lock().expect("image registry poisoned");
        if let Some(hit) = loaded.get(name) {
            return hit.clone();
        }
        let image = load::load(name);
        loaded.insert(name.to_string(), image.clone());
        image
    }

    /// Whether an image with the given name is registered. Locations are not
    /// consulted.
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

    /// Names [`Self::resolve`] has read from a location, in unspecified order.
    /// Names it failed to read are not reported.
    ///
    /// Owned rather than borrowed because the entries live behind a lock. What
    /// consumes it is the document writer, which embeds these alongside the
    /// registered entries so a reader without the original files still has
    /// the pixels.
    pub fn loaded_names(&self) -> Vec<String> {
        let loaded = self.loaded.lock().expect("image registry poisoned");
        loaded
            .iter()
            .filter(|(_, image)| image.is_some())
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Number of registered images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether the registry has no registered entries.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// An empty register behind an `Arc`, for tests that have to pass one
/// but name no images.
#[cfg(test)]
pub(crate) fn no_images() -> std::sync::Arc<ImageRegistry> {
    std::sync::Arc::new(ImageRegistry::new())
}

/// Reading a name that spells a location, with a process-global memo so two
/// registries naming one file read and decode it once.
mod load {
    use super::*;

    /// Every name already read this process, `None` for one that could not
    /// be. Global rather than per-registry so the six plots of a composition
    /// naming one logo share the decode and the pixels.
    fn cache() -> &'static Mutex<HashMap<String, Option<Image>>> {
        static CACHE: std::sync::OnceLock<Mutex<HashMap<String, Option<Image>>>> =
            std::sync::OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Read `name` as a location, or return the memoized outcome of having
    /// tried before.
    pub(super) fn load(name: &str) -> Option<Image> {
        let mut cache = cache().lock().expect("image cache poisoned");
        if let Some(hit) = cache.get(name) {
            return hit.clone();
        }
        let image = read(name);
        cache.insert(name.to_string(), image.clone());
        image
    }

    /// The read itself: a URL fetch when the name is one and this build can,
    /// otherwise a filesystem path.
    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    fn read(name: &str) -> Option<Image> {
        if is_url(name) {
            return fetch(name).and_then(|bytes| crate::image::decode_image(&bytes).ok());
        }
        crate::image::read_image(name).ok()
    }

    /// Nothing is readable in a build with no codec, so every name that isn't
    /// registered resolves to nothing.
    #[cfg(not(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp")))]
    fn read(_name: &str) -> Option<Image> {
        None
    }

    /// Whether the name is an `http(s)` URL rather than a path.
    // A build with no codec cannot decode what a location holds, so its
    // `read` never asks either of these anything.
    #[allow(dead_code)]
    fn is_url(name: &str) -> bool {
        name.starts_with("http://") || name.starts_with("https://")
    }

    /// Fetch a URL's bytes. Synchronous, like the `google-fonts` lookup this
    /// mirrors.
    #[cfg(feature = "image-url")]
    #[allow(dead_code)]
    fn fetch(url: &str) -> Option<Vec<u8>> {
        use std::io::Read;

        const HTTP_TIMEOUT_SECS: u64 = 30;
        /// Ceiling on a fetched image, so a wrong URL cannot exhaust memory.
        const MAX_BYTES: u64 = 64 * 1024 * 1024;

        let response = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .get(url)
            .call()
            .ok()?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Without the `image-url` feature a URL is simply not a location this
    /// build can read.
    #[cfg(not(feature = "image-url"))]
    #[allow(dead_code)]
    fn fetch(_url: &str) -> Option<Vec<u8>> {
        None
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
