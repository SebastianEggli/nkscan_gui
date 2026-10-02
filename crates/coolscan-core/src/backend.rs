//! The seam between the app and a scanner

use crate::{DeviceInfo, HolderInfo, Image, Phase, Progress, ScanSettings, ScannerInfo, Strip};
use std::ops::ControlFlow;

/// What a progress callback answers: `Break` cancels the pass
pub type Flow = ControlFlow<()>;

/// One scanner, real or simulated
///
/// Calls block until the scanner is done. A backend is created and used on a
/// single thread, so it need not be `Send`.
pub trait Backend {
    /// Every scanner that can be seen right now
    fn list_devices(&mut self) -> anyhow::Result<Vec<DeviceInfo>>;

    /// Open the scanner at `location`, as [`DeviceInfo::location`] gives it
    fn connect(&mut self, location: &str) -> anyhow::Result<ScannerInfo>;

    /// Let go of the scanner
    fn disconnect(&mut self);

    /// Get the loaded holder ready to scan, answering which holder it is, or
    /// `None` when nothing is loaded
    fn prepare(&mut self) -> anyhow::Result<Option<HolderInfo>>;

    /// Find the frames on the loaded film. Forgets any earlier strip.
    ///
    /// Refuses a format the loaded holder does not take
    /// ([`HolderInfo::check_format`]) before anything moves
    fn discover(
        &mut self,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Strip>;

    /// Focus, meter and scan frame `index` of the last [`discover`](Self::discover)
    fn scan_frame(
        &mut self,
        index: usize,
        settings: &ScanSettings,
        on: &mut dyn FnMut(Phase, Progress) -> Flow,
    ) -> anyhow::Result<Image>;

    /// Give the film back
    fn eject(&mut self) -> anyhow::Result<()>;
}
