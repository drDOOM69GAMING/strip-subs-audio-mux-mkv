//! Embedded copies of the external tools. When the default `bundle` feature is
//! enabled, `mkvmerge.exe` and `ffprobe.exe` must exist next to `Cargo.toml`
//! in a `tools/` directory (copied from MKVToolNix + FFmpeg builds) - they are
//! baked into this executable and extracted to %TEMP% at first run.
//!
//! Licensing note: MKVToolNix is GPLv2 and FFmpeg builds are GPL; distributing
//! this binary therefore distributes those programs, so this project should be
//! published under a GPL-compatible licence with the appropriate notices.

pub const MKVMERGE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/mkvmerge.exe"));
pub const FFPROBE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/ffprobe.exe"));
pub const BUILD_ID: &str = env!("CARGO_PKG_VERSION");