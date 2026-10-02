//! A thread that owns the scanner and runs commands one at a time
//!
//! The caller sends [`Command`]s and polls [`Event`]s, and is never blocked by
//! a scan. The backend is built on the worker thread itself, so nothing about
//! it has to be `Send`.

use crate::{
    Backend, DeviceInfo, Flow, HolderInfo, Phase, Progress, SavedFrame, ScanSettings, ScannerInfo, Strip,
    is_cancelled, preview, tiff_out,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};
use tracing::{error, info, warn};

/// Longest side of the preview sent with each saved frame
const SAVED_PREVIEW_SIDE: usize = 640;

#[derive(Debug, Clone)]
pub enum Command {
    ListDevices,
    Connect(String),
    Disconnect,
    /// Get the film ready and find its frames
    Preview(ScanSettings),
    /// Scan these frames (zero-based) of the last preview and save them
    Scan { settings: ScanSettings, frames: Vec<usize> },
    Eject,
}

#[derive(Debug, Clone)]
pub enum Event {
    Devices(Vec<DeviceInfo>),
    Connected(ScannerInfo),
    Disconnected,
    /// A command started; the text says what is happening
    Busy(String),
    Progress { label: String, fraction: Option<f32> },
    /// The holder that is loaded, or None once it is gone
    Holder(Option<HolderInfo>),
    Strip(Strip),
    FrameSaved(SavedFrame),
    /// The command finished; the text says how
    Done(String),
    Cancelled(String),
    Error(String),
}

/// Handle to the worker thread
pub struct Worker {
    commands: Sender<Command>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    /// Start the thread. `make_backend` runs on it; `notify` is called after
    /// every event, e.g. to wake a UI
    pub fn spawn<B, F, N>(make_backend: F, notify: N) -> Self
    where
        B: Backend + 'static,
        F: FnOnce() -> B + Send + 'static,
        N: Fn() + Send + 'static,
    {
        let (commands, inbox) = mpsc::channel::<Command>();
        let (outbox, events) = mpsc::channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        thread::Builder::new()
            .name("scanner".into())
            .spawn(move || {
                let mut backend = make_backend();
                let send = |event: Event| {
                    let _ = outbox.send(event);
                    notify();
                };
                for command in inbox {
                    run(&mut backend, command, &flag, &send);
                }
                backend.disconnect();
            })
            .expect("spawning the scanner thread");
        Self { commands, events, cancel }
    }

    /// Queue a command
    pub fn send(&self, command: Command) {
        self.cancel.store(false, Ordering::SeqCst);
        let _ = self.commands.send(command);
    }

    /// Ask the running command to stop at its next chance
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Events that have arrived since the last call
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Wait for the next event, for callers without a UI loop
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread ends once the channel closes; a pass in flight is told to stop
        self.cancel.store(true, Ordering::SeqCst);
    }
}

fn run(backend: &mut dyn Backend, command: Command, cancel: &AtomicBool, send: &dyn Fn(Event)) {
    let result = match command {
        Command::ListDevices => backend.list_devices().map(|d| send(Event::Devices(d))),
        Command::Connect(location) => {
            send(Event::Busy(format!("Connecting to {location}…")));
            backend.connect(&location).map(|info| {
                send(Event::Connected(info.clone()));
                send(Event::Done(format!("Connected to {}", info.product)));
            })
        }
        Command::Disconnect => {
            backend.disconnect();
            send(Event::Disconnected);
            Ok(())
        }
        Command::Preview(settings) => preview(backend, &settings, cancel, send),
        Command::Scan { settings, frames } => scan(backend, &settings, &frames, cancel, send),
        Command::Eject => {
            send(Event::Busy("Ejecting…".into()));
            backend.eject().map(|()| {
                send(Event::Holder(None));
                send(Event::Done("Film ejected".into()));
            })
        }
    };

    if let Err(e) = result {
        if is_cancelled(&e) {
            info!("cancelled");
            // A stopped pass leaves the unit mid-scan; giving the film back is
            // what puts it in a known state again, as nkscan's CLI does
            let message = match backend.eject() {
                Ok(()) => "Cancelled, film ejected".to_string(),
                Err(e) => {
                    warn!(%e, "could not eject after cancel");
                    format!("Cancelled, but could not eject: {e:#}")
                }
            };
            send(Event::Cancelled(message));
        } else {
            error!("{e:#}");
            send(Event::Error(format!("{e:#}")));
        }
    }
}

fn preview(
    backend: &mut dyn Backend,
    settings: &ScanSettings,
    cancel: &AtomicBool,
    send: &dyn Fn(Event),
) -> anyhow::Result<()> {
    send(Event::Busy("Checking for film…".into()));
    let Some(holder) = backend.prepare()? else {
        send(Event::Holder(None));
        anyhow::bail!("No film holder detected. Insert the holder and try again.");
    };
    send(Event::Holder(Some(holder)));
    send(Event::Busy("Finding frames…".into()));
    let mut on = progress_reporter("Overview".into(), cancel, send);
    let strip = backend.discover(settings, &mut on)?;
    let count = strip.frames.len();
    send(Event::Strip(strip));
    send(Event::Done(match count {
        0 => "No frames detected on this film".into(),
        1 => "1 frame detected".into(),
        n => format!("{n} frames detected"),
    }));
    Ok(())
}

fn scan(
    backend: &mut dyn Backend,
    settings: &ScanSettings,
    frames: &[usize],
    cancel: &AtomicBool,
    send: &dyn Fn(Event),
) -> anyhow::Result<()> {
    settings.validate(None).map_err(anyhow::Error::msg)?;
    if frames.is_empty() {
        anyhow::bail!("no frames selected");
    }
    for (n, &index) in frames.iter().enumerate() {
        let label = format!("Frame {} ({} of {})", index + 1, n + 1, frames.len());
        send(Event::Busy(format!("Scanning {label}…")));
        let mut on = progress_reporter(label, cancel, send);
        let image = backend.scan_frame(index, settings, &mut on)?;
        if !image.complete {
            warn!(frame = index + 1, "incomplete pass, saving what arrived");
        }

        let path = tiff_out::frame_path(&settings.output_dir, &settings.basename, index + 1);
        tiff_out::write_tiff(&path, &image.planes, image.dpi)?;
        info!(path = %path.display(), "saved");
        send(Event::FrameSaved(SavedFrame {
            index,
            path,
            width: image.planes.width,
            height: image.planes.height,
            dpi: image.dpi,
            preview: preview::render(&image.planes, SAVED_PREVIEW_SIDE, false),
        }));
    }
    send(Event::Done(format!(
        "Saved {} frame{} to {}",
        frames.len(),
        if frames.len() == 1 { "" } else { "s" },
        settings.output_dir.display()
    )));
    Ok(())
}

/// A progress callback that forwards to the UI, at most once per percent, and
/// stops the pass once cancel is set
fn progress_reporter<'a>(
    label: String,
    cancel: &'a AtomicBool,
    send: &'a dyn Fn(Event),
) -> impl FnMut(Phase, Progress) -> Flow + 'a {
    let mut last: Option<(Phase, i32)> = None;
    move |phase, progress| {
        if cancel.load(Ordering::SeqCst) {
            return Flow::Break(());
        }
        let fraction = progress.fraction();
        let percent = fraction.map_or(-1, |f| (f * 100.0) as i32);
        if last != Some((phase, percent)) {
            last = Some((phase, percent));
            let step = match phase {
                Phase::Thumbnail => "overview pass".to_string(),
                Phase::Meter(n) => format!("metering exposure (pass {n})"),
                Phase::Scan => "scanning".to_string(),
            };
            send(Event::Progress { label: format!("{label}: {step}"), fraction });
        }
        Flow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeBackend;
    use std::time::Duration;

    fn wait_for(worker: &Worker, mut until: impl FnMut(&Event) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        loop {
            let event = worker.events.recv_timeout(Duration::from_secs(10)).expect("an event");
            let done = until(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
    }

    fn finished(e: &Event) -> bool {
        matches!(e, Event::Done(_) | Event::Error(_) | Event::Cancelled(_))
    }

    fn settings(dir: &std::path::Path) -> ScanSettings {
        ScanSettings {
            output_dir: dir.to_path_buf(),
            dpi: Some(1000),
            format: crate::FilmFormat::F66,
            ..ScanSettings::default()
        }
    }

    #[test]
    fn previews_and_scans_selected_frames() {
        let dir = std::env::temp_dir().join(format!("coolscan-worker-{}", std::process::id()));
        let worker = Worker::spawn(FakeBackend::instant, || {});
        worker.send(Command::Connect("demo:0".into()));
        wait_for(&worker, finished);

        worker.send(Command::Preview(settings(&dir)));
        let events = wait_for(&worker, finished);
        let strip = events.iter().find_map(|e| match e {
            Event::Strip(s) => Some(s.clone()),
            _ => None,
        });
        // Three 6x6 frames fit the FH-869S
        assert_eq!(strip.expect("a strip").frames.len(), 3);

        worker.send(Command::Scan { settings: settings(&dir), frames: vec![0, 2] });
        let events = wait_for(&worker, finished);
        let saved: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::FrameSaved(f) => Some(f.path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(saved.len(), 2, "{events:?}");
        assert!(saved.iter().all(|p| p.exists()));
        assert!(saved[1].file_name().unwrap().to_string_lossy().starts_with("scan_03"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cancel_stops_a_scan() {
        let dir = std::env::temp_dir().join(format!("coolscan-cancel-{}", std::process::id()));
        // Slow passes, so the cancel lands mid-scan
        let worker = Worker::spawn(FakeBackend::new, || {});
        worker.send(Command::Connect("demo:0".into()));
        wait_for(&worker, finished);
        worker.send(Command::Preview(settings(&dir)));
        wait_for(&worker, finished);

        worker.send(Command::Scan { settings: settings(&dir), frames: vec![0, 1, 2] });
        wait_for(&worker, |e| matches!(e, Event::Progress { .. }));
        worker.cancel();
        let events = wait_for(&worker, finished);
        assert!(matches!(events.last(), Some(Event::Cancelled(_))), "{events:?}");
        assert!(!events.iter().any(|e| matches!(e, Event::FrameSaved(_))));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn errors_are_reported() {
        let worker = Worker::spawn(FakeBackend::instant, || {});
        worker.send(Command::Connect("nowhere".into()));
        let events = wait_for(&worker, finished);
        assert!(matches!(events.last(), Some(Event::Error(_))));
    }
}
