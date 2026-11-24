use crate::Result;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SampleBox {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Serialize)]
pub struct SampledColors {
    pub background: u32,
    pub ink: Option<u32>,
    #[serde(rename = "maskSafe")]
    pub mask_safe: bool,
}

pub fn sample_backgrounds(
    pixels: &[u8],
    width: usize,
    height: usize,
    boxes: &[SampleBox],
) -> Result<Vec<u32>> {
    validate_image(pixels, width, height, boxes)?;
    Ok(boxes
        .iter()
        .map(|bounds| background(pixels, width, height, bounds).0)
        .collect())
}

pub fn sample_colors(
    pixels: &[u8],
    width: usize,
    height: usize,
    boxes: &[SampleBox],
) -> Result<Vec<SampledColors>> {
    validate_image(pixels, width, height, boxes)?;
    let mut samples = Vec::with_capacity(48 * 24);
    Ok(boxes
        .iter()
        .map(|bounds| {
            let (background, mask_safe) = background(pixels, width, height, bounds);
            let ink = if valid(bounds) {
                ink(pixels, width, height, bounds, background, &mut samples)
            } else {
                None
            };
            SampledColors { background, ink, mask_safe }
        })
        .collect())
}

fn validate_image(pixels: &[u8], width: usize, height: usize, boxes: &[SampleBox]) -> Result<()> {
    if !(1..=1024).contains(&width)
        || !(1..=1024).contains(&height)
        || pixels.len() != width * height * 4
        || boxes.len() > 32_768
    {
        return Err("Некоректне зображення або координати блоків PDF.".into());
    }
    Ok(())
}

fn valid(bounds: &SampleBox) -> bool {
    [bounds.x, bounds.y, bounds.width, bounds.height]
        .into_iter()
        .all(f64::is_finite)
        && bounds.width > 0.0
        && bounds.height > 0.0
}

