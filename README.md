# strip-subs

Strips subtitle tracks and non-essential audio tracks out of MKV files.
Remux only - the video is never re-encoded, so it is fast and lossless.

A single self-contained Windows executable. `mkvmerge` and `ffprobe` are
embedded inside the binary and extracted to `%TEMP%\strip-subs` on first run -
nothing else needs to be installed.

## Usage

### GUI (default)

Run `strip-subs.exe`. A window opens:

- drop `.mkv` files, or whole folders, onto the window
- tick which audio/subtitle languages to KEEP (or untick "drop all others" to
  keep everything)
- press START

Per language, the file's **default-flagged track is kept**; otherwise the first
track is kept. Files with nothing to remove are skipped automatically, and the
original is replaced only after a successful remux (no `*.new.mkv` leftover).

### Headless

```
strip-subs --headless [options] file_or_folder...

  --keep-audio eng,jpn   languages to keep (default: eng)
  --keep-subs und,eng    subtitle languages to keep (default: none)
  --no-strip-audio       keep ALL audio
  --no-strip-subs        keep all subtitles
  --keep-attachments     don't remove attachments
  --no-replace           write "<name>.new.mkv" instead of replacing
  --no-recursive         don't scan sub-folders of dropped folders
  --quiet                only print the final summary
```

This is the same pipeline the GUI runs and is used by the test harness.

## Building

Requires a Rust toolchain with the GNU target (MSVC build tools are not needed
if you point the linker at a MinGW/w64 distribution that provides `dlltool`):

```
# mkvmerge.exe: from an MKVToolNix install
# ffprobe.exe:  from an FFmpeg build (bin folder)
copy [...]\mkvmerge.exe tools\
copy [...]\ffprobe.exe    tools\

cargo build --release
```

`tools/` must contain both executables before compiling.

To build without embedding (tools are then looked up on PATH, under
`C:\Program Files\MKVToolNix`, and in WinGet's Gyan.FFmpeg package folder):

```
cargo build --release --no-default-features
```

## Testing

- Unit tests: `cargo test`
- Pipeline tests against real files: build, then run
  `strip-subs --headless --keep-audio eng file.mkv` on a copy and check the log.

## License

GPL-2.0-or-later (see LICENSE).

The executable embeds distribution of third-party GPL programs:

- **mkvmerge** - part of MKVToolNix, GPL-2.0 (see https://mkvtoolnix.download)
- **ffprobe** - part of FFmpeg, GPL builds available (see https://ffmpeg.org)

Your use of the program therefore must comply with the GPLv2 and the terms of
those projects.