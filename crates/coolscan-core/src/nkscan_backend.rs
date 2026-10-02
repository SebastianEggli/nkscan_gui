//! The real scanner, through nkscan
//!
//! The call order mirrors nkscan's own CLI (`src/bin/nkscan/scan.rs`): wait
//! for film, stage, discover the frames, then per frame focus, meter and take
//! the pass. Nothing is done to the samples beyond what nkscan itself does
//! (CCD row correction and scaling to full 16 bits).

use crate::{
    Backend, DeviceInfo, FilmFormat, FilmType, Flow, FrameBox, HolderFormats, HolderInfo, Image,
    Phase, Planes, Progress, ScanSettings, ScannerInfo, Strip,
};
use anyhow::{Context, anyhow, bail};
use nkscan::{
    device,
    error::Error,
    protocol::{
        caps::{Capabilities, film::FilmFormat as NkFormat},
        data::Rect,
        decode::{Samples, filled_columns},
        window::Channel,
    },
    scan::{
        focus::Focus,
        frame::{self, Phase as NkPhase},
        framing,
        meter::Metering,
        pass::{Pass, Progress as NkProgress},
        profile::Film,
        window::{MAX_SAMPLES, Recipe},
    },
    session::Session,
};
use tracing::{info, warn};

/// Drives a scanner through nkscan
#[derive(Default)]
pub struct NkscanBackend {
    session: Option<Session>,
    /// Reused between passes; a scan's buffers are moved out of it
    samples: Samples,
    /// The frames the last discovery found
    frames: Vec<Rect>,
    /// The holder the last [`Backend::prepare`] found
    holder: Option<HolderInfo>,
}

impl NkscanBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn session(&mut self) -> anyhow::Result<&mut Session> {
        self.session.as_mut().ok_or_else(|| anyhow!("no scanner connected"))
    }
}

impl Backend for NkscanBackend {
    fn list_devices(&mut self) -> anyhow::Result<Vec<DeviceInfo>> {
        // A connected unit is held exclusively and would list as unavailable
        // to ourselves, so let go first
        self.disconnect();
        Ok(device::list()
            .into_iter()
            .map(|d| {
                let description = match (&d.identity, &d.model) {
                    (Some(id), _) => format!("{} {}", id.vendor.trim(), id.product.trim()),
                    (None, Some(model)) => format!("{model:?}"),
                    (None, None) => "unknown scanner".into(),
                };
                DeviceInfo {
                    location: d.attach.to_string(),
                    description,
                    available: d.opened,
                }
            })
            .collect())
    }

    fn connect(&mut self, location: &str) -> anyhow::Result<ScannerInfo> {
        self.disconnect();
        let devices = device::list();
        let device = device::Selector::Location(location.to_string())
            .resolve(&devices)
            .map_err(|e| anyhow!("{e}"))?;
        let session = Session::open(device.open()?).context("opening a session")?;
        let caps = session.capabilities();
        let x = &caps.address.x_axis;
        let info = ScannerInfo {
            product: caps.identity.product.trim().to_string(),
            optical_dpi: x.optical_dpi,
            min_dpi: x.dpi_range.start,
            max_dpi: x.dpi_range.last,
            max_samples: MAX_SAMPLES,
        };
        info!(?info, "connected");
        self.session = Some(session);
        Ok(info)
    }

    fn disconnect(&mut self) {
        self.session = None;
        self.frames.clear();
        self.holder = None;
    }

    fn prepare(&mut self) -> anyhow::Result<Option<HolderInfo>> {
        let session = self.session()?;
        // What the unit knows about the holder is stale after an eject
        refresh_while_empty(session)?;
        let loaded = match loaded_or_load(session) {
            Err(Error::Media(condition)) => {
                info!(%condition, "waiting on the operator");
                false
            }
            other => other?,
        };
        if !loaded {
            self.holder = None;
            return Ok(None);
        }
        session.stage().context("staging the film")?;
        let holder = holder_info(session.capabilities());
        info!(?holder, "holder loaded");
        self.holder = Some(holder.clone());
        Ok(Some(holder))
    }

    fn discover(
        &mut self,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Strip> {
        self.frames.clear();
        // A frame length the holder was never made for is refused before the
        // overview pass moves anything
        let holder = self
            .holder
            .as_ref()
            .ok_or_else(|| anyhow!("no film holder loaded"))?;
        holder.check_format(settings.format).map_err(anyhow::Error::msg)?;
        let format = nk_format(settings.format);
        let session = self.session.as_mut().ok_or_else(|| anyhow!("no scanner connected"))?;
        let discovery = framing::discover_with(session, format, &mut self.samples, |p| {
            on(Phase::Thumbnail, progress(p))
        })?;

        let caps = session.capabilities();
        let thumbnail = discovery.thumbnail.as_ref().map(|pass| {
            // The detector still reads the buffer, so stretch a copy
            let mut samples = self.samples.clone();
            samples.to_full_scale(pass.layout.bits_per_sample);
            let order = color_planes(pass);
            let mut colors: Vec<Option<Vec<u16>>> = samples.colors.into_iter().map(Some).collect();
            Planes {
                width: pass.cols,
                height: pass.rows,
                stride: pass.cols,
                planes: order.iter().filter_map(|&i| colors.get_mut(i)?.take()).collect(),
            }
        });

        // A thumbnail column is one line pitch of film, so a frame's feed
        // addresses map onto columns through the discovery's own ruler
        let width = thumbnail.as_ref().map_or(0, |t| t.width);
        let frames = discovery
            .frames
            .iter()
            .map(|r| {
                let pitch = discovery.line_pitch?;
                let start = pitch.line_at(caps, r.top).min(width);
                let end = pitch.line_at(caps, r.bottom).min(width);
                (end > start).then_some(FrameBox { start, end })
            })
            .collect();

        info!(frames = discovery.frames.len(), "discovered");
        self.frames = discovery.frames;
        Ok(Strip { thumbnail, frames })
    }

    fn scan_frame(
        &mut self,
        index: usize,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Image> {
        let rect = *self
            .frames
            .get(index)
            .ok_or_else(|| anyhow!("frame {} was not detected", index + 1))?;
        let session = self.session.as_mut().ok_or_else(|| anyhow!("no scanner connected"))?;

        let caps = session.capabilities();
        // nkscan clamps windows to the axis itself; this is a second,
        // independent check, and it refuses rather than clamps
        let y = &caps.address.y_axis;
        check_feed(rect.top, rect.bottom, y.address_range.start, y.address_range.last, y.boundary)
            .map_err(|e| anyhow!("refusing to scan frame {}: {e}", index + 1))?;
        let recipe = Recipe::new(caps, settings.dpi, settings.samples, false, false);
        recipe.supported(caps)?;
        let options = frame::Options {
            exposures: None,
            lock_white_balance: Metering::locks_white_balance(nk_film(settings.film)),
            clean: false,
            focus: Focus::default(),
        };

        let scanned = frame::scan_frame_with(
            session,
            &recipe,
            rect,
            options,
            &mut self.samples,
            |phase, p| {
                let phase = match phase {
                    NkPhase::Meter(n) => Phase::Meter(n),
                    NkPhase::Scan => Phase::Scan,
                };
                on(phase, progress(p))
            },
        )?;
        info!(frame = index + 1, exposures = ?scanned.exposures, "scanned");

        let pass = scanned.pass;
        if pass.blocks == 0 {
            bail!("the scanner returned no data for frame {}", index + 1);
        }
        if !pass.complete {
            warn!(frame = index + 1, "the scanner gave less than the pass promised");
        }

        let order = color_planes(&pass);
        if order.len() != 3 && order.len() != 1 {
            bail!("{} color planes is not an image this writes", order.len());
        }
        // Only what arrived: the tail of a short pass is padding, not film
        let width = {
            let planes: Vec<&[u16]> = self.samples.colors.iter().map(Vec::as_slice).collect();
            filled_columns(&planes, pass.rows, pass.cols)
        };
        // Moved out rather than copied: a 6x9 frame at 4000 dpi is ~700 MB
        let mut colors: Vec<Option<Vec<u16>>> = std::mem::take(&mut self.samples.colors)
            .into_iter()
            .map(Some)
            .collect();
        let planes = order.iter().filter_map(|&i| colors.get_mut(i)?.take()).collect();

        Ok(Image {
            planes: Planes {
                width,
                height: pass.rows,
                stride: pass.cols,
                planes,
            },
            dpi: pass.layout.dpi,
            complete: pass.complete,
        })
    }

    fn eject(&mut self) -> anyhow::Result<()> {
        self.frames.clear();
        self.holder = None;
        let session = self.session()?;
        if !session.eject()? {
            warn!("this scanner cannot eject; take the holder out by hand");
        }
        Ok(())
    }
}

/// Check a frame's extent along the feed before the stage is sent there
///
/// The holder travels along Y. A window past the end of the axis, or longer
/// than the unit's Y boundary, sends the stage behind its stop, where nkscan
/// notes it grinds until a power cycle.
pub(crate) fn check_feed(
    top: u32,
    bottom: u32,
    start: u32,
    last: u32,
    boundary: u32,
) -> Result<(), String> {
    if bottom <= top {
        return Err(format!("the frame is empty ({top}..{bottom})"));
    }
    if top < start || bottom > last {
        return Err(format!(
            "the frame spans {top}..{bottom}, outside the holder's travel {start}..{last}"
        ));
    }
    if boundary > 0 && bottom - top > boundary {
        return Err(format!(
            "the frame is {} long, more than the {boundary} the scanner allows",
            bottom - top
        ));
    }
    Ok(())
}

/// The holder's name and formats, from the ID the unit reports
fn holder_info(caps: &Capabilities) -> HolderInfo {
    let id = caps.address.holder_id;
    let name = match id {
        Some(0x12) => "FH-816".to_string(),
        Some(0x14) => "FH-835M".to_string(),
        Some(0x15) => "FH-835S".to_string(),
        Some(0x16) => "FH-869M".to_string(),
        Some(0x17) => "FH-869S".to_string(),
        Some(0x18) => "FH-869G".to_string(),
        Some(0x19..=0x1D) => "FH-869GR".to_string(),
        Some(other) => format!("holder {other:#04x}"),
        None => "unknown holder".to_string(),
    };
    let ours = |f: NkFormat| match f {
        NkFormat::F135 => Some(FilmFormat::F135),
        NkFormat::F135Half => Some(FilmFormat::F135Half),
        NkFormat::F16 => Some(FilmFormat::F16),
        NkFormat::F645 => Some(FilmFormat::F645),
        NkFormat::F66 => Some(FilmFormat::F66),
        NkFormat::F67 => Some(FilmFormat::F67),
        NkFormat::F68 => Some(FilmFormat::F68),
        NkFormat::F69 => Some(FilmFormat::F69),
        NkFormat::IX240 | NkFormat::Custom(_) => None,
    };
    let formats = match id {
        Some(id) => match (NkFormat::from_holder(id), NkFormat::choices_for_holder(id)) {
            (Some(fixed), _) => ours(fixed).map_or(HolderFormats::Unknown, HolderFormats::Fixed),
            (None, Some(choices)) => HolderFormats::Choices(choices.iter().copied().filter_map(ours).collect()),
            (None, None) => HolderFormats::Unknown,
        },
        None => HolderFormats::Unknown,
    };
    HolderInfo { name, formats }
}

fn progress(p: NkProgress) -> Progress {
    Progress {
        done: p.bytes,
        total: p.total,
    }
}

fn nk_film(film: FilmType) -> Film {
    match film {
        FilmType::Negative => Film::Negative,
        FilmType::Positive => Film::Positive,
        FilmType::Mono => Film::MonochromeNegative,
    }
}

fn nk_format(format: FilmFormat) -> Option<NkFormat> {
    Some(match format {
        FilmFormat::Auto => return None,
        FilmFormat::F135 => NkFormat::F135,
        FilmFormat::F135Half => NkFormat::F135Half,
        FilmFormat::F16 => NkFormat::F16,
        FilmFormat::F645 => NkFormat::F645,
        FilmFormat::F66 => NkFormat::F66,
        FilmFormat::F67 => NkFormat::F67,
        FilmFormat::F68 => NkFormat::F68,
        FilmFormat::F69 => NkFormat::F69,
    })
}

/// Indices into the pass's color buffers, as R, G, B, or the single channel a
/// one-channel unit calls the default. After nkscan's CLI `io::color_planes`
fn color_planes(pass: &Pass) -> Vec<usize> {
    let plane_of = |channel: Channel| {
        pass.layout
            .colors()
            .position(|id| Channel::from(id) == channel)
    };
    let rgb: Vec<usize> = [Channel::Red, Channel::Green, Channel::Blue]
        .into_iter()
        .filter_map(plane_of)
        .collect();
    match rgb.is_empty() {
        true => plane_of(Channel::Default).into_iter().collect(),
        false => rgb,
    }
}

/// [`Session::refresh`], tolerating the empty gate it can itself hit
fn refresh_while_empty(session: &mut Session) -> Result<(), Error> {
    match session.refresh() {
        Ok(()) | Err(Error::Media(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether film is loaded, taking in whatever a feeder has waiting
fn loaded_or_load(session: &mut Session) -> Result<bool, Error> {
    if session.media_loaded()? {
        return Ok(true);
    }
    if !session.load()? {
        return Ok(false);
    }
    session.refresh()?;
    session.media_loaded()
}

#[cfg(test)]
mod tests {
    use super::check_feed;

    #[test]
    fn a_frame_inside_the_travel_passes() {
        assert!(check_feed(2236, 2236 + 13176, 0, 40000, 14000).is_ok());
    }

    #[test]
    fn a_frame_past_the_end_is_refused() {
        assert!(check_feed(30000, 43176, 0, 40000, 14000).is_err());
    }

    #[test]
    fn a_frame_before_the_start_is_refused() {
        assert!(check_feed(10, 5000, 100, 40000, 14000).is_err());
    }

    #[test]
    fn a_frame_longer_than_the_boundary_is_refused() {
        assert!(check_feed(0, 15000, 0, 40000, 14000).is_err());
    }

    #[test]
    fn an_empty_frame_is_refused() {
        assert!(check_feed(500, 500, 0, 40000, 14000).is_err());
    }
}