fn background(pixels: &[u8], width: usize, height: usize, bounds: &SampleBox) -> (u32, bool) {
    if !valid(bounds) {
        return (0xff_ff_ff, false);
    }
    let mut samples = [0u64; 40];
    let sample_at = |x: f64, y: f64| {
        let x = ((x * width as f64) as usize).min(width - 1);
        let y = ((y * height as f64) as usize).min(height - 1);
        let start = (y * width + x) * 4;
        let [red, green, blue] =
            [pixels[start], pixels[start + 1], pixels[start + 2]].map(u64::from);
        let bucket = ((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3);
        (bucket << 24) | (red << 16) | (green << 8) | blue
    };
    for (index, sample) in samples.iter_mut().enumerate() {
        let x = bounds.x + bounds.width * ((index % 8) as f64 + 0.5) / 8.0;
        let y = bounds.y + bounds.height * ((index / 8) as f64 + 0.5) / 5.0;
        *sample = sample_at(x, y);
    }
    // Tight glyph boxes can contain more ink than paper. Prefer a color that
    // dominates on both sides of the writing and also occurs inside the box.
    let horizontal = bounds.width * width as f64 >= bounds.height * height as f64;
    let (extent, lo, hi) = if horizontal {
        (height, bounds.y, bounds.y + bounds.height)
    } else {
        (width, bounds.x, bounds.x + bounds.width)
    };
    let margin = 1.5 / extent as f64;
    if lo >= margin && hi + margin < 1.0 {
        let sides = [lo - margin, hi + margin].map(|edge| {
            let mut colors = std::array::from_fn::<_, 8, _>(|index| {
                let offset = (index as f64 + 0.5) / 8.0;
                if horizontal {
                    sample_at(bounds.x + bounds.width * offset, edge)
                } else {
                    sample_at(edge, bounds.y + bounds.height * offset)
                }
            });
            colors.sort_unstable();
            colors
        });
        let paper = sides[0][4];
        let matches_paper = |sample: &&u64| {
            [0, 8, 16]
                .into_iter()
                .all(|shift| ((**sample >> shift) & 255).abs_diff((paper >> shift) & 255) <= 32)
        };
        if sides
            .iter()
            .all(|side| side.iter().filter(matches_paper).count() >= 6)
            && samples.iter().filter(matches_paper).count() >= 4
        {
            return ((paper & 0xff_ff_ff) as u32, true);
        }
    }

    samples.sort_unstable();
    let mut best = (0, 0);
    let mut start = 0;
    while start < samples.len() {
        let end =
            start + samples[start..].partition_point(|sample| sample >> 24 == samples[start] >> 24);
        if end - start >= best.0 {
            best = (end - start, (start + end) / 2);
        }
        start = end;
    }
    let paper = samples[best.1];
    // A narrow box next to a border may have no usable paper samples outside it.
    // A solid panel still has a consistent color inside the text box.
    let uniform_inside = samples.iter().filter(|&&sample| {
        [0, 8, 16].into_iter()
            .all(|shift| ((sample >> shift) & 255).abs_diff((paper >> shift) & 255) <= 16)
    }).count() >= 30;
    ((paper & 0xff_ff_ff) as u32, uniform_inside)
}

fn ink(
    pixels: &[u8],
    width: usize,
    height: usize,
    bounds: &SampleBox,
    paper: u32,
    samples: &mut Vec<u64>,
) -> Option<u32> {
    let x = (bounds.x * width as f64) as usize;
    let y = (bounds.y * height as f64) as usize;
    let right = (((bounds.x + bounds.width) * width as f64).ceil() as usize).min(width);
    let bottom = (((bounds.y + bounds.height) * height as f64).ceil() as usize).min(height);
    if x >= right || y >= bottom {
        return None;
    }
    let x_step = (right - x).div_ceil(48);
    let y_step = (bottom - y).div_ceil(24);
    let distance = |color: u32| {
        [0, 8, 16]
            .into_iter()
            .map(|shift| ((color >> shift) & 255).abs_diff((paper >> shift) & 255))
            .max()
            .unwrap_or(0)
    };
    samples.clear();
    let mut total = 0;
    for row in (y..bottom).step_by(y_step) {
        for column in (x..right).step_by(x_step) {
            total += 1;
            let index = (row * width + column) * 4;
            let [r, g, b] = [pixels[index], pixels[index + 1], pixels[index + 2]].map(u32::from);
            let color = r << 16 | g << 8 | b;
            if distance(color) >= 56 {
                let cluster = (r >> 4) << 8 | (g >> 4) << 4 | (b >> 4);
                samples.push(u64::from(cluster) << 24 | u64::from(color));
            }
        }
    }
    samples.sort_unstable();
    let minimum = 3.max(total / 50);
    let mut remaining = samples.as_slice();
    let mut best = None;
    while let Some(first) = remaining.first() {
        let count = remaining.partition_point(|sample| sample >> 24 == first >> 24);
        if count >= minimum {
            let color = (remaining[count / 2] & 0xff_ff_ff) as u32;
            let score = count * distance(color) as usize;
            if best.is_none_or(|(previous, _)| score > previous) {
                best = Some((score, color));
            }
        }
        remaining = &remaining[count..];
    }
    best.map(|(_, color)| color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_dense_ink_out_of_paper_masks_in_both_directions() {
        let width = 24;
        let height = 24;
        for vertical in [false, true] {
            let mut pixels = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    let (along, across) = if vertical { (y, x) } else { (x, y) };
                    let rgb = if (9..15).contains(&across) && (2..22).contains(&along) {
                        if (5..19).contains(&along) {
                            [16, 18, 15]
                        } else {
                            // Tight boxes can contain only antialiased paper samples.
                            [220, 228, 186]
                        }
                    } else {
                        [234, 247, 208]
                    };
                    pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
            let (x, y, w, h) = if vertical {
                (9, 2, 6, 20)
            } else {
                (2, 9, 20, 6)
            };
            let bounds = SampleBox {
                x: f64::from(x) / 24.0,
                y: f64::from(y) / 24.0,
                width: f64::from(w) / 24.0,
                height: f64::from(h) / 24.0,
            };
            let colors = sample_colors(&pixels, width, height, &[bounds]).unwrap();
            assert_eq!(colors[0].background, 0xea_f7_d0);
            assert_eq!(colors[0].ink, Some(0x10_12_0f));
        }
    }

    #[test]
    fn distinguishes_colored_ink_light_ink_and_blank_paper() {
        let mut pixels = Vec::new();
        for y in 0..20 {
            for x in 0..60 {
                let stroke = (5..15).contains(&y) && x % 20 < 5;
                let rgb = match (x / 20, stroke) {
                    (0, true) => [22, 69, 146],
                    (1, true) => [246, 238, 217],
                    (1, false) => [38, 32, 65],
                    _ => [243, 250, 216],
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let boxes = [0, 1, 2].map(|column| SampleBox {
            x: f64::from(column) / 3.0,
            y: 0.0,
            width: 1.0 / 3.0,
            height: 1.0,
        });
        let colors = sample_colors(&pixels, 60, 20, &boxes).unwrap();
        assert_eq!(colors[0].background, 0xf3_fa_d8);
        assert_eq!(colors[0].ink, Some(0x16_45_92));
        assert_eq!(colors[1].background, 0x26_20_41);
        assert_eq!(colors[1].ink, Some(0xf6_ee_d9));
        assert_eq!(colors[2].ink, None);
        assert!(colors.iter().all(|color| color.mask_safe));
    }

    #[test]
    fn only_marks_solid_backgrounds_safe_for_an_overlay() {
        let (width, height) = (96, 32);
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let rgb = if (13..18).contains(&y) && x % 8 < 3 {
                    [12, 12, 12]
                } else if x < 48 {
                    [240, 245, 222]
                } else {
                    [30 + ((x - 48) * 4) as u8, 105, 175]
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let boxes = [0.025, 0.525].map(|x| SampleBox { x, y: 0.34, width: 0.45, height: 0.27 });
        let colors = sample_colors(&pixels, width, height, &boxes).unwrap();
        assert!(colors[0].mask_safe);
        assert!(!colors[1].mask_safe);
    }

    #[test]
    fn samples_each_background_without_using_text_ink() {
        let mut rgba = Vec::new();
        for row in 0..10 {
            for column in 0..16 {
                let rgb = if row == 3 {
                    [15, 17, 13]
                } else if column < 8 {
                    [230, 246, 202]
                } else {
                    [255, 255, 255]
                };
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let boxes = [
            SampleBox {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            },
            SampleBox {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            },
        ];
        assert_eq!(
            sample_backgrounds(&rgba, 16, 10, &boxes).unwrap(),
            [0xe6_f6_ca, 0xff_ff_ff]
        );
    }
}
