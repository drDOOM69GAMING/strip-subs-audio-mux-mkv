//! egui UI for strip-subs. Mirrors the PowerShell GUI: drop zone, per-language
//! keep lists for audio + subtitles, a file queue with language info, a log,
//! and one-click processing on a background thread.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use crate::core;
use crate::tools::Tools;

const BG: egui::Color32 = egui::Color32::from_rgb(15, 21, 29);
const PANEL: egui::Color32 = egui::Color32::from_rgb(18, 26, 36);
const LIGHT: egui::Color32 = egui::Color32::from_rgb(26, 36, 50);
const LIST: egui::Color32 = egui::Color32::from_rgb(11, 15, 21);
const INK: egui::Color32 = egui::Color32::from_rgb(222, 226, 234);
const DIM: egui::Color32 = egui::Color32::from_rgb(142, 150, 164);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(255, 142, 58);
const BLUE: egui::Color32 = egui::Color32::from_rgb(56, 132, 255);

pub enum LogMsg {
    Line(String, egui::Color32),
    Done,
}

pub struct App {
    tools: Option<Tools>,
    tools_error: Option<String>,

    queue: Vec<PathBuf>,
    sel: HashSet<usize>,
    info: HashMap<PathBuf, core::FileInfo>,

    strip_audio: bool,
    strip_subs: bool,
    keep_audio: HashSet<String>,
    keep_subs: HashSet<String>,
    remove_attachments: bool,
    replace: bool,
    recursive: bool,

    log: Vec<(String, egui::Color32)>,
    tx: Sender<LogMsg>,
    rx: Receiver<LogMsg>,
    running: bool,
    abort: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(cc);
        let (tx, rx) = std::sync::mpsc::channel();
        let (tools, tools_error) = match Tools::obtain() {
            Ok(t) => {
                let note = if t.bundled {
                    "embedded tools ready".to_string()
                } else {
                    "external tools found".to_string()
                };
                (Some(t.clone()), Some(note))
            }
            Err(e) => (None, Some(e)),
        };
        let mut keep_audio = HashSet::new();
        keep_audio.insert("eng".to_string()); // default: keep English audio
        let mut app = App {
            tools,
            tools_error,
            queue: Vec::new(),
            sel: HashSet::new(),
            info: HashMap::new(),
            strip_audio: true,
            strip_subs: true,
            keep_audio,
            keep_subs: HashSet::new(),
            remove_attachments: true,
            replace: true,
            recursive: true,
            log: Vec::new(),
            tx,
            rx,
            running: false,
            abort: Arc::new(AtomicBool::new(false)),
            worker: None,
        };
        if let Some(note) = app.tools_error.clone() {
            app.line(note, ORANGE);
        }
        app
    }

    fn line(&mut self, text: impl Into<String>, color: egui::Color32) {
        self.log.push((text.into(), color));
    }

    fn drain_log(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                LogMsg::Line(t, c) => self.line(t, c),
                LogMsg::Done => {
                    self.running = false;
                    if let Some(h) = self.worker.take() {
                        let _ = h.join();
                    }
                }
            }
        }
    }

    fn settings(&self) -> core::KeepSettings {
        let mut ka = HashSet::new();
        for code in &self.keep_audio {
            for c in core::expand_codes(code) {
                ka.insert(c);
            }
        }
        let mut ks = HashSet::new();
        for code in &self.keep_subs {
            for c in core::expand_codes(code) {
                ks.insert(c);
            }
        }
        core::KeepSettings {
            strip_audio: self.strip_audio,
            strip_subs: self.strip_subs,
            keep_audio: ka,
            keep_subs: ks,
            remove_attachments: self.remove_attachments,
            replace: self.replace,
        }
    }

    fn probe_sync(&mut self, path: PathBuf) {
        let Some(tools) = self.tools.clone() else {
            return;
        };
        match core::probe(&tools.ffprobe, &path) {
            Ok(info) => {
                let aud = core::FileInfo::lang_groups(&info, core::TrackKind::Audio)
                    .iter()
                    .map(|(l, n)| if *n > 1 { format!("{l} x{n}") } else { l.clone() })
                    .collect::<Vec<_>>();
                let subs = core::FileInfo::lang_groups(&info, core::TrackKind::Subtitle)
                    .iter()
                    .map(|(l, n)| if *n > 1 { format!("{l} x{n}") } else { l.clone() })
                    .collect::<Vec<_>>();
                let at = if aud.is_empty() { "none".into() } else { aud.join(", ") };
                let st = if subs.is_empty() { "none".into() } else { subs.join(", ") };
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.line(format!("   [audio {at} / subs {st}]  {name}"), egui::Color32::from_gray(150));
                self.info.insert(path, info);
            }
            Err(e) => self.line(format!("   probe failed: {e}"), egui::Color32::from_rgb(220, 80, 70)),
        }
    }

    fn add_path(&mut self, path: PathBuf) {
        if path.to_string_lossy().ends_with(".new.mkv") {
            return;
        }
        if self.queue.contains(&path) {
            return;
        }
        self.queue.push(path.clone());
        self.probe_sync(path);
    }

    fn add_folder(&mut self, folder: PathBuf, recursive: bool) {
        let files = core::collect_mkvs(&folder, recursive);
        let before = self.queue.len();
        for f in files {
            if !self.queue.contains(&f) {
                self.add_path(f);
            }
        }
        let added = self.queue.len() - before;
        self.line(format!("Added {added} file(s) from folder."), DIM);
    }

    fn start(&mut self) {
        if self.running || self.queue.is_empty() {
            if self.queue.is_empty() {
                self.line("Nothing to do - add some MKV files first.", ORANGE);
            }
            return;
        }
        let Some(tools) = self.tools.clone() else {
            self.line("mkvmerge/ffprobe are not available.", egui::Color32::from_rgb(220, 80, 70));
            return;
        };
        if self.strip_audio && self.keep_audio.is_empty() {
            self.line(
                "Audio stripping is on but no audio language is checked - select at least one, or all audio would be dropped.",
                ORANGE,
            );
            return;
        }
        let files: Vec<PathBuf> = self.queue.clone();
        let settings = self.settings();
        let tx = self.tx.clone();
        let abort = Arc::new(AtomicBool::new(false));
        self.abort = abort.clone();
        self.running = true;

        self.line(String::new(), egui::Color32::TRANSPARENT);
        let _ = self.tx.send(LogMsg::Line("========== STRIPPING STARTED ==========".into(), egui::Color32::from_rgb(80, 210, 220)));
        let _ = self.tx.send(LogMsg::Line(
            format!(
                "  audio:   {}",
                if settings.strip_audio { "keep checked languages" } else { "keep all" }
            ),
            egui::Color32::from_rgb(80, 210, 220),
        ));
        let _ = self.tx.send(LogMsg::Line(
            format!("  subs:    {}", if settings.strip_subs { "keep checked languages" } else { "keep all" }),
            egui::Color32::from_rgb(80, 210, 220),
        ));

        let handle = std::thread::spawn(move || {
            let total = files.len();
            let mut done = 0usize;
            let mut saved = 0u64;
            let mut errors = 0usize;
            for (i, path) in files.iter().enumerate() {
                if abort.load(Ordering::Relaxed) {
                    let _ = tx.send(LogMsg::Line("Stopped by user.".into(), ORANGE));
                    break;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let _ = tx.send(LogMsg::Line(format!("[{}/{}] {}", i + 1, total, name), egui::Color32::from_rgb(80, 210, 220)));
                match core::probe(&tools.ffprobe, path) {
                    Err(e) => {
                        let _ = tx.send(LogMsg::Line(format!("   probe failed: {e}"), egui::Color32::from_rgb(220, 80, 70)));
                        errors += 1;
                        continue;
                    }
                    Ok(info) => {
                        match core::process_file(&tools.mkvmerge, &info, &settings) {
                            Ok((core::Outcome::Cleaned, bytes)) => {
                                let desc = core::plan(&info, &settings);
                                let keep = desc.kept_desc.join(", ");
                                let _ = tx.send(LogMsg::Line(
                                    format!("   Audio keeping: {}", if keep.is_empty() { "none".into() } else { keep }),
                                    egui::Color32::WHITE,
                                ));
                                let subs = desc.subs_kept
                                    .iter()
                                    .map(|i| format!("{i}"))
                                    .collect::<Vec<_>>();
                                let _ = tx.send(LogMsg::Line(
                                    format!("   Subs keeping:  {}", if subs.is_empty() { "none".into() } else { subs.join(", ") }),
                                    egui::Color32::WHITE,
                                ));
                                let mb = bytes as f64 / 1_048_576.0;
                                let _ = tx.send(LogMsg::Line(
                                    format!("   Removing {} audio + {} subtitle track(s).", desc.audio_to_remove, desc.subs_to_remove),
                                    egui::Color32::from_rgb(120, 210, 120),
                                ));
                                let _ = tx.send(LogMsg::Line(
                                    format!(
                                        "   {} Space saved: {:.1} MB.",
                                        if settings.replace { "Replaced." } else { "Wrote *.new.mkv." },
                                        mb
                                    ),
                                    egui::Color32::from_rgb(120, 210, 120),
                                ));
                                saved += bytes;
                                done += 1;
                            }
                            Ok((core::Outcome::NoCheckedLanguage, _)) => {
                                let _ = tx.send(LogMsg::Line(
                                    "   None of the checked audio languages present - left untouched.".into(),
                                    ORANGE,
                                ));
                            }
                            Ok((core::Outcome::NothingToRemove, _)) => {
                                let ac = info.audio_count();
                                let sc = info.sub_count();
                                let _ = tx.send(LogMsg::Line(
                                    format!("   Nothing to remove - already matches your keep settings (audio {ac}, subs {sc})."),
                                    egui::Color32::from_gray(130),
                                ));
                                done += 1;
                            }
                            Ok((core::Outcome::Error, _)) => {
                                errors += 1;
                            }
                            Err(e) => {
                                let _ = tx.send(LogMsg::Line(format!("   mkvmerge failed: {e}"), egui::Color32::from_rgb(220, 80, 70)));
                                errors += 1;
                            }
                        }
                    }
                }
            }
            let gb = saved as f64 / 1_073_741_824.0;
            let _ = tx.send(LogMsg::Line(String::new(), egui::Color32::TRANSPARENT));
            let _ = tx.send(LogMsg::Line(
                format!("=== DONE - done: {done}, errors: {errors}, space saved: {gb:.2} GB ==="),
                egui::Color32::from_rgb(80, 210, 220),
            ));
            let _ = tx.send(LogMsg::Done);
        });
        self.worker = Some(handle);
    }

    fn after_remove(&mut self) {
        self.line(format!("Files in queue: {}", self.queue.len()), DIM);
    }

    fn tool_status_line(&self) -> String {
        match &self.tools {
            Some(t) => format!(
                "Tools:  mkvmerge [{}]  ffprobe [{}]",
                if t.mkvmerge.is_file() { "found" } else { "MISSING" },
                if t.ffprobe.is_file() { "found" } else { "MISSING" }
            ),
            None => "Tools: MISSING - see log".to_string(),
        }
    }

    fn selected_file(&self) -> Option<&core::FileInfo> {
        let last = self
            .sel
            .iter()
            .filter(|i| **i < self.queue.len())
            .max()
            .copied();
        if let Some(i) = last {
            return self.info.get(&self.queue[i]);
        }
        self.queue.first().and_then(|p| self.info.get(p))
    }
}

fn apply_theme(cc: &eframe::CreationContext<'_>) {
    let ctx = &cc.egui_ctx;
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.faint_bg_color = PANEL;
    visuals.extreme_bg_color = LIST;
    visuals.widgets.inactive.bg_fill = PANEL;
    visuals.widgets.inactive.fg_stroke.width = 1.0;
    visuals.widgets.hovered.bg_fill = LIGHT;
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(38, 52, 70);
    visuals.selection.bg_fill = BLUE;
    visuals.selection.stroke.color = ORANGE;
    visuals.override_text_color = Some(INK);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(9.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn button(text: &str, fill: egui::Color32, w: f32, h: f32) -> egui::Button<'_> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(egui::Color32::WHITE)
            .size(13.0)
            .strong(),
    )
    .fill(fill)
    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 100, 40)))
    .corner_radius(egui::CornerRadius::same(6))
    .min_size(egui::vec2(w, h))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_log();

        let dropped: Vec<PathBuf> = ui.input(|i| {
            i.raw.dropped_files
                .iter()
                .filter_map(|f| Some(f.path().to_path_buf()))
                .collect()
        });
        if !dropped.is_empty() {
            for p in dropped {
                if p.is_dir() {
                    self.add_folder(p, self.recursive);
                } else if p.extension().map(|e| e.eq_ignore_ascii_case("mkv")).unwrap_or(false) {
                    self.add_path(p);
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("strip-subs")
                            .size(22.0)
                            .strong()
                            .color(ORANGE),
                    );
                    ui.label("Strip subtitle tracks and non-essential audio from MKV. Remux only - no re-encode.");
                });
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 2.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.painter().rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, ORANGE);
                    },
                );
                ui.add_space(8.0);

                // Drop zone (fixed-height strip, NOT full-window)
                let dragging = ui.input(|i| !i.raw.hovered_files.is_empty());
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 56.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(if dragging { egui::Color32::from_rgb(34, 47, 64) } else { LIGHT })
                            .stroke(egui::Stroke::new(
                                1.4,
                                if dragging { ORANGE } else { egui::Color32::from_rgb(255, 142, 58) },
                            ))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(14))
                            .show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        egui::RichText::new("⬳   Drop .mkv file(s) or folders here")
                                            .size(15.0)
                                            .strong()
                                            .color(egui::Color32::from_rgb(190, 198, 210)),
                                    );
                                });
                            });
                    },
                );
                ui.add_space(6.0);

                // Options
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.replace, "Replace originals after success")
                        .on_hover_text("On: the original file is replaced only after the remux succeeds.\nOff: a new \"<name>.new.mkv\" file is written instead.");
                    ui.checkbox(&mut self.remove_attachments, "Remove attachments")
                        .on_hover_text("Remove attached fonts/images etc. from the file (can shrink it).");
                    ui.checkbox(&mut self.recursive, "Scan dropped folders recursively")
                        .on_hover_text("When you drop a folder, also scan all its sub-folders for .mkv files.");
                });
                ui.add_space(4.0);

                // Tools status
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(140, 230, 160), self.tool_status_line());
                    if let Some(err) = &self.tools_error {
                        ui.colored_label(egui::Color32::from_rgb(255, 190, 110), err);
                    }
                });
                ui.add_space(6.0);

                // Two columns
                let button_h = 30.0;
                let cols_h = ui.available_height() - button_h - 14.0 - 120.0;
                let cols_h = cols_h.max(240.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), cols_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.columns(2, |cols| {
                            let w0 = cols[0].available_width();
                            let w1 = cols[1].available_width();
                            // LEFT: keep lists
                            cols[0].vertical(|ui| {
                                ui.set_width(w0);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("AUDIO").strong().color(ORANGE));
                                    ui.checkbox(&mut self.strip_audio, "drop all others, keep checked below")
                                        .on_hover_text("On: remove every audio track EXCEPT the languages you tick below.\nOff: keep all audio.");
                                });
                                ui.add_space(4.0);
                                egui::ScrollArea::vertical()
                                    .id_salt("audio_list")
                                    .max_height(170.0)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.set_width(w0);
                                        for (code, name) in core::LANGUAGES {
                                            let mut checked = self.keep_audio.contains(*code);
                                            if ui
                                                .checkbox(&mut checked, format!("{code} - {name}"))
                                                .on_hover_text("The audio languages to KEEP. First track of each checked language is kept; prefers the file's default-flagged track.")
                                                .changed()
                                            {
                                                if checked {
                                                    self.keep_audio.insert(code.to_string());
                                                } else {
                                                    self.keep_audio.remove(*code);
                                                }
                                            }
                                        }
                                    });

                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("SUBTITLES").strong().color(ORANGE));
                                    ui.checkbox(&mut self.strip_subs, "drop all others, keep checked below")
                                        .on_hover_text("On: remove every subtitle track EXCEPT the languages you tick below.\nOff: keep all subtitles.");
                                });
                                ui.add_space(4.0);
                                egui::ScrollArea::vertical()
                                    .id_salt("sub_list")
                                    .max_height(170.0)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.set_width(w0);
                                        for (code, name) in core::LANGUAGES {
                                            let mut checked = self.keep_subs.contains(*code);
                                            if ui
                                                .checkbox(&mut checked, format!("{code} - {name}"))
                                                .on_hover_text("The subtitle languages to KEEP. First track of each checked language is kept; prefers the file's default-flagged track.")
                                                .changed()
                                            {
                                                if checked {
                                                    self.keep_subs.insert(code.to_string());
                                                } else {
                                                    self.keep_subs.remove(*code);
                                                }
                                            }
                                        }
                                    });
                            });
                            // RIGHT: queue
                            cols[1].vertical(|ui| {
                                ui.set_width(w1);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("FILES").strong().color(ORANGE));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(format!("Files: {}", self.queue.len())).strong().color(DIM));
                                    });
                                });
                                ui.add_space(4.0);
                                let list_h = ui.available_height() - 46.0;

                                egui::ScrollArea::vertical()
                                    .id_salt("file_list")
                                    .max_height(list_h)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.set_width(w1);
                                        for (i, p) in self.queue.iter().enumerate() {
                                            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                            let st = self.sel.contains(&i);
                                            let r = ui.selectable_label(st, format!("  {name}")).on_hover_text(p.display().to_string());
                                            if r.clicked() {
                                                if st {
                                                    self.sel.remove(&i);
                                                } else {
                                                    self.sel.insert(i);
                                                }
                                            }
                                        }
                                    });

                                let bar = ui.horizontal(|ui| {
                                    match self.selected_file() {
                                        Some(info) => {
                                            let fmt = |kind: core::TrackKind| -> String {
                                                let g = info.lang_groups(kind);
                                                if g.is_empty() {
                                                    "none".to_string()
                                                } else {
                                                    g.iter()
                                                        .map(|(l, n)| if *n > 1 { format!("{l} x{n}") } else { l.clone() })
                                                        .collect::<Vec<_>>()
                                                        .join(", ")
                                                }
                                            };
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Sound: {}   |   Subs: {}",
                                                    fmt(core::TrackKind::Audio),
                                                    fmt(core::TrackKind::Subtitle)
                                                ))
                                                .strong()
                                                .color(ORANGE),
                                            );
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "  ({})",
                                                    info.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                                                ))
                                                .color(DIM),
                                            );
                                        }
                                        None => {
                                            ui.label(egui::RichText::new("Sound: ...   |   Subs: ...").color(DIM));
                                        }
                                    }
                                });
                                bar.response.on_hover_text("Audio + subtitle languages in the selected file.");
                            });
                        });
                    },
                );
                ui.add_space(8.0);

                // Footer buttons
                ui.horizontal(|ui| {
                    if ui.add(button("Add folder...", egui::Color32::from_rgb(55, 74, 96), 120.0, button_h)).on_hover_text("Add a whole folder of MKV files.").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            let rec = self.recursive;
                            self.add_folder(dir, rec);
                        }
                    }
                    let can_remove = !self.sel.is_empty();
                    if ui
                        .add_enabled(can_remove, button("Remove selected", egui::Color32::from_rgb(55, 74, 96), 150.0, button_h))
                        .on_hover_text("Remove the selected file(s) from the queue.")
                        .clicked()
                    {
                        let mut rem: Vec<usize> = self.sel.iter().copied().collect();
                        rem.sort_unstable();
                        rem.reverse();
                        for i in rem {
                            if i < self.queue.len() {
                                self.queue.remove(i);
                            }
                        }
                        self.sel.clear();
                        self.after_remove();
                    }
                    if ui.add(button("Clear", egui::Color32::from_rgb(55, 74, 96), 80.0, button_h)).on_hover_text("Clear the whole queue.").clicked() {
                        self.queue.clear();
                        self.sel.clear();
                        self.info.clear();
                        self.after_remove();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.running {
                            if ui.add(button("START", ORANGE, 100.0, button_h)).on_hover_text("Start stripping. Files with nothing to remove are skipped automatically.").clicked() {
                                self.start();
                            }
                        } else {
                            if ui.add(button("Stop", egui::Color32::from_rgb(150, 70, 50), 80.0, button_h)).on_hover_text("Stop after the current file finishes.").clicked() {
                                self.abort.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                });
                ui.add_space(6.0);

                // Log
                egui::Frame::new()
                    .fill(LIST)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 50, 62)))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_min_height(90.0);
                        egui::ScrollArea::vertical()
                            .id_salt("log")
                            .max_height(120.0)
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for (text, color) in &self.log {
                                    if text.is_empty() {
                                        ui.add_space(4.0);
                                    } else {
                                        ui.label(egui::RichText::new(text).monospace().color(*color));
                                    }
                                }
                            });
                    });
            });
    }

    fn on_exit(&mut self) {
        self.abort.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}