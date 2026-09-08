//! strip-subs - entry point.
//!
//! Default: launches the egui window.
//! `--headless` runs the same pipeline from the command line (no GUI) and is
//! used by the test harness:
//!
//! ```
//! strip-subs --headless [options] file_or_folder...
//!   --keep-audio eng,jpn    languages to keep (default: eng)
//!   --keep-subs und,eng     subtitle languages to keep (default: none)
//!   --no-strip-audio        keep ALL audio
//!   --no-strip-subs         keep all subtitles
//!   --keep-attachments      don't remove attachments
//!   --no-replace            write "<name>.new.mkv" instead of replacing
//!   --no-recursive          don't scan sub-folders of dropped folders
//! ```

mod app;
mod bundle;
mod core;
mod tools;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use core::KeepSettings;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--headless") {
        let code = headless(args);
        std::process::exit(code);
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([980.0, 800.0])
            .with_min_inner_size([800.0, 640.0])
            .with_title("strip-subs"),
        ..Default::default()
    };
    eframe::run_native(
        "strip-subs",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

fn parse_keep_list(s: &str) -> HashSet<String> {
    s.split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

fn headless(args: Vec<String>) -> i32 {
    let mut strip_audio = true;
    let mut strip_subs = true;
    let mut keep_audio: HashSet<String> =
        core::expand_codes("eng").into_iter().collect();
    let mut keep_subs: HashSet<String> = HashSet::new();
    let mut remove_attachments = true;
    let mut replace = true;
    let mut recursive = true;
    let mut quiet = false;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--headless" => {}
            "--no-strip-audio" => {
                strip_audio = false;
                let mut s = HashSet::new();
                for (c, _) in core::LANGUAGES {
                    s.extend(core::expand_codes(c));
                }
                keep_audio = s;
            }
            "--no-strip-subs" => strip_subs = false,
            "--keep-attachments" => remove_attachments = false,
            "--no-replace" => replace = false,
            "--no-recursive" => recursive = false,
            "--quiet" => quiet = true,
            "--keep-audio" => {
                if let Some(v) = it.next() {
                    let mut s = HashSet::new();
                    for c in parse_keep_list(&v) {
                        s.extend(core::expand_codes(&c));
                    }
                    keep_audio = s;
                }
            }
            "--keep-subs" => {
                if let Some(v) = it.next() {
                    let mut s = HashSet::new();
                    for c in parse_keep_list(&v) {
                        s.extend(core::expand_codes(&c));
                    }
                    keep_subs = s;
                }
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }

    let settings = KeepSettings {
        strip_audio,
        strip_subs,
        keep_audio,
        keep_subs,
        remove_attachments,
        replace,
    };

    let tools = match tools::Tools::obtain() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return 2;
        }
    };

    let mut files: Vec<PathBuf> = Vec::new();
    for p in &paths {
        if p.is_dir() {
            files.extend(core::collect_mkvs(p, recursive));
        } else if p.extension().map(|e| e.eq_ignore_ascii_case("mkv")).unwrap_or(false)
            && !p.to_string_lossy().ends_with(".new.mkv")
        {
            files.push(p.clone());
        }
    }
    if files.is_empty() {
        eprintln!("No .mkv files given.");
        return 2;
    }

    let abort = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let t = tools.clone();
    let s2 = settings.clone();
    let handle = std::thread::spawn(move || {
        run_pipeline(&t, &files, &s2, &abort, &tx);
    });
    let mut done = 0usize;
    let mut errors = 0usize;
    while let Ok(msg) = rx.recv() {
        match msg {
            LogOut::Text(txt) => {
                if !quiet {
                    println!("{txt}");
                }
            }
            LogOut::Summary(d, e) => {
                done = d;
                errors = e;
            }
        }
    }
    let _ = handle.join();
    println!("=== DONE - done: {done}, errors: {errors} ===");
    if errors > 0 { 1 } else { 0 }
}

enum LogOut {
    Text(String),
    Summary(usize, usize),
}

fn run_pipeline(
    tools: &tools::Tools,
    files: &[PathBuf],
    settings: &KeepSettings,
    abort: &AtomicBool,
    tx: &mpsc::Sender<LogOut>,
) {
    let total = files.len();
    let mut done = 0usize;
    let mut errors = 0usize;
    for (i, path) in files.iter().enumerate() {
        if abort.load(Ordering::Relaxed) {
            let _ = tx.send(LogOut::Text("Stopped by user.".to_string()));
            break;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let _ = tx.send(LogOut::Text(format!("[{}/{}] {}", i + 1, total, name)));
        match core::probe(&tools.ffprobe, path) {
            Err(e) => {
                let _ = tx.send(LogOut::Text(format!("   probe failed: {e}")));
                errors += 1;
                continue;
            }
            Ok(info) => match core::process_file(&tools.mkvmerge, &info, settings) {
                Ok((core::Outcome::Cleaned, bytes)) => {
                    let desc = core::plan(&info, settings);
                    let keep = desc.kept_desc.join(", ");
                    let _ = tx.send(LogOut::Text(format!(
                        "   Audio keeping: {}",
                        if keep.is_empty() { "none".to_string() } else { keep }
                    )));
                    let sks = desc
                        .subs_kept
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>();
                    let _ = tx.send(LogOut::Text(format!(
                        "   Subs keeping:  {}",
                        if sks.is_empty() { "none".to_string() } else { sks.join(", ") }
                    )));
                    let _ = tx.send(LogOut::Text(format!(
                        "   Removing {} audio + {} subtitle track(s).",
                        desc.audio_to_remove, desc.subs_to_remove
                    )));
                    let mb = bytes as f64 / 1_048_576.0;
                    let _ = tx.send(LogOut::Text(format!(
                        "   {} Space saved: {:.1} MB.",
                        if settings.replace { "Replaced." } else { "Wrote *.new.mkv." },
                        mb
                    )));
                    done += 1;
                }
                Ok((core::Outcome::NoCheckedLanguage, _)) => {
                    let _ = tx.send(LogOut::Text(
                        "   None of the checked audio languages present - left untouched.".to_string(),
                    ));
                }
                Ok((core::Outcome::NothingToRemove, _)) => {
                    let _ = tx.send(LogOut::Text(format!(
                        "   Nothing to remove - already matches your keep settings (audio {}, subs {}).",
                        info.audio_count(),
                        info.sub_count()
                    )));
                    done += 1;
                }
                Ok((core::Outcome::Error, _)) => errors += 1,
                Err(e) => {
                    let _ = tx.send(LogOut::Text(format!("   mkvmerge failed: {e}")));
                    errors += 1;
                }
            },
        }
    }
    let _ = tx.send(LogOut::Summary(done, errors));
}