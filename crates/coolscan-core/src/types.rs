//! Plain data passed between the backend, the worker and the UI

use crate::FilmFormat;
use std::path::PathBuf;

/// A scanner the backend can see
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Where it is attached, e.g. `/dev/sg3` or `\\.\Scanner0`. This is what
    /// [`Backend::connect`](crate::Backend::connect) takes
    pub location: String,
    /// Vendor and product as the unit names itself
    pub description: String,
    /// False where another program holds the device
    pub available: bool,
}

/// What a connected scanner can do, as far as the settings care
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannerInfo {
    pub product: String,
    pub optical_dpi: u16,
    pub min_dpi: u16,
    pub max_dpi: u16,
    pub max_samples: u8,
}

/// Which frame formats a holder takes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderFormats {
    /// The holder fixes the format, e.g. the FH-869GR's mask
    Fixed(FilmFormat),
    /// Any of these, chosen by the operator
    Choices(Vec<FilmFormat>),
    /// A holder nkscan has no table for
    Unknown,
}

/// The film holder that is loaded
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderInfo {
    pub name: String,
    pub formats: HolderFormats,
}

impl HolderInfo {
    /// Whether `format` may be scanned in this holder
    ///
    /// Only formats the holder table lists are allowed: a frame length the
    /// holder was never meant for is not something to try on the hardware.
    /// `Auto` is allowed where the holder fixes the format.
    pub fn check_format(&self, format: FilmFormat) -> Result<(), String> {
        let list = |formats: &[FilmFormat]| {
            formats.iter().map(|f| f.label()).collect::<Vec<_>>().join(", ")
        };
        match (&self.formats, format) {
            (HolderFormats::Fixed(_), FilmFormat::Auto) => Ok(()),
            (HolderFormats::Fixed(fixed), f) if *fixed == f => Ok(()),
            (HolderFormats::Fixed(fixed), f) => Err(format!(
                "the {} only takes {}, not {}",
                self.name,
                fixed.label(),
                f.label()
            )),
            (HolderFormats::Choices(choices), FilmFormat::Auto) => Err(format!(
                "the {} takes several formats, so choose one: {}",
                self.name,
                list(choices)
            )),
            (HolderFormats::Choices(choices), f) if choices.contains(&f) => Ok(()),
            (HolderFormats::Choices(choices), f) => Err(format!(
                "the {} does not take {}; it takes {}",
                self.name,
                f.label(),
                list(choices)
            )),
            (HolderFormats::Unknown, _) => Ok(()),
        }
    }

    /// Whether the UI should offer `format` for this holder
    pub fn offers(&self, format: FilmFormat) -> bool {
        self.check_format(format).is_ok()
    }
}

/// Image samples, one buffer per channel, row-major
///
/// `stride` can exceed `width`: a pass the unit padded past the end of the
/// film keeps its buffers as they arrived, and only `width` columns are image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Planes {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// Red, green, blue in that order, or a single gray plane
    pub planes: Vec<Vec<u16>>,
}

impl Planes {
    /// Sample at `(x, y)` of `plane`
    #[inline]
    pub fn at(&self, plane: usize, x: usize, y: usize) -> u16 {
        self.planes[plane][y * self.stride + x]
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.planes.is_empty()
    }
}

/// Where one detected frame sits on the strip thumbnail, in thumbnail columns
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBox {
    pub start: usize,
    pub end: usize,
}

/// What frame discovery found on the loaded film
#[derive(Debug, Clone, Default)]
pub struct Strip {
    /// The overview pass, linear samples. `None` where the holder publishes
    /// its frames and no thumbnail pass was taken
    pub thumbnail: Option<Planes>,
    /// One entry per detected frame; `None` where the frame cannot be placed on
    /// the thumbnail
    pub frames: Vec<Option<FrameBox>>,
}

/// One scanned frame, exactly as the scanner delivered it
#[derive(Debug, Clone)]
pub struct Image {
    pub planes: Planes,
    pub dpi: u32,
    /// False where the unit gave less than the pass promised
    pub complete: bool,
}

/// Which pass a progress report belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The overview pass frame detection reads
    Thumbnail,
    /// Exposure metering, counting passes from one
    Meter(usize),
    /// The frame itself
    Scan,
}

/// How far a pass has got, in bytes. `total` is 0 until the first chunk arrives
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32)
    }
}

/// An 8-bit RGBA picture for showing on screen
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8 {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// A frame that was written to disk
#[derive(Debug, Clone)]
pub struct SavedFrame {
    /// Zero-based index into the strip's frames
    pub index: usize,
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    pub dpi: u32,
    /// Small display rendering of what was saved
    pub preview: Rgba8,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_holder() -> HolderInfo {
        HolderInfo {
            name: "FH-869S".into(),
            formats: HolderFormats::Choices(vec![FilmFormat::F66, FilmFormat::F67, FilmFormat::F69]),
        }
    }

    #[test]
    fn a_strip_holder_takes_only_its_listed_formats() {
        let h = strip_holder();
        assert!(h.check_format(FilmFormat::F67).is_ok());
        assert!(h.check_format(FilmFormat::F645).is_err());
        assert!(h.check_format(FilmFormat::F135).is_err());
        // Several formats fit, so the operator has to say which
        assert!(h.check_format(FilmFormat::Auto).is_err());
    }

    #[test]
    fn a_masked_holder_fixes_its_format() {
        let h = HolderInfo { name: "FH-869GR".into(), formats: HolderFormats::Fixed(FilmFormat::F645) };
        assert!(h.check_format(FilmFormat::Auto).is_ok());
        assert!(h.check_format(FilmFormat::F645).is_ok());
        assert!(h.check_format(FilmFormat::F69).is_err());
    }
}
