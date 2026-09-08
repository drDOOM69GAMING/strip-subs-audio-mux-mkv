//! Core logic for strip-subs: track model, probing, planning, processing.
//! Pure Rust - no UI dependencies, so it can be unit-tested and reused by
//! both the GUI and the `--headless` mode.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LANGUAGES: &[(&str, &str)] = &[
    ("eng", "English"),
    ("jpn", "Japanese"),
    ("spa", "Spanish"),
    ("fra", "French"),
    ("deu", "German"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("kor", "Korean"),
    ("zho", "Chinese"),
    ("rus", "Russian"),
    ("ara", "Arabic"),
    ("hin", "Hindi"),
    ("tur", "Turkish"),
    ("pol", "Polish"),
    ("nld", "Dutch"),
    ("swe", "Swedish"),
    ("nor", "Norwegian"),
    ("fin", "Finnish"),
    ("dan", "Danish"),
    ("tha", "Thai"),
    ("vie", "Vietnamese"),
    ("ind", "Indonesian"),
    ("ces", "Czech"),
    ("ell", "Greek"),
    ("heb", "Hebrew"),
    ("ron", "Romanian"),
    ("ukr", "Ukrainian"),
    ("hun", "Hungarian"),
    ("und", "Unknown / untagged"),
];

/// Expand a user-checked language code into the raw codes that can appear in
/// a file. Kept identical to the PowerShell GUI behaviour.
pub fn expand_codes(code: &str) -> Vec<String> {
    match code.trim().to_ascii_lowercase().as_str() {
        "eng" => vec!["eng".to_string(), "en".to_string()],
        "und" => vec!["und".to_string(), String::new()],
        c => vec![c.to_string()],
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub index: usize,
    pub kind: TrackKind,
    pub lang: String,
    pub title: Option<String>,
    pub is_default: bool,
}

#[derive(Clone, Debug)]
pub struct FileInfo {
    pub path: PathBuf,
    pub tracks: Vec<Track>,
}

impl FileInfo {
    pub fn audio(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio)
    }
    pub fn subs(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle)
    }
    pub fn audio_count(&self) -> usize {
        self.audio().count()
    }
    pub fn sub_count(&self) -> usize {
        self.subs().count()
    }
    /// Grouped counts in stream order, e.g. [("eng",2),("jpn",1)].
    pub fn lang_groups(&self, kind: TrackKind) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for t in self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
        {
            let lang = t.lang.clone();
            match out.iter_mut().find(|(l, _)| *l == lang) {
                Some((_, n)) => *n += 1,
                None => out.push((lang, 1)),
            }
        }
        out
    }
}

/// Pull a `key=value` field (up to the next `|`) out of a compact ffprobe line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let pos = line.find(needle.as_str())?;
    let rest = &line[pos + needle.len()..];
    let end = rest.find('|').unwrap_or(rest.len());
    let v = rest[..end].trim();
    if v.is_empty() { None } else { Some(v) }
}

/// Parse `-of compact` ffprobe stream output into Track model.
pub fn parse_ffprobe(lines: &[&str]) -> Vec<Track> {
    let mut tracks = Vec::new();
    for line in lines {
        let ll = line.to_ascii_lowercase();
        let kind = if ll.contains("codec_type=audio") {
            TrackKind::Audio
        } else if ll.contains("codec_type=subtitle") {
            TrackKind::Subtitle
        } else if ll.contains("codec_type=video") {
            TrackKind::Video
        } else {
            continue;
        };
        let index = field(line, "index")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let lang = field(&ll, "language")
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_else(|| "und".to_string());
        let title = field(line, "title")
            .map(|v| v.trim_matches('"').to_string())
            .filter(|v| !v.is_empty());
        let is_default = ll.contains("default=1");
        tracks.push(Track {
            index,
            kind,
            lang,
            title,
            is_default,
        });
    }
    tracks
}

#[derive(Clone, Debug, Default)]
pub struct KeepSettings {
    pub strip_audio: bool,
    pub strip_subs: bool,
    pub keep_audio: HashSet<String>,
    pub keep_subs: HashSet<String>,
    pub remove_attachments: bool,
    pub replace: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkipReason {
    /// None of the checked audio languages is present - file left untouched.
    NoCheckedLanguage,
    /// Already matches the keep settings - nothing to remove.
    NothingToRemove,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub audio_kept: Vec<usize>,
    pub subs_kept: Vec<usize>,
    pub audio_to_remove: usize,
    pub subs_to_remove: usize,
    pub skip: Option<SkipReason>,
    /// Display strings like "2 (eng, default)" for the kept audio tracks.
    pub kept_desc: Vec<String>,
}

pub fn plan(f: &FileInfo, s: &KeepSettings) -> Plan {
    let pick = |tracks: &[Track],
                keep: &HashSet<String>,
                kind: TrackKind|
     -> Vec<usize> {
        let in_keep = |t: &Track| keep.contains(&t.lang.to_ascii_lowercase());
        let mut sel: Vec<usize> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        // Pass 1: prefer the file's default-flagged track.
        for t in tracks {
            if t.kind != kind || !in_keep(t) || !t.is_default || seen.contains(t.lang.as_str()) {
                continue;
            }
            seen.insert(t.lang.as_str());
            sel.push(t.index);
        }
        // Pass 2: first track of each remaining checked language.
        for t in tracks {
            if t.kind != kind || !in_keep(t) || seen.contains(t.lang.as_str()) {
                continue;
            }
            seen.insert(t.lang.as_str());
            sel.push(t.index);
        }
        sel.sort_unstable();
        sel
    };

    let audio_kept = pick(&f.tracks, &s.keep_audio, TrackKind::Audio);
    let subs_kept = pick(&f.tracks, &s.keep_subs, TrackKind::Subtitle);

    let audio_to_remove = if s.strip_audio {
        f.audio_count().saturating_sub(audio_kept.len())
    } else {
        0
    };
    let subs_to_remove = if s.strip_subs {
        f.sub_count().saturating_sub(subs_kept.len())
    } else {
        0
    };

    let no_checked_present = s.strip_audio && f.audio_count() > 0 && audio_kept.is_empty();
    let skip = if no_checked_present {
        Some(SkipReason::NoCheckedLanguage)
    } else if audio_to_remove == 0 && subs_to_remove == 0 {
        Some(SkipReason::NothingToRemove)
    } else {
        None
    };

    let kept_desc = f
        .tracks
        .iter()
        .filter(|t| audio_kept.contains(&t.index))
        .map(|t| {
            let mut d = format!("{} ({}{})", t.index, t.lang, if t.is_default { ", default" } else { "" });
            if let Some(title) = &t.title {
                d.push_str(&format!(" [{}]", title));
            }
            d
        })
        .collect();

    Plan {
        audio_kept,
        subs_kept,
        audio_to_remove,
        subs_to_remove,
        skip,
        kept_desc,
    }
}

/// Probe a file with ffprobe and build FileInfo.
pub fn probe(ffprobe: &Path, path: &Path) -> Result<FileInfo, String> {
    let out = Command::new(ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("stream=index,codec_type:stream_tags=language,title:stream_disposition=default")
        .arg("-of")
        .arg("compact")
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed with {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let tracks = parse_ffprobe(&lines);
    Ok(FileInfo {
        path: path.to_path_buf(),
        tracks,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Cleaned,
    NoCheckedLanguage,
    NothingToRemove,
    #[allow(dead_code)]
    Error,
}

/// Remove-track processing of a single file. Returns outcome and saved bytes.
pub fn process_file(
    mkvmerge: &Path,
    info: &FileInfo,
    s: &KeepSettings,
) -> Result<(Outcome, u64), String> {
    let p = plan(info, s);
    match p.skip {
        Some(SkipReason::NoCheckedLanguage) => return Ok((Outcome::NoCheckedLanguage, 0)),
        Some(SkipReason::NothingToRemove) => return Ok((Outcome::NothingToRemove, 0)),
        None => {}
    }

    let out_file = format!("{}.new.mkv", info.path.display());
    let mut args: Vec<String> = vec!["--output".into(), out_file.clone()];
    if s.strip_audio {
        args.push("--audio-tracks".into());
        args.push(p.audio_kept.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","));
    }
    if s.strip_subs {
        if p.subs_kept.is_empty() {
            args.push("--no-subtitles".into());
        } else {
            args.push("--subtitle-tracks".into());
            args.push(p.subs_kept.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        }
    }
    if s.remove_attachments {
        args.push("--no-attachments".into());
    }
    args.push(info.path.display().to_string());

    let out = Command::new(mkvmerge)
        .args(&args)
        .output()
        .map_err(|e| format!("failed to run mkvmerge: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mkvmerge failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let out_path = PathBuf::from(&out_file);
    let new_len = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    let old_len = std::fs::metadata(&info.path).map(|m| m.len()).unwrap_or(new_len);

    if s.replace {
        let name = info
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| "bad file name".to_string())?;
        std::fs::remove_file(&info.path)
            .map_err(|e| format!("could not remove original: {e}"))?;
        let dest = info.path.parent().unwrap_or(Path::new(".")).join(&name);
        std::fs::rename(&out_path, &dest)
            .map_err(|e| format!("could not rename output: {e}"))?;
    }

    Ok((Outcome::Cleaned, old_len.saturating_sub(new_len)))
}

/// Recursively collect `.mkv` files from a folder.
pub fn collect_mkvs(folder: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e.eq_ignore_ascii_case("mkv")).unwrap_or(false) {
                if !path.to_string_lossy().ends_with(".new.mkv") {
                    out.push(path);
                }
            } else if recursive && path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_lines() -> Vec<&'static str> {
        vec![
            "stream|index=0|codec_type=video|disposition:default=1",
            "stream|index=1|codec_type=audio|disposition:default=0|tag:language=eng|tag:title=English (main)",
            "stream|index=2|codec_type=audio|disposition:default=1|tag:language=eng",
            "stream|index=3|codec_type=audio|disposition:default=0|tag:language=jpn",
            "stream|index=4|codec_type=subtitle|disposition:default=1",
        ]
    }

    #[test]
    fn parses_compact_output() {
        let t = parse_ffprobe(&sample_lines());
        assert_eq!(t.len(), 5);
        assert_eq!(t[2].lang, "eng");
        assert!(t[2].is_default);
        assert!(!t[1].is_default);
        assert_eq!(t[1].title.as_deref(), Some("English (main)"));
        assert_eq!(t[4].kind, TrackKind::Subtitle);
        assert_eq!(t[4].lang, "und");
    }

    #[test]
    fn prefers_default_track() {
        let f = FileInfo {
            path: PathBuf::from("x.mkv"),
            tracks: parse_ffprobe(&sample_lines()),
        };
        let mut s = KeepSettings::default();
        s.strip_audio = true;
        s.keep_audio = expand_codes("eng").into_iter().collect();
        let p = plan(&f, &s);
        assert_eq!(p.audio_kept, vec![2]); // default-flagged eng, not index 1
        assert_eq!(p.audio_to_remove, 2);
        assert!(p.skip.is_none()); // removing eng dup + jpn
    }

    #[test]
    fn nothing_to_remove_skip() {
        // One eng audio + one untagged sub; keep everything checked -> skip.
        let f = FileInfo {
            path: PathBuf::from("x.mkv"),
            tracks: parse_ffprobe(&[
                "stream|index=0|codec_type=video|disposition:default=1",
                "stream|index=1|codec_type=audio|disposition:default=1|tag:language=eng",
                "stream|index=2|codec_type=subtitle|disposition:default=1",
            ]),
        };
        let mut s = KeepSettings {
            strip_audio: true,
            strip_subs: true,
            ..Default::default()
        };
        s.keep_audio = expand_codes("eng").into_iter().collect();
        s.keep_subs = expand_codes("und").into_iter().collect();
        let p = plan(&f, &s);
        assert_eq!(p.skip, Some(SkipReason::NothingToRemove));

        // eng + jpn audio and one untagged sub; keep eng only -> must remove.
        let f2 = FileInfo {
            path: PathBuf::from("y.mkv"),
            tracks: parse_ffprobe(&[
                "stream|index=0|codec_type=video|disposition:default=1",
                "stream|index=1|codec_type=audio|disposition:default=1|tag:language=eng",
                "stream|index=2|codec_type=audio|disposition:default=0|tag:language=jpn",
                "stream|index=3|codec_type=subtitle|disposition:default=1",
            ]),
        };
        let mut s2 = KeepSettings {
            strip_audio: true,
            strip_subs: true,
            ..Default::default()
        };
        s2.keep_audio = expand_codes("eng").into_iter().collect();
        let p2 = plan(&f2, &s2);
        assert_eq!(p2.skip, None);
        assert_eq!(p2.audio_to_remove, 1);
        assert_eq!(p2.subs_to_remove, 1);
    }

    #[test]
    fn no_checked_language_skip() {
        let f = FileInfo {
            path: PathBuf::from("x.mkv"),
            tracks: parse_ffprobe(&sample_lines()),
        };
        let mut s = KeepSettings {
            strip_audio: true,
            ..Default::default()
        };
        s.keep_audio = expand_codes("fra").into_iter().collect();
        let p = plan(&f, &s);
        assert_eq!(p.skip, Some(SkipReason::NoCheckedLanguage));
    }
}