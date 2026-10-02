//! The window: a device bar, settings on the left, the film strip in the
//! middle, progress at the bottom
//!
//! All scanner work happens on the core's worker thread; this only sends
//! commands and draws the events that come back.

use coolscan_core::{
    Command, DeviceInfo, Event, FakeBackend, FilmFormat, FilmType, HolderInfo, NkscanBackend, Planes,
    Rgba8, SavedFrame, ScanSettings, ScannerInfo, Worker,
    preview,
    settings::{DPI_CHOICES, SAMPLE_CHOICES},
};
use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Vec2,
};
use serde::{Deserialize, Serialize};

/// Longest side of the strip texture
const STRIP_SIDE: usize = 2048;
/// How many log lines to keep
const LOG_LINES: usize = 200;

/// What survives a restart
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    settings: ScanSettings,
    invert_preview: bool,
}

/// The loaded film after a preview
struct StripView {
    thumbnail: Option<Planes>,
    frames: Vec<Option<coolscan_core::FrameBox>>,
    selected: Vec<bool>,
    texture: Option<TextureHandle>,
    /// Whether `texture` was rendered inverted
    texture_inverted: bool,
    /// The format the frames were found with
    format: FilmFormat,
}

pub struct App {
    worker: Worker,
    demo: bool,
    devices: Vec<DeviceInfo>,
    chosen_device: Option<String>,
    scanner: Option<ScannerInfo>,
    /// The loaded holder, known after Load / Preview
    holder: Option<HolderInfo>,
    settings: ScanSettings,
    invert_preview: bool,
    strip: Option<StripView>,
    /// Text of the command running now; `None` when idle
    busy: Option<String>,
    progress: Option<(String, Option<f32>)>,
    status: String,
    status_is_error: bool,
    /// The format the last Load / Preview asked for
    preview_format: FilmFormat,
    last_saved: Option<(SavedFrame, TextureHandle)>,
    log: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, demo: bool) -> Self {
        let saved: Saved = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();

        let ctx = cc.egui_ctx.clone();
        let wake = move || ctx.request_repaint();
        let worker = match demo {
            true => Worker::spawn(FakeBackend::new, wake),
            false => Worker::spawn(NkscanBackend::new, wake),
        };
        worker.send(Command::ListDevices);

        Self {
            worker,
            demo,
            devices: Vec::new(),
            chosen_device: None,
            scanner: None,
            holder: None,
            settings: saved.settings,
            invert_preview: saved.invert_preview,
            strip: None,
            busy: Some("Looking for scanners…".into()),
            progress: None,
            status: String::new(),
            status_is_error: false,
            preview_format: FilmFormat::Auto,
            last_saved: None,
            log: Vec::new(),
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > LOG_LINES {
            self.log.remove(0);
        }
    }

    fn set_status(&mut self, text: String, error: bool) {
        self.log(text.clone());
        self.status = text;
        self.status_is_error = error;
    }

    fn handle(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Devices(devices) => {
                self.busy = None;
                // Keep the choice if it is still there, else take the first
                if !devices.iter().any(|d| Some(&d.location) == self.chosen_device.as_ref()) {
                    self.chosen_device = devices.first().map(|d| d.location.clone());
                }
                let text = match devices.len() {
                    0 => "No scanner found. Check the FireWire cable and power, then Refresh.".into(),
                    1 => format!("Found {}", devices[0].description),
                    n => format!("Found {n} scanners"),
                };
                self.devices = devices;
                self.scanner = None;
                self.set_status(text, false);
            }
            Event::Connected(info) => self.scanner = Some(info),
            Event::Disconnected => {
                self.scanner = None;
                self.holder = None;
                self.strip = None;
            }
            Event::Holder(holder) => {
                if let Some(h) = &holder {
                    self.log(format!("Holder: {}", h.name));
                }
                self.holder = holder;
            }
            Event::Busy(text) => {
                self.log(text.clone());
                self.busy = Some(text);
                self.progress = None;
            }
            Event::Progress { label, fraction } => self.progress = Some((label, fraction)),
            Event::Strip(strip) => {
                let count = strip.frames.len();
                self.strip = Some(StripView {
                    thumbnail: strip.thumbnail,
                    frames: strip.frames,
                    selected: vec![true; count],
                    texture: None,
                    texture_inverted: false,
                    format: self.preview_format,
                });
            }
            Event::FrameSaved(frame) => {
                self.log(format!(
                    "Saved {} ({} × {} at {} dpi)",
                    frame.path.display(),
                    frame.width,
                    frame.height,
                    frame.dpi
                ));
                let texture = load_texture(ctx, "last-saved", &frame.preview);
                self.last_saved = Some((frame, texture));
            }
            Event::Done(text) => self.finish(text, false),
            Event::Cancelled(text) => {
                self.strip = None;
                self.finish(text, false);
            }
            Event::Error(text) => self.finish(text, true),
        }
    }

    fn finish(&mut self, text: String, error: bool) {
        self.busy = None;
        self.progress = None;
        self.set_status(text, error);
    }

    fn device_bar(&mut self, ui: &mut egui::Ui) {
        let idle = self.busy.is_none();
        ui.horizontal(|ui| {
            ui.label("Scanner:");
            let shown = self
                .chosen_device
                .as_ref()
                .and_then(|loc| self.devices.iter().find(|d| &d.location == loc))
                .map_or_else(|| "none found".to_string(), describe);
            ui.add_enabled_ui(idle && self.scanner.is_none(), |ui| {
                egui::ComboBox::from_id_salt("device")
                    .selected_text(shown)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for d in &self.devices {
                            ui.selectable_value(&mut self.chosen_device, Some(d.location.clone()), describe(d));
                        }
                    });
            });
            if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                self.strip = None;
                self.worker.send(Command::ListDevices);
            }
            match &self.scanner {
                None => {
                    let can = idle && self.chosen_device.is_some();
                    if ui.add_enabled(can, egui::Button::new("Connect")).clicked()
                        && let Some(loc) = self.chosen_device.clone()
                    {
                        self.worker.send(Command::Connect(loc));
                    }
                    ui.colored_label(Color32::GRAY, "not connected");
                }
                Some(info) => {
                    if ui.add_enabled(idle, egui::Button::new("Disconnect")).clicked() {
                        self.worker.send(Command::Disconnect);
                    }
                    ui.colored_label(Color32::from_rgb(60, 170, 80), format!("Connected: {}", info.product));
                }
            }
            if self.demo {
                ui.colored_label(Color32::from_rgb(200, 140, 40), "DEMO MODE");
            }
        });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let idle = self.busy.is_none();
        let connected = self.scanner.is_some();
        let s = &mut self.settings;

        ui.add_enabled_ui(idle, |ui| {
            ui.heading("Film");
            for film in FilmType::ALL {
                ui.radio_value(&mut s.film, film, film.label());
            }
            ui.add_space(4.0);
            ui.label("Format");
            let holder = self.holder.as_ref();
            egui::ComboBox::from_id_salt("format")
                .selected_text(s.format.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    // Once the holder is known, only its formats can be picked
                    for format in FilmFormat::ALL {
                        let offered = holder.is_none_or(|h| h.offers(format));
                        ui.add_enabled_ui(offered, |ui| {
                            ui.selectable_value(&mut s.format, format, format.label());
                        });
                    }
                });
            if let Some(h) = holder {
                ui.small(format!("Holder: {}", h.name));
            }

            ui.separator();
            ui.heading("Scan");
            let max_dpi = self.scanner.as_ref().map_or(4000, |i| i.max_dpi);
            egui::Grid::new("scan-settings").num_columns(2).show(ui, |ui| {
                ui.label("Resolution");
                let label = |dpi: Option<u16>| dpi.map_or("Optical max".into(), |d| format!("{d} dpi"));
                egui::ComboBox::from_id_salt("dpi")
                    .selected_text(label(s.dpi))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.dpi, None, label(None));
                        for dpi in DPI_CHOICES.into_iter().filter(|&d| d <= max_dpi) {
                            ui.selectable_value(&mut s.dpi, Some(dpi), label(Some(dpi)));
                        }
                    });
                ui.end_row();

                ui.label("Samples").on_hover_text("Times each line is read and averaged. More is less noisy and slower.");
                let max_samples = self.scanner.as_ref().map_or(16, |i| i.max_samples);
                egui::ComboBox::from_id_salt("samples")
                    .selected_text(format!("{}×", s.samples))
                    .show_ui(ui, |ui| {
                        for n in SAMPLE_CHOICES.into_iter().filter(|&n| n <= max_samples) {
                            ui.selectable_value(&mut s.samples, n, format!("{n}×"));
                        }
                    });
                ui.end_row();
            });

            ui.separator();
            ui.heading("Output");
            ui.horizontal(|ui| {
                let shown = s.output_dir.display().to_string();
                ui.add(egui::Label::new(shown.clone()).truncate()).on_hover_text(shown);
            });
            if ui.button("Choose folder…").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_directory(&s.output_dir).pick_folder()
            {
                s.output_dir = dir;
            }
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut s.basename);
            });
            ui.small(format!("Saves {}_01.tif, … (16-bit, unadjusted)", s.basename.trim()));
        });

        ui.separator();
        let format_problem = self.holder.as_ref().and_then(|h| h.check_format(self.settings.format).err());
        let invalid = self.settings.validate(self.scanner.as_ref()).err().or(format_problem);
        if let Some(problem) = &invalid {
            ui.colored_label(Color32::from_rgb(220, 80, 60), problem);
        }

        let selected: Vec<usize> = self
            .strip
            .as_ref()
            .map(|s| s.selected.iter().enumerate().filter(|(_, on)| **on).map(|(i, _)| i).collect())
            .unwrap_or_default();

        let wide = Vec2::new(ui.available_width(), 28.0);
        if ui
            .add_enabled(idle && connected, egui::Button::new("Load / Preview").min_size(wide))
            .on_hover_text("Insert the film holder, then find the frames on it")
            .clicked()
        {
            self.strip = None;
            self.preview_format = self.settings.format;
            self.worker.send(Command::Preview(self.settings.clone()));
        }
        // Frames found for one format are the wrong rectangles for another
        let stale = self.strip.as_ref().is_some_and(|s| s.format != self.settings.format);
        if stale {
            ui.colored_label(
                Color32::from_rgb(220, 150, 40),
                "Format changed: press Load / Preview again before scanning",
            );
        }
        let scan_text = format!("Scan selected ({})", selected.len());
        let can_scan = idle && connected && !selected.is_empty() && invalid.is_none() && !stale;
        if ui.add_enabled(can_scan, egui::Button::new(scan_text).min_size(wide)).clicked() {
            self.worker.send(Command::Scan { settings: self.settings.clone(), frames: selected });
        }
        ui.horizontal(|ui| {
            let half = Vec2::new((ui.available_width() - ui.spacing().item_spacing.x) / 2.0, 28.0);
            if ui.add_enabled(!idle, egui::Button::new("Cancel").min_size(half)).clicked() {
                self.worker.cancel();
            }
            if ui.add_enabled(idle && connected, egui::Button::new("Eject").min_size(half)).clicked() {
                self.strip = None;
                self.holder = None;
                self.worker.send(Command::Eject);
            }
        });
    }

    fn strip_view(&mut self, ui: &mut egui::Ui) {
        let idle = self.busy.is_none();
        ui.horizontal(|ui| {
            ui.heading("Film strip");
            ui.checkbox(&mut self.invert_preview, "Invert preview")
                .on_hover_text("Display only; saved files are never inverted");
            if let Some(strip) = &mut self.strip
                && !strip.selected.is_empty()
            {
                if ui.add_enabled(idle, egui::Button::new("All")).clicked() {
                    strip.selected.iter_mut().for_each(|s| *s = true);
                }
                if ui.add_enabled(idle, egui::Button::new("None")).clicked() {
                    strip.selected.iter_mut().for_each(|s| *s = false);
                }
            }
        });

        let invert = self.invert_preview;
        let Some(strip) = &mut self.strip else {
            ui.add_space(20.0);
            ui.label(match self.scanner {
                Some(_) => "Insert a film holder and press Load / Preview.",
                None => "Connect to a scanner to start.",
            });
            return;
        };

        // Re-render when the inversion setting changes
        if let Some(thumbnail) = &strip.thumbnail
            && (strip.texture.is_none() || strip.texture_inverted != invert)
        {
            let image = preview::render(thumbnail, STRIP_SIDE, invert);
            strip.texture = Some(load_texture(ui.ctx(), "strip", &image));
            strip.texture_inverted = invert;
        }

        match (&strip.texture, &strip.thumbnail) {
            (Some(texture), Some(thumbnail)) => {
                let size = texture.size_vec2();
                let scale = (ui.available_width() / size.x).min(260.0 / size.y);
                let (rect, _) = ui.allocate_exact_size(size * scale, Sense::hover());
                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                // Frame boxes, clickable to toggle
                let columns = thumbnail.width.max(1) as f32;
                for (i, frame) in strip.frames.iter().enumerate() {
                    let Some(frame) = frame else { continue };
                    let x0 = rect.left() + frame.start as f32 / columns * rect.width();
                    let x1 = rect.left() + frame.end as f32 / columns * rect.width();
                    let r = Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x1, rect.bottom())).shrink(1.0);
                    let response = ui.interact(r, ui.id().with(("frame", i)), Sense::click());
                    if response.clicked() && idle {
                        strip.selected[i] = !strip.selected[i];
                    }
                    let on = strip.selected[i];
                    let color = if on { Color32::from_rgb(70, 200, 90) } else { Color32::from_gray(140) };
                    let width = if response.hovered() { 3.0 } else { 2.0 };
                    ui.painter().rect_stroke(r, CornerRadius::ZERO, Stroke::new(width, color), StrokeKind::Inside);
                    ui.painter().text(
                        r.left_top() + Vec2::new(5.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}{}", i + 1, if on { " ✔" } else { "" }),
                        egui::FontId::proportional(15.0),
                        color,
                    );
                }
            }
            _ => {
                ui.label("This holder reports its frames itself, so there is no overview image.");
            }
        }

        // The same selection as checkboxes, which also covers frames that
        // could not be placed on the thumbnail
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if strip.frames.is_empty() {
                ui.label("No frames detected. Check the format setting and try again.");
            }
            for (i, selected) in strip.selected.iter_mut().enumerate() {
                ui.add_enabled(idle, egui::Checkbox::new(selected, format!("Frame {}", i + 1)));
            }
        });

        ui.separator();
        if let Some((frame, texture)) = &self.last_saved {
            ui.label(format!(
                "Last saved: {}  ({} × {} px at {} dpi)",
                frame.path.display(),
                frame.width,
                frame.height,
                frame.dpi
            ));
            let size = texture.size_vec2();
            let available = ui.available_size();
            let scale = (available.x / size.x).min((available.y - 4.0).max(60.0) / size.y).min(1.0);
            ui.image((texture.id(), size * scale));
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(busy) = &self.busy {
                ui.spinner();
                match &self.progress {
                    Some((label, fraction)) => {
                        let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0))
                            .desired_width(240.0)
                            .show_percentage();
                        ui.add(bar);
                        ui.label(label);
                    }
                    None => {
                        ui.label(busy);
                    }
                }
            } else if self.status_is_error {
                ui.colored_label(Color32::from_rgb(220, 80, 60), &self.status);
            } else {
                ui.label(&self.status);
            }
        });
        egui::CollapsingHeader::new("Log").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(120.0).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for event in self.worker.poll() {
            self.handle(ctx, event);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("devices").show(ui, |ui| {
            ui.add_space(4.0);
            self.device_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
        });
        egui::Panel::left("settings")
            .resizable(false)
            .exact_size(230.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_panel(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| self.strip_view(ui));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saved = Saved { settings: self.settings.clone(), invert_preview: self.invert_preview };
        eframe::set_value(storage, eframe::APP_KEY, &saved);
    }
}

fn describe(d: &DeviceInfo) -> String {
    let busy = if d.available { "" } else { " (in use)" };
    format!("{} — {}{busy}", d.description, d.location)
}

fn load_texture(ctx: &egui::Context, name: &str, image: &Rgba8) -> TextureHandle {
    let color = ColorImage::from_rgba_unmultiplied([image.width.max(1), image.height.max(1)], match image.pixels.is_empty() {
        true => &[0, 0, 0, 255],
        false => &image.pixels,
    });
    ctx.load_texture(name, color, TextureOptions::LINEAR)
}
