//! Locate and prepare mkvmerge/ffprobe for use. When built with the default
//! `bundle` feature the executables are embedded in the binary and extracted
//! to a cache folder under %TEMP% once per build.

use std::path::{Path, PathBuf};

#[cfg(feature = "bundle")]
use crate::bundle;

#[derive(Clone, Debug)]
pub struct Tools {
    pub mkvmerge: PathBuf,
    pub ffprobe: PathBuf,
    pub bundled: bool,
}

impl Tools {
    pub fn obtain() -> Result<Tools, String> {
        #[cfg(feature = "bundle")]
        {
            if let Some(t) = extract_bundled() {
                return Ok(t);
            }
        }
        external()
    }
}

#[cfg(feature = "bundle")]
fn extract_bundled() -> Option<Tools> {
    use std::io::Write;
    let base = std::env::temp_dir()
        .join("strip-subs")
        .join(bundle::BUILD_ID);
    let write = |name: &str, bytes: &[u8], check: bool| -> Option<PathBuf> {
        let p = base.join(name);
        if check && p.is_file() {
            if let Ok(md) = std::fs::metadata(&p) {
                if md.len() as usize == bytes.len() {
                    return Some(p);
                }
            }
        }
        std::fs::create_dir_all(&base).ok()?;
        let mut f = std::fs::File::create(&p).ok()?;
        f.write_all(bytes).ok()?;
        f.flush().ok()?;
        Some(p)
    };
    let mkvmerge = write("mkvmerge.exe", bundle::MKVMERGE, true)?;
    let ffprobe = write("ffprobe.exe", bundle::FFPROBE, true)?;
    Some(Tools {
        mkvmerge,
        ffprobe,
        bundled: true,
    })
}

fn try_path(p: &str) -> Option<PathBuf> {
    let pb = PathBuf::from(p);
    if pb.is_file() {
        return Some(pb);
    }
    // PATH lookup
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(p);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn external() -> Result<Tools, String> {
    let mkvmerge = try_path("mkvmerge.exe")
        .or_else(|| try_path(r"C:\Program Files\MKVToolNix\mkvmerge.exe"));
    let ffprobe = {
        let mut found = try_path("ffprobe.exe");
        if found.is_none() {
            // Scan WinGet package folders for a Gyan FFmpeg build.
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let root = Path::new(&local)
                    .join("Microsoft").join("WinGet").join("Packages");
                if let Ok(read) = std::fs::read_dir(&root) {
                    let mut pkg = read.flatten().filter(|e| e.path().is_dir())
                        .map(|e| e.path())
                        .collect::<Vec<_>>();
                    pkg.sort();
                    for dir in pkg {
                        if !dir.file_name().map(|n| n.to_string_lossy().starts_with("Gyan.")).unwrap_or(false) {
                            continue;
                        }
                        if let Some(pb) = find_under(&dir, "ffprobe.exe") {
                            found = Some(pb);
                            break;
                        }
                    }
                }
            }
        }
        found
    };
    match (mkvmerge, ffprobe) {
        (Some(m), Some(f)) => Ok(Tools {
            mkvmerge: m,
            ffprobe: f,
            bundled: false,
        }),
        _ => Err("mkvmerge and ffprobe were not found. Rebuild the app without the bundle feature is not "
            .to_string()
            + "possible - install MKVToolNix and FFmpeg, or run a bundled build."),
    }
}

fn find_under(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let mut found = Vec::new();
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().map(|n| n == name).unwrap_or(false) {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found.into_iter().next()
}