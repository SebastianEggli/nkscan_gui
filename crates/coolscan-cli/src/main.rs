//! Command line front end: list scanners, or scan a strip to plain TIFFs
//!
//! Meant for bringing up hardware and for headless use. It drives the backend
//! directly, without the worker thread.

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use coolscan_core::{
    Backend, FakeBackend, FilmFormat, FilmType, Flow, NkscanBackend, Phase, Progress, ScanSettings,
    is_cancelled, tiff_out,
};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Parser)]
#[command(version, about = "Scan film on a Nikon Coolscan through nkscan")]
struct Cli {
    /// Use a simulated scanner instead of real hardware
    #[arg(long, global = true)]
    demo: bool,

    /// Log level filter, e.g. info, debug, nkscan=trace
    #[arg(long, global = true, default_value = "warn")]
    log: String,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the scanners that can be seen
    List,
    /// Find the frames on the loaded film and save them as 16-bit TIFFs
    Scan(ScanArgs),
    /// Give the film back
    Eject {
        /// Scanner location as `list` prints it; defaults to the only one
        #[arg(long)]
        device: Option<String>,
    },
}

#[derive(clap::Args)]
struct ScanArgs {
    /// Scanner location as `list` prints it; defaults to the only one
    #[arg(long)]
    device: Option<String>,
    /// Resolution in dpi; defaults to the scanner's optical resolution
    #[arg(long)]
    dpi: Option<u16>,
    /// Times each line is read and averaged, 1-16
    #[arg(long, default_value_t = 1)]
    samples: u8,
    /// Film type, which decides how exposure is metered
    #[arg(long, value_enum, default_value_t = Film::Negative)]
    film: Film,
    /// Frame format: auto, 135, half, 16, 645, 66, 67, 68, 69
    #[arg(long, default_value = "auto", value_parser = parse_format)]
    format: FilmFormat,
    /// Frames to scan, counting from 1, e.g. 1,3,4. Defaults to all
    #[arg(long, value_delimiter = ',')]
    frames: Vec<usize>,
    /// Folder to save into
    #[arg(long, short, default_value = ".")]
    out: PathBuf,
    /// File names are <name>_<frame>.tif
    #[arg(long, default_value = "scan")]
    name: String,
    /// Leave the film in the scanner afterwards
    #[arg(long)]
    no_eject: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Film {
    Negative,
    Positive,
    Mono,
}

impl From<Film> for FilmType {
    fn from(film: Film) -> Self {
        match film {
            Film::Negative => FilmType::Negative,
            Film::Positive => FilmType::Positive,
            Film::Mono => FilmType::Mono,
        }
    }
}

fn parse_format(s: &str) -> Result<FilmFormat, String> {
    FilmFormat::parse(s).ok_or_else(|| format!("unknown format {s:?}"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_writer(std::io::stderr)
        .init();

    let mut backend: Box<dyn Backend> = match cli.demo {
        true => Box::new(FakeBackend::new()),
        false => Box::new(NkscanBackend::new()),
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst)).context("installing Ctrl-C handler")?;

    match cli.command {
        Cmd::List => list(backend.as_mut()),
        Cmd::Scan(args) => {
            let result = scan(backend.as_mut(), args, &cancel);
            match result {
                Err(e) if is_cancelled(&e) => {
                    eprintln!("\ncancelled, ejecting");
                    backend.eject().ok();
                    Ok(())
                }
                other => other,
            }
        }
        Cmd::Eject { device } => {
            connect(backend.as_mut(), device)?;
            backend.eject()?;
            eprintln!("ejected");
            Ok(())
        }
    }
}

fn list(backend: &mut dyn Backend) -> anyhow::Result<()> {
    let devices = backend.list_devices()?;
    if devices.is_empty() {
        eprintln!("no scanners found");
    }
    for d in devices {
        let note = if d.available { "" } else { "  (in use by another program)" };
        println!("{}\t{}{note}", d.location, d.description);
    }
    Ok(())
}

/// Connect to `device`, or to the only scanner there is
fn connect(backend: &mut dyn Backend, device: Option<String>) -> anyhow::Result<()> {
    let location = match device {
        Some(d) => d,
        None => {
            let devices = backend.list_devices()?;
            match devices.as_slice() {
                [only] => only.location.clone(),
                [] => bail!("no scanners found"),
                _ => bail!("several scanners found, pick one with --device (see `list`)"),
            }
        }
    };
    let info = backend.connect(&location)?;
    eprintln!("connected to {} at {location}", info.product);
    Ok(())
}

fn scan(backend: &mut dyn Backend, args: ScanArgs, cancel: &AtomicBool) -> anyhow::Result<()> {
    let settings = ScanSettings {
        film: args.film.into(),
        format: args.format,
        dpi: args.dpi,
        samples: args.samples,
        output_dir: args.out,
        basename: args.name,
    };
    settings.validate(None).map_err(anyhow::Error::msg)?;
    connect(backend, args.device)?;

    // The LS-9000 takes its holder by hand, so wait for one to go in
    let holder = match backend.prepare()? {
        Some(holder) => holder,
        None => {
            eprintln!("insert the film holder (Ctrl-C to stop)");
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(500));
                if let Some(holder) = backend.prepare()? {
                    break holder;
                }
            }
        }
    };
    eprintln!("holder: {}", holder.name);

    let strip = backend.discover(&settings, &mut reporter("overview", cancel))?;
    eprintln!();
    let found = strip.frames.len();
    eprintln!("{found} frame(s) detected");

    let frames: Vec<usize> = match args.frames.is_empty() {
        true => (0..found).collect(),
        false => args
            .frames
            .iter()
            .map(|&n| match n {
                1.. if n <= found => Ok(n - 1),
                _ => bail!("frame {n} is not one of the {found} detected"),
            })
            .collect::<anyhow::Result<_>>()?,
    };

    for &index in &frames {
        let label = format!("frame {}", index + 1);
        let image = backend.scan_frame(index, &settings, &mut reporter(&label, cancel))?;
        eprintln!();
        let path = tiff_out::frame_path(&settings.output_dir, &settings.basename, index + 1);
        tiff_out::write_tiff(&path, &image.planes, image.dpi)?;
        println!(
            "{}  ({} x {} at {} dpi{})",
            path.display(),
            image.planes.width,
            image.planes.height,
            image.dpi,
            if image.complete { "" } else { ", incomplete" }
        );
    }

    if !args.no_eject {
        backend.eject()?;
        eprintln!("ejected");
    }
    Ok(())
}

/// A progress line on stderr that stops the pass on Ctrl-C
fn reporter<'a>(label: &'a str, cancel: &'a AtomicBool) -> impl FnMut(Phase, Progress) -> Flow + 'a {
    let mut last = (None, -1);
    move |phase, progress| {
        if cancel.load(Ordering::SeqCst) {
            return Flow::Break(());
        }
        let percent = progress.fraction().map_or(0, |f| (f * 100.0) as i32);
        if last != (Some(phase), percent) {
            last = (Some(phase), percent);
            let step = match phase {
                Phase::Thumbnail => "overview".to_string(),
                Phase::Meter(n) => format!("metering {n}"),
                Phase::Scan => "scanning".to_string(),
            };
            eprint!("\r{label}: {step:<12} {percent:>3}%   ");
            std::io::stderr().flush().ok();
        }
        Flow::Continue(())
    }
}
