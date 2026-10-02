//! Turning linear 16-bit samples into something to look at
//!
//! Display only: nothing here touches what gets saved. The image is
//! downsampled, stretched between its 0.1 and 99.9 percentiles, optionally
//! inverted, and gamma-encoded for the screen.

use crate::{Planes, Rgba8};

/// Render `planes` no larger than `max_side` on either side
pub fn render(planes: &Planes, max_side: usize, invert: bool) -> Rgba8 {
    if planes.is_empty() {
        return Rgba8 { width: 0, height: 0, pixels: Vec::new() };
    }
    let step = planes.width.max(planes.height).div_ceil(max_side.max(1)).max(1);
    let (width, height) = (planes.width.div_ceil(step), planes.height.div_ceil(step));
    let channels = planes.planes.len();

    let sampled: Vec<u16> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x * step, y * step)))
        .flat_map(|(x, y)| (0..channels).map(move |c| planes.at(c, x, y)))
        .collect();
    let (low, high) = percentiles(&sampled);
    let span = (high - low).max(1.0);

    let lut: Vec<u8> = (0..=u16::MAX)
        .map(|v| {
            let mut t = ((f32::from(v) - low) / span).clamp(0.0, 1.0);
            if invert {
                t = 1.0 - t;
            }
            (t.powf(1.0 / 2.2) * 255.0).round() as u8
        })
        .collect();

    let mut pixels = Vec::with_capacity(width * height * 4);
    for pixel in sampled.chunks_exact(channels) {
        match pixel {
            [gray] => {
                let g = lut[*gray as usize];
                pixels.extend([g, g, g, 255]);
            }
            [r, g, b, ..] => pixels.extend([lut[*r as usize], lut[*g as usize], lut[*b as usize], 255]),
            [_, _] => unreachable!("two planes are never produced"),
            [] => unreachable!("chunks are never empty"),
        }
    }
    Rgba8 { width, height, pixels }
}

/// The 0.1 and 99.9 percentile values, so a speck of dust cannot set the range
fn percentiles(values: &[u16]) -> (f32, f32) {
    let mut histogram = vec![0u32; 65536];
    for &v in values {
        histogram[v as usize] += 1;
    }
    let total = values.len() as u64;
    let find = |fraction: f64| {
        let target = (total as f64 * fraction) as u64;
        let mut seen = 0u64;
        histogram
            .iter()
            .position(|&count| {
                seen += u64::from(count);
                seen > target
            })
            .unwrap_or(65535) as f32
    };
    (find(0.001), find(0.999))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(width: usize, height: usize) -> Planes {
        let plane: Vec<u16> = (0..width * height).map(|i| (i * 65535 / (width * height - 1)) as u16).collect();
        Planes { width, height, stride: width, planes: vec![plane.clone(), plane.clone(), plane] }
    }

    #[test]
    fn fits_and_stretches() {
        let out = render(&ramp(1000, 400), 250, false);
        assert!(out.width <= 250 && out.height <= 250);
        assert_eq!(out.pixels.len(), out.width * out.height * 4);
        assert_eq!(out.pixels[0], 0);
        assert_eq!(out.pixels[out.pixels.len() - 4], 255);
    }

    #[test]
    fn inverts() {
        let out = render(&ramp(100, 10), 100, true);
        assert_eq!(out.pixels[0], 255);
        assert_eq!(out.pixels[out.pixels.len() - 4], 0);
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(render(&Planes::default(), 100, false).pixels.len(), 0);
    }
}
