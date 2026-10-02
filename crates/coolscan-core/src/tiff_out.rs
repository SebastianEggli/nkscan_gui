//! Writing a scan to disk as a 16-bit TIFF, samples untouched
//!
//! Linear, uncompressed, no color profile: the file holds what the scanner
//! delivered and nothing else. Modeled on nkscan's CLI writer
//! (`src/bin/nkscan/io.rs`, MIT OR Apache-2.0).

use crate::Planes;
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
};
use tiff::{
    encoder::{
        Rational, TiffEncoder,
        colortype::{ColorType, Gray16, RGB16},
    },
    tags::ResolutionUnit,
};

/// `<dir>/<basename>_<frame>.tif`, with a counter added rather than
/// overwriting a file that is already there. `frame` counts from one
pub fn frame_path(dir: &Path, basename: &str, frame: usize) -> PathBuf {
    let stem = format!("{}_{frame:02}", basename.trim());
    let first = dir.join(format!("{stem}.tif"));
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| dir.join(format!("{stem}_{n}.tif")))
        .find(|p| !p.exists())
        .expect("the range is unbounded")
}

/// Write `planes` to `path`: three planes as RGB, one as gray
pub fn write_tiff(path: &Path, planes: &Planes, dpi: u32) -> Result<()> {
    if planes.is_empty() {
        bail!("nothing to write: the image is empty");
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    match planes.planes.len() {
        3 => write::<RGB16>(path, planes, dpi),
        1 => write::<Gray16>(path, planes, dpi),
        n => bail!("{n} planes is not a TIFF this writes"),
    }
    .with_context(|| format!("writing {}", path.display()))
}

fn write<C: ColorType<Inner = u16>>(path: &Path, planes: &Planes, dpi: u32) -> Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut tiff = TiffEncoder::new(file)?;
    let mut image = tiff.new_image::<C>(planes.width as u32, planes.height as u32)?;
    image.resolution(ResolutionUnit::Inch, Rational { n: dpi.max(1), d: 1 });

    // Interleave one strip at a time, so the whole image is never copied
    let per_pixel = planes.planes.len();
    let mut strip = Vec::new();
    let mut done = 0usize;
    while image.next_strip_sample_count() > 0 {
        let count = image.next_strip_sample_count() as usize;
        let first = done / per_pixel;
        strip.clear();
        strip.reserve(count);
        for pixel in first..first + count / per_pixel {
            let (x, y) = (pixel % planes.width, pixel / planes.width);
            strip.extend((0..per_pixel).map(|p| planes.at(p, x, y)));
        }
        image.write_strip(&strip)?;
        done += count;
    }
    image.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::decoder::{Decoder, DecodingResult};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coolscan-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_rgb_and_skips_padding() {
        // Five image columns in buffers eight wide, each sample naming its source
        let (width, height, stride) = (5usize, 3usize, 8usize);
        let planes: Vec<Vec<u16>> = (0..3u16)
            .map(|c| {
                (0..height * stride)
                    .map(|i| match i % stride < width {
                        true => c * 1000 + i as u16,
                        false => 0,
                    })
                    .collect()
            })
            .collect();
        let image = Planes { width, height, stride, planes };
        let dir = temp_dir("rgb");
        let path = dir.join("t.tif");
        write_tiff(&path, &image, 4000).unwrap();

        let mut decoder = Decoder::new(std::io::BufReader::new(File::open(&path).unwrap())).unwrap();
        assert_eq!(decoder.dimensions().unwrap(), (width as u32, height as u32));
        let DecodingResult::U16(read) = decoder.read_image().unwrap() else {
            panic!("not 16-bit")
        };
        let want: Vec<u16> = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| (0..3u16).map(move |c| c * 1000 + (y * stride + x) as u16)))
            .collect();
        assert_eq!(read, want);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn writes_gray() {
        let image = Planes { width: 2, height: 2, stride: 2, planes: vec![vec![0, 1, 65534, 65535]] };
        let dir = temp_dir("gray");
        let path = dir.join("g.tif");
        write_tiff(&path, &image, 1000).unwrap();
        let mut decoder = Decoder::new(std::io::BufReader::new(File::open(&path).unwrap())).unwrap();
        let DecodingResult::U16(read) = decoder.read_image().unwrap() else {
            panic!("not 16-bit")
        };
        assert_eq!(read, vec![0, 1, 65534, 65535]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn never_overwrites() {
        let dir = temp_dir("names");
        let first = frame_path(&dir, "scan", 3);
        assert_eq!(first.file_name().unwrap(), "scan_03.tif");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(frame_path(&dir, "scan", 3).file_name().unwrap(), "scan_03_1.tif");
        std::fs::remove_dir_all(dir).ok();
    }
}
