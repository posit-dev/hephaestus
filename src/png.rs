//! PNG reader and writer.
//!
//! Aliases for the PNG entry points in [`crate::image`], the module that
//! carries every raster codec.

pub use crate::image::{
    decode_png, encode_png, encode_png_dpi, read_png, read_png_from, write_png, write_png_dpi,
    write_png_dpi_to, write_png_to,
};

#[cfg(test)]
mod tests {
    #[test]
    fn png_entry_points_are_reachable_from_this_module() {
        let pixels = vec![255u8; 2 * 2 * 4];
        let bytes = crate::png::encode_png(2, 2, &pixels).expect("encode");
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        let image = crate::png::decode_png(&bytes).expect("decode");
        assert_eq!((image.width, image.height), (2, 2));
    }
}
