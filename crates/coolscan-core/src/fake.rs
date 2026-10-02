//! A simulated scanner, for working on the app without hardware
//!
//! It models an LS-9000 with an FH-869S strip holder loaded. The holder has a
//! fixed travel, so how many frames fit depends on the format: shorter frames,
//! more of them. Frames are placed the way nkscan places them, as rectangles
//! in feed addresses (4000 per inch), and go through the same format and travel
//! checks as the real backend.
//!
//! The travel is a stand-in, not a measured value: nkscan reads the real one
//! off the scanner at runtime, and Nikon does not publish it per holder.

use crate::{
    Backend, Cancelled, DeviceInfo, FilmFormat, Flow, FrameBox, HolderFormats, HolderInfo, Image,
    Phase, Planes, Progress, ScanSettings, ScannerInfo, Strip, nkscan_backend::check_feed,
};
use anyhow::{anyhow, bail};
use std::{thread, time::Duration};

const LOCATION: &str = "demo:0";

/// Feed addresses per inch, the scanner's optical resolution
const OPTICAL_DPI: u32 = 4000;
/// Where the holder's travel starts and ends, in feed addresses. About 190 mm
const TRAVEL: (u32, u32) = (0, 29_900);
/// The longest window the scanner takes, as its Y boundary would say. A 6x9
/// frame and a little margin
const BOUNDARY: u32 = 14_000;
/// Unexposed film between two frames, about 3 mm
const GAP: u32 = 470;
/// Feed addresses per thumbnail column
const PER_COLUMN: u32 = 60;
/// Thumbnail rows, across the film
const ROWS: usize = 120;

/// A scanner that is not there
pub struct FakeBackend {
    connected: bool,
    loaded: bool,
    /// Where each frame of the last discovery sits, in feed addresses
    frames: Vec<(u32, u32)>,
    /// How long a simulated pass takes; zero in tests
    pass_time: Duration,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            connected: false,
            loaded: true,
            frames: Vec::new(),
            pass_time: Duration::from_millis(800),
        }
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Passes finish instantly, for tests
    pub fn instant() -> Self {
        Self {
            pass_time: Duration::ZERO,
            ..Self::default()
        }
    }

    fn holder() -> HolderInfo {
        // nkscan's table for the FH-869S
        HolderInfo {
            name: "FH-869S".into(),
            formats: HolderFormats::Choices(vec![FilmFormat::F66, FilmFormat::F67, FilmFormat::F69]),
        }
    }

    /// Report progress over `pass_time`, stopping when told to
    fn run_pass(&self, phase: Phase, on: &mut dyn FnMut(Phase, Progress) -> Flow) -> anyhow::Result<()> {
        const STEPS: u64 = 20;
        for step in 0..=STEPS {
            if on(phase, Progress { done: step, total: STEPS }).is_break() {
                return Err(Cancelled.into());
            }
            if !self.pass_time.is_zero() {
                thread::sleep(self.pass_time / STEPS as u32);
            }
        }
        Ok(())
    }

    fn check(&self) -> anyhow::Result<()> {
        match (self.connected, self.loaded) {
            (false, _) => bail!("no scanner connected"),
            (_, false) => bail!("no film loaded"),
            _ => Ok(()),
        }
    }
}

/// Frame length along the feed, in addresses at 4000 dpi; nkscan's gate sizes
fn length(format: FilmFormat) -> Option<u32> {
    Some(match format {
        FilmFormat::Auto => return None,
        FilmFormat::F135 => 5959,
        FilmFormat::F135Half => 2835,
        FilmFormat::F16 => 3150,
        FilmFormat::F645 => 6696,
        FilmFormat::F66 => 8964,
        FilmFormat::F67 => 10945,
        FilmFormat::F68 => 11969,
        FilmFormat::F69 => 13176,
    })
}

/// Where the frames of `length` sit along the travel: as many whole frames
/// as fit, each followed by a gap
pub(crate) fn place_frames(length: u32) -> Vec<(u32, u32)> {
    let (start, last) = TRAVEL;
    let mut frames = Vec::new();
    let mut top = start + GAP;
    while top + length <= last {
        frames.push((top, top + length));
        top += length + GAP;
    }
    frames
}

impl Backend for FakeBackend {
    fn list_devices(&mut self) -> anyhow::Result<Vec<DeviceInfo>> {
        Ok(vec![DeviceInfo {
            location: LOCATION.into(),
            description: "Demo LS-9000 ED (simulated)".into(),
            available: true,
        }])
    }

    fn connect(&mut self, location: &str) -> anyhow::Result<ScannerInfo> {
        if location != LOCATION {
            bail!("no scanner at {location}");
        }
        self.connected = true;
        Ok(ScannerInfo {
            product: "LS-9000 ED (demo)".into(),
            optical_dpi: OPTICAL_DPI as u16,
            min_dpi: 333,
            max_dpi: 4000,
            max_samples: 16,
        })
    }

    fn disconnect(&mut self) {
        self.connected = false;
        self.frames.clear();
    }

    fn prepare(&mut self) -> anyhow::Result<Option<HolderInfo>> {
        if !self.connected {
            bail!("no scanner connected");
        }
        Ok(self.loaded.then(Self::holder))
    }

    fn discover(
        &mut self,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Strip> {
        self.check()?;
        self.frames.clear();
        Self::holder().check_format(settings.format).map_err(anyhow::Error::msg)?;
        let length = length(settings.format).ok_or_else(|| anyhow!("choose a format"))?;
        self.run_pass(Phase::Thumbnail, on)?;

        let frames = place_frames(length);
        // The overview covers the whole travel; a column is PER_COLUMN addresses
        let width = (TRAVEL.1 / PER_COLUMN) as usize;
        let column = |address: u32| (address / PER_COLUMN) as usize;
        let base = [52_000u16, 38_000, 26_000]; // orange-ish film base
        let mut planes = vec![vec![0u16; width * ROWS]; 3];
        for x in 0..width {
            let address = x as u32 * PER_COLUMN;
            let inside = frames.iter().find(|(top, bottom)| (*top..*bottom).contains(&address));
            for y in 0..ROWS {
                for (c, plane) in planes.iter_mut().enumerate() {
                    plane[y * width + x] = match inside {
                        Some((top, bottom)) => scene(
                            (address - top) as f32 / (bottom - top) as f32,
                            y as f32 / ROWS as f32,
                            base[c],
                        ),
                        None => base[c],
                    };
                }
            }
        }
        let boxes = frames
            .iter()
            .map(|&(top, bottom)| Some(FrameBox { start: column(top), end: column(bottom).min(width) }))
            .collect();
        self.frames = frames;
        Ok(Strip {
            thumbnail: Some(Planes { width, height: ROWS, stride: width, planes }),
            frames: boxes,
        })
    }

    fn scan_frame(
        &mut self,
        index: usize,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Image> {
        self.check()?;
        let &(top, bottom) = self
            .frames
            .get(index)
            .ok_or_else(|| anyhow!("frame {} was not detected", index + 1))?;
        check_feed(top, bottom, TRAVEL.0, TRAVEL.1, BOUNDARY)
            .map_err(|e| anyhow!("refusing to scan frame {}: {e}", index + 1))?;
        self.run_pass(Phase::Meter(1), on)?;
        self.run_pass(Phase::Scan, on)?;

        // 56 mm across the film by the frame's length, at a tenth of the
        // pixel count so the demo stays quick
        let dpi = u32::from(settings.dpi.unwrap_or(OPTICAL_DPI as u16));
        let width = ((560 * dpi / 254) / 10).max(16) as usize;
        let height = ((bottom - top) * dpi / OPTICAL_DPI / 10).max(16) as usize;
        let base = [52_000u16, 38_000, 26_000];
        let planes = (0..3)
            .map(|c| {
                (0..width * height)
                    .map(|i| {
                        let (x, y) = (i % width, i / width);
                        scene(
                            y as f32 / height as f32 + index as f32 * 0.3,
                            x as f32 / width as f32,
                            base[c],
                        )
                    })
                    .collect()
            })
            .collect();
        Ok(Image {
            planes: Planes { width, height, stride: width, planes },
            dpi,
            complete: true,
        })
    }

    fn eject(&mut self) -> anyhow::Result<()> {
        if !self.connected {
            bail!("no scanner connected");
        }
        self.frames.clear();
        Ok(())
    }
}

/// A smooth made-up picture, as a negative would transmit it
fn scene(x: f32, y: f32, base: u16) -> u16 {
    let picture = 0.5 + 0.25 * (x * 9.0).sin() * (y * 5.0).cos() + 0.2 * (1.0 - y);
    (f32::from(base) * (1.0 - 0.85 * picture.clamp(0.0, 1.0))) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(format: FilmFormat) -> usize {
        place_frames(length(format).unwrap()).len()
    }

    #[test]
    fn longer_frames_fit_fewer_times() {
        assert_eq!(count(FilmFormat::F66), 3);
        assert_eq!(count(FilmFormat::F67), 2);
        assert_eq!(count(FilmFormat::F69), 2);
    }

    #[test]
    fn every_frame_stays_inside_the_travel() {
        for format in [FilmFormat::F66, FilmFormat::F67, FilmFormat::F69] {
            for (top, bottom) in place_frames(length(format).unwrap()) {
                assert!(check_feed(top, bottom, TRAVEL.0, TRAVEL.1, BOUNDARY).is_ok());
            }
        }
    }

    #[test]
    fn refuses_formats_the_holder_does_not_take() {
        let mut fake = FakeBackend::instant();
        fake.connect(LOCATION).unwrap();
        let mut on = |_, _| Flow::Continue(());
        for format in [FilmFormat::Auto, FilmFormat::F645, FilmFormat::F135] {
            let settings = ScanSettings { format, ..ScanSettings::default() };
            assert!(fake.discover(&settings, &mut on).is_err(), "{format:?}");
        }
        let settings = ScanSettings { format: FilmFormat::F67, ..ScanSettings::default() };
        assert_eq!(fake.discover(&settings, &mut on).unwrap().frames.len(), 2);
    }
}
