use crate::Result;
use std::io::Cursor;

const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;

/// Rotate pixels clockwise before passing them to a portable recognizer.
pub fn rotate_ocr_png(input: &[u8], angle: u16) -> Result<Vec<u8>> {
    let invalid = || "Некоректне або завелике зображення для повороту OCR.".to_owned();
    if !matches!(angle, 90 | 180 | 270) || input.len() > MAX_PNG_BYTES {
        return Err(invalid());
    }
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(input),
        png::Limits {
            bytes: MAX_PNG_BYTES,
        },
    );
    let dimensions = decoder.read_header_info().map_err(|_| invalid())?;
    let (width, height) = (dimensions.width as usize, dimensions.height as usize);
    if !(1..=4096).contains(&width) || !(1..=4096).contains(&height) {
        return Err(invalid());
    }
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| invalid())?;
    let size = reader
        .output_buffer_size()
        .filter(|&size| size <= MAX_PNG_BYTES)
        .ok_or_else(invalid)?;
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels).map_err(|_| invalid())?;
    drop(reader);
    let channels = info.color_type.samples();
    if info.width as usize != width
        || info.height as usize != height
        || info.line_size != width * channels
    {
        return Err(invalid());
    }
    let (new_width, new_height) = if angle == 180 {
        (width, height)
    } else {
        (height, width)
    };
    let mut rotated = vec![0; info.buffer_size()];
    for y in 0..height {
        for x in 0..width {
            let (right, down) = match angle {
                90 => (height - 1 - y, x),
                180 => (width - 1 - x, height - 1 - y),
                _ => (y, width - 1 - x),
            };
            let source = y * info.line_size + x * channels;
            let target = (down * new_width + right) * channels;
            rotated[target..target + channels].copy_from_slice(&pixels[source..source + channels]);
        }
    }
    drop(pixels);
    let mut result = Vec::new();
    let mut encoder = png::Encoder::new(&mut result, new_width as u32, new_height as u32);
    encoder.set_color(info.color_type);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&rotated))
        .map_err(|_| invalid())?;
    if result.len() > MAX_PNG_BYTES {
        return Err(invalid());
    }
    Ok(result)
}
