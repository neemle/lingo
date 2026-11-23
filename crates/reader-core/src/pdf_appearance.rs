use crate::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct SampleBox {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

pub fn sample_backgrounds(pixels: &[u8], width: usize, height: usize, boxes: &[SampleBox]) -> Result<Vec<u32>> {
    if !(1..=1024).contains(&width) || !(1..=1024).contains(&height) ||
        pixels.len() != width * height * 4 || boxes.len() > 32_768 {
        return Err("Некоректне зображення або координати блоків PDF.".into());
    }
    Ok(boxes.iter().map(|bounds| {
        if ![bounds.x, bounds.y, bounds.width, bounds.height].into_iter().all(f64::is_finite) ||
            bounds.width <= 0.0 || bounds.height <= 0.0 {
            return 0xff_ff_ff;
        }
        let mut samples = [0u64; 40];
        for (index, sample) in samples.iter_mut().enumerate() {
            let x = bounds.x + bounds.width * ((index % 8) as f64 + 0.5) / 8.0;
            let y = bounds.y + bounds.height * ((index / 8) as f64 + 0.5) / 5.0;
            let x = ((x * width as f64) as usize).min(width - 1);
            let y = ((y * height as f64) as usize).min(height - 1);
            let start = (y * width + x) * 4;
            let [red, green, blue] = [pixels[start], pixels[start + 1], pixels[start + 2]].map(u64::from);
            let bucket = ((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3);
            *sample = (bucket << 24) | (red << 16) | (green << 8) | blue;
        }
        samples.sort_unstable();
        // Return a measured color from the dominant cluster of nearby raster colors.
        let mut best = (0, 0);
        let mut start = 0;
        while start < samples.len() {
            let end = start + samples[start..].partition_point(|sample| sample >> 24 == samples[start] >> 24);
            if end - start >= best.0 {
                best = (end - start, (start + end) / 2);
            }
            start = end;
        }
        (samples[best.1] & 0xff_ff_ff) as u32
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_each_background_without_using_text_ink() {
        let mut rgba = Vec::new();
        for row in 0..10 {
            for column in 0..16 {
                let rgb = if row == 3 { [15, 17, 13] }
                    else if column < 8 { [230, 246, 202] } else { [255, 255, 255] };
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let boxes = [
            SampleBox { x: 0.0, y: 0.0, width: 0.5, height: 1.0 },
            SampleBox { x: 0.5, y: 0.0, width: 0.5, height: 1.0 },
        ];
        assert_eq!(sample_backgrounds(&rgba, 16, 10, &boxes).unwrap(), [0xe6_f6_ca, 0xff_ff_ff]);
    }
}
