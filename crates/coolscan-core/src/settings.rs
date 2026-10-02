//! What the operator chooses before a scan

use crate::ScannerInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The film in the holder
///
/// For now this only decides how exposure is metered: a color negative meters
/// each channel on its own to take the orange mask off, everything else keeps
/// the channels together. The saved pixels are never altered by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FilmType {
    #[default]
    Negative,
    Positive,
    Mono,
}

impl FilmType {
    pub const ALL: [FilmType; 3] = [FilmType::Negative, FilmType::Positive, FilmType::Mono];

    pub fn label(self) -> &'static str {
        match self {
            FilmType::Negative => "Color negative",
            FilmType::Positive => "Slide (positive)",
            FilmType::Mono => "B&W negative",
        }
    }
}

/// The frame size on the film
///
/// `Auto` lets the holder say. A 120 holder takes several formats, so there the
/// operator has to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FilmFormat {
    #[default]
    Auto,
    F135,
    F135Half,
    F16,
    F645,
    F66,
    F67,
    F68,
    F69,
}

impl FilmFormat {
    pub const ALL: [FilmFormat; 9] = [
        FilmFormat::Auto,
        FilmFormat::F135,
        FilmFormat::F135Half,
        FilmFormat::F16,
        FilmFormat::F645,
        FilmFormat::F66,
        FilmFormat::F67,
        FilmFormat::F68,
        FilmFormat::F69,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilmFormat::Auto => "Auto (from holder)",
            FilmFormat::F135 => "35mm (24×36)",
            FilmFormat::F135Half => "35mm half frame",
            FilmFormat::F16 => "16mm",
            FilmFormat::F645 => "120 6×4.5",
            FilmFormat::F66 => "120 6×6",
            FilmFormat::F67 => "120 6×7",
            FilmFormat::F68 => "120 6×8",
            FilmFormat::F69 => "120 6×9",
        }
    }

    /// Parse the names the CLI takes: auto, 135, half, 16, 645, 66, 67, 68, 69
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "auto" => FilmFormat::Auto,
            "135" => FilmFormat::F135,
            "half" | "135half" => FilmFormat::F135Half,
            "16" => FilmFormat::F16,
            "645" => FilmFormat::F645,
            "66" => FilmFormat::F66,
            "67" => FilmFormat::F67,
            "68" => FilmFormat::F68,
            "69" => FilmFormat::F69,
            _ => return None,
        })
    }
}

/// Resolutions offered in the UI, highest first. The scanner rounds anything
/// off its own ladder
pub const DPI_CHOICES: [u16; 5] = [4000, 3000, 2000, 1000, 500];

/// Samples per line offered in the UI
pub const SAMPLE_CHOICES: [u8; 5] = [1, 2, 4, 8, 16];

/// Everything one scan run needs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSettings {
    pub film: FilmType,
    pub format: FilmFormat,
    /// Scan resolution; `None` is the scanner's optical resolution
    pub dpi: Option<u16>,
    /// Times each line is read and averaged, 1 to 16
    pub samples: u8,
    pub output_dir: PathBuf,
    /// File names are `<basename>_<frame>.tif`
    pub basename: String,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            film: FilmType::default(),
            format: FilmFormat::default(),
            dpi: None,
            samples: 1,
            output_dir: default_output_dir(),
            basename: "scan".into(),
        }
    }
}

impl ScanSettings {
    /// Check the settings against each other and, when connected, the scanner
    pub fn validate(&self, scanner: Option<&ScannerInfo>) -> Result<(), String> {
        let max_samples = scanner.map_or(16, |s| s.max_samples);
        if self.samples == 0 || self.samples > max_samples {
            return Err(format!("samples must be between 1 and {max_samples}"));
        }
        if let (Some(dpi), Some(s)) = (self.dpi, scanner)
            && !(s.min_dpi..=s.max_dpi).contains(&dpi)
        {
            return Err(format!(
                "{dpi} dpi is outside this scanner's {}–{} dpi",
                s.min_dpi, s.max_dpi
            ));
        }
        let name = self.basename.trim();
        if name.is_empty() {
            return Err("file name must not be empty".into());
        }
        if let Some(bad) = name
            .chars()
            .find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
        {
            return Err(format!("file name must not contain {bad:?}"));
        }
        if self.output_dir.as_os_str().is_empty() {
            return Err("choose an output folder".into());
        }
        Ok(())
    }
}

/// The user's Pictures folder where there is one, else the working directory
fn default_output_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home {
        Some(home) => {
            let pictures = PathBuf::from(home).join("Pictures");
            match pictures.is_dir() {
                true => pictures.join("coolscan"),
                false => PathBuf::from(".").join("scans"),
            }
        }
        None => PathBuf::from(".").join("scans"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> ScannerInfo {
        ScannerInfo {
            product: "LS-9000 ED".into(),
            optical_dpi: 4000,
            min_dpi: 333,
            max_dpi: 4000,
            max_samples: 16,
        }
    }

    #[test]
    fn defaults_are_valid() {
        assert_eq!(ScanSettings::default().validate(Some(&scanner())), Ok(()));
    }

    #[test]
    fn rejects_bad_values() {
        let base = ScanSettings::default();
        let s = ScanSettings { samples: 0, ..base.clone() };
        assert!(s.validate(None).is_err());
        let s = ScanSettings { samples: 17, ..base.clone() };
        assert!(s.validate(None).is_err());
        let s = ScanSettings { dpi: Some(8000), ..base.clone() };
        assert!(s.validate(Some(&scanner())).is_err());
        // Without a scanner the dpi cannot be judged yet
        assert!(s.validate(None).is_ok());
        let s = ScanSettings { basename: " ".into(), ..base.clone() };
        assert!(s.validate(None).is_err());
        let s = ScanSettings { basename: "a/b".into(), ..base };
        assert!(s.validate(None).is_err());
    }

    #[test]
    fn parses_formats() {
        assert_eq!(FilmFormat::parse("67"), Some(FilmFormat::F67));
        assert_eq!(FilmFormat::parse("AUTO"), Some(FilmFormat::Auto));
        assert_eq!(FilmFormat::parse("70"), None);
    }
}
