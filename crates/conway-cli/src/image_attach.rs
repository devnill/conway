//! Terminal image attachment: reading an image off the system clipboard or
//! a file path, bounding its size, sniffing its format, decoding its pixel
//! dimensions, and rendering the one-line chip every surface that shows an
//! attached image (the TUI composer, `conway sessions show`, one-shot's own
//! echo of what it sent) uses -- a SECOND file for the same reason
//! `diff.rs` is: this logic is shared by the TUI (`tui/input.rs`'s Ctrl-V
//! and `@`-path handling) and the headless surfaces (`oneshot.rs`'s
//! `--image`, `commands/sessions.rs`'s replay renderer), none of which may
//! depend on the others.
//!
//! ## No new dependency (C-04)
//!
//! Decoding a PNG/JPEG/WebP header's width/height is ~20 lines of fixed-
//! offset byte parsing per format (this module's own [`decode_dimensions`]);
//! an image crate would pull in a decoder for formats this item never
//! renders a single pixel of. Base64-encoding the bytes for
//! `conway_core::content::ContentBlock::Image::data_base64` is RFC 4648
//! standard alphabet, ~15 lines ([`encode_base64`]) -- nothing in this
//! workspace already depends on a `base64` crate (checked: no crate in this
//! workspace names one), so adding one for a single call site would be a
//! new dependency for arithmetic the standard library already makes trivial
//! to hand-write correctly. The clipboard read is the one piece that
//! legitimately needs an external program (no Rust clipboard access without
//! a new dependency, and the point of C-04 is to prefer a shell-out to an
//! existing system tool over a new crate): `pngpaste`/`osascript` on macOS,
//! `wl-paste`/`xclip` on Linux -- see [`read_clipboard_image`]'s own doc for
//! exactly which, in what order, and how an unavailable toolchain degrades.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The byte bound every attach route enforces, BEFORE base64 encoding (the
/// encoded form the record stores is ~4/3 larger again). 5 MiB is generous
/// for any ordinary screenshot (a full-screen 4K PNG screenshot is
/// typically well under 2 MiB; a hand-picked, information-dense screenshot
/// rarely exceeds 3-4 MiB) while keeping a single attachment from
/// dominating a session's `.jsonl` log, which stores it inline (see
/// `conway_core::log::LogRecord::UserImage`'s own doc for why inline,
/// rather than a side blob store).
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// An image read from the clipboard or a file path, ready to become a
/// [`conway::plugin::ContentBlock::Image`] (the caller's job -- this type
/// has no dependency on `conway`/`conway-core` so it stays reusable from a
/// plain CLI-arg-parsing context with no live `Conway` yet constructed).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageAttachment {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// The `[image #N · WxH · K KB]` (or, dimensions unknown, `[image #N ·
/// K KB]`) chip every surface renders instead of an image's raw bytes.
/// `index` is 1-based. A free function, not only [`ImageAttachment::
/// render_chip`]'s inherent method, because `conway sessions show`'s
/// replay renderer has a persisted `conway::LogRecord::UserImage` (just a
/// byte count, width, and height -- no live `ImageAttachment` to call a
/// method on) rather than one freshly read off a path or the clipboard.
pub fn render_chip(index: usize, byte_len: u64, width: Option<u32>, height: Option<u32>) -> String {
    let kb = byte_len.div_ceil(1024);
    match (width, height) {
        (Some(w), Some(h)) => format!("[image #{index} \u{b7} {w}\u{d7}{h} \u{b7} {kb} KB]"),
        _ => format!("[image #{index} \u{b7} {kb} KB]"),
    }
}

impl ImageAttachment {
    /// `self.bytes.len()` as a `u64`, named for callers that want to read
    /// "the size this chip reports" without reaching into the field
    /// directly.
    pub fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// The `[image #N · WxH · K KB]` (or, dimensions unknown, `[image #N ·
    /// K KB]`) chip every surface renders instead of the raw bytes.
    /// `index` is 1-based, matching how an operator counts their own
    /// attachments. A thin wrapper over the free [`render_chip`] function --
    /// see that function's own doc for why it exists separately (a
    /// persisted `LogRecord::UserImage` has no live `ImageAttachment` to
    /// call this method on).
    pub fn render_chip(&self, index: usize) -> String {
        render_chip(index, self.byte_len(), self.width, self.height)
    }
}

/// Why an attach attempt failed. `Display` is the exact message a caller
/// (the TUI status line, one-shot's stderr, `--image`'s usage error) shows
/// verbatim.
#[derive(Debug, PartialEq)]
pub enum ImageAttachError {
    /// `path` is `None` for a clipboard read.
    TooLarge {
        path: Option<PathBuf>,
        bytes: u64,
    },
    UnsupportedFormat {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        detail: String,
    },
    ClipboardEmpty,
    /// Neither clipboard tool this platform supports is on `PATH`.
    ClipboardUnavailable {
        tried: Vec<&'static str>,
    },
}

impl std::fmt::Display for ImageAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageAttachError::TooLarge { path, bytes } => {
                let mb = *bytes as f64 / (1024.0 * 1024.0);
                let max_mb = MAX_IMAGE_BYTES as f64 / (1024.0 * 1024.0);
                match path {
                    Some(p) => write!(
                        f,
                        "{} is {mb:.1} MB, over the {max_mb:.0} MB attachment limit",
                        p.display()
                    ),
                    None => write!(
                        f,
                        "clipboard image is {mb:.1} MB, over the {max_mb:.0} MB attachment limit"
                    ),
                }
            }
            ImageAttachError::UnsupportedFormat { path } => write!(
                f,
                "{}: not a recognized PNG/JPEG/WebP image",
                path.display()
            ),
            ImageAttachError::Io { path, detail } => {
                write!(f, "{}: {detail}", path.display())
            }
            ImageAttachError::ClipboardEmpty => {
                write!(f, "the clipboard does not hold an image")
            }
            ImageAttachError::ClipboardUnavailable { tried } => write!(
                f,
                "no clipboard image reader available (tried: {}) -- attach by path instead",
                tried.join(", ")
            ),
        }
    }
}

impl std::error::Error for ImageAttachError {}

/// Sniffs `bytes`' leading magic to tell PNG/JPEG/WebP apart -- the
/// authoritative check (never the file extension alone, which a renamed or
/// extensionless clipboard dump would fail). Returns the wire media type
/// `ContentBlock::Image::media_type` expects.
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Decodes `(width, height)` from a sniffed image's own header. `None`
/// when the format-specific structure this function looks for is not
/// where it expects -- callers treat a `None` dimension as "unknown",
/// never as a reason to refuse the attachment (the chip just omits the
/// `WxH` segment; see [`ImageAttachment::render_chip`]).
pub fn decode_dimensions(media_type: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match media_type {
        "image/png" => decode_png_dimensions(bytes),
        "image/jpeg" => decode_jpeg_dimensions(bytes),
        "image/webp" => decode_webp_dimensions(bytes),
        _ => None,
    }
}

/// PNG: an 8-byte signature, then the first chunk is always `IHDR`
/// (13-byte body: width, height, each big-endian `u32`, at fixed offsets
/// 16 and 20 from the start of the file) -- the PNG spec guarantees IHDR
/// is first, so no general chunk walk is needed.
fn decode_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// JPEG: a sequence of `0xFF <marker>` segments, each (except the bare
/// `SOI`/`EOI`/RST markers) followed by a big-endian 2-byte length. Width/
/// height live in the first Start-Of-Frame segment (`0xC0`-`0xCF`, except
/// the DHT/JPG/DAC markers `0xC4`/`0xC8`/`0xCC`, which share the range but
/// are not SOF): 5 bytes into that segment's body (1 byte precision, then
/// height, then width, each big-endian `u16`).
fn decode_jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2; // skip the SOI marker (0xFFD8)
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        // Review round 1, minor finding #3: the JPEG spec explicitly
        // allows "any number of fill bytes" (additional 0xFF bytes) between
        // the 0xFF prefix and the real marker byte -- a real encoder can
        // and does emit these (padding to a byte/word boundary). The
        // pre-fix code assumed the marker was always exactly `bytes[i+1]`,
        // so a run of fill bytes before a segment (most commonly seen
        // right before `SOS`/`EOI`) would misread a `0xFF` fill byte
        // itself as the marker and desync the whole scan. Skip every
        // consecutive `0xFF` here to find the actual marker position `m`.
        let mut m = i + 1;
        while m < bytes.len() && bytes[m] == 0xFF {
            m += 1;
        }
        if m >= bytes.len() {
            return None;
        }
        let marker = bytes[m];
        if marker == 0x00 {
            // `0xFF 0x00` is an escaped literal 0xFF byte inside
            // entropy-coded scan data, never a marker at all.
            i = m + 1;
            continue;
        }
        // Standalone markers with no length/body: skip past just the marker.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i = m + 1;
            continue;
        }
        if m + 8 >= bytes.len() {
            return None;
        }
        let seg_len = u16::from_be_bytes(bytes[m + 1..m + 3].try_into().ok()?) as usize;
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            let height = u16::from_be_bytes(bytes[m + 4..m + 6].try_into().ok()?);
            let width = u16::from_be_bytes(bytes[m + 6..m + 8].try_into().ok()?);
            return Some((width as u32, height as u32));
        }
        if marker == 0xD9 || seg_len < 2 {
            return None;
        }
        i = m + 1 + seg_len;
    }
    None
}

/// WebP: a 12-byte `RIFF....WEBP` container, then exactly one of three
/// chunk layouts this function distinguishes by the 4-byte chunk id at
/// offset 12: simple lossy (`VP8 `, dimensions as two little-endian 14-bit
/// fields at a fixed offset past its own 10-byte frame tag), simple
/// lossless (`VP8L`, a packed 14-bit-each width-1/height-1 bitfield), or
/// extended (`VP8X`, a 24-bit-each width-1/height-1 field). Each decodes a
/// DIFFERENT bit layout -- WebP has no single fixed width/height offset
/// the way PNG/JPEG effectively do.
fn decode_webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Only enough to safely read the chunk-id tag below -- each arm does
    // its OWN length check before indexing any further, so a short/
    // malformed file past this point returns `None` rather than panicking
    // on an out-of-bounds slice (a lesson this function's own lossless arm
    // used to get wrong: a blanket `len() < 30` guard here rejected a
    // genuinely complete, 25-byte lossless file before its own, correctly
    // sized, 25-byte check ever ran).
    if bytes.len() < 16 {
        return None;
    }
    match &bytes[12..16] {
        b"VP8 " => {
            // Lossy: 10-byte frame tag starts at offset 20; the 3-byte
            // start code 0x9D012A sits at the end of it, followed
            // immediately by two little-endian u16s, each with its top 2
            // bits a scaling factor to mask off.
            if bytes.len() < 30 || bytes[23..26] != [0x9D, 0x01, 0x2A] {
                return None;
            }
            let w = u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3FFF;
            let h = u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3FFF;
            Some((w as u32, h as u32))
        }
        b"VP8L" => {
            // Lossless: byte 20 is a fixed 0x2F signature, then a 32-bit
            // little-endian bitfield: 14 bits (width - 1), 14 bits
            // (height - 1), 4 bits alpha/version.
            if bytes.len() < 25 || bytes[20] != 0x2F {
                return None;
            }
            let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            Some((w, h))
        }
        b"VP8X" => {
            // Extended: a 4-byte flags field, 3 reserved bytes, then two
            // 24-bit little-endian (canvas_width - 1)/(canvas_height - 1).
            if bytes.len() < 30 {
                return None;
            }
            let w = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1;
            let h = u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

/// RFC 4648 standard-alphabet base64, with `=` padding -- the shape
/// `conway_core::content::ContentBlock::Image::data_base64` and
/// `conway_core::log::LogRecord::UserImage::data_base64` both expect. See
/// this module's own doc for why this is hand-written rather than a new
/// dependency.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The exact inverse of [`encode_base64`] -- needed by `sessions show`'s
/// chip renderer, which reads an already-persisted `data_base64` string
/// back into a byte count rather than re-deriving it from a live
/// [`ImageAttachment`]. Returns `None` on malformed input (never panics):
/// a corrupt log should report "could not render", not crash the reader.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 3);
    for chunk in bytes.chunks(4) {
        // Each of up to 4 sextets occupies 6 bits of a 24-bit frame, most
        // significant first; a short final group (2 or 3 chars, the `=`-
        // padded tail) leaves the missing trailing sextet(s) as zero,
        // exactly like the padding bits the real encoder would have
        // written.
        let mut sextets = [0u32; 4];
        for (i, &c) in chunk.iter().enumerate() {
            sextets[i] = val(c)? as u32;
        }
        let combined = (sextets[0] << 18) | (sextets[1] << 12) | (sextets[2] << 6) | sextets[3];
        out.push((combined >> 16) as u8);
        if chunk.len() > 2 {
            out.push((combined >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(combined as u8);
        }
    }
    Some(out)
}

/// Loads and validates an image at `path`: checks its size, reads the
/// bytes, sniffs the format (PNG/JPEG/WebP; anything else is
/// [`ImageAttachError::UnsupportedFormat`]), enforces [`MAX_IMAGE_BYTES`],
/// and decodes dimensions (best-effort -- an unrecognized-but-sniffed
/// header still attaches, with `width`/`height: None`).
///
/// **Checks the file's declared size BEFORE reading it** (review round 1,
/// significant finding #2): a multi-gigabyte path named by `--image` (or a
/// future TUI path-attach) used to be read into memory in full via a plain
/// `fs::read`, with [`MAX_IMAGE_BYTES`] only enforced on the ALREADY-
/// BUFFERED result in `attach_from_bytes` (this module's private shared
/// validation core) -- the bound existed but did nothing to stop the
/// allocation it was meant to prevent. `fs::metadata`
/// now rejects an over-bound file by its directory-entry size alone,
/// before a single byte is read. That size can still be stale by the time
/// the read happens (a file that GROWS between the two calls -- a classic
/// TOCTOU window), so the read itself is also capped at `MAX_IMAGE_BYTES +
/// 1` via [`Read::take`]: reading one byte past the bound is enough to
/// prove the file is still too large, without ever buffering more than
/// `MAX_IMAGE_BYTES + 1` bytes no matter how large the file grows mid-read.
pub fn load_image_path(path: &Path) -> Result<ImageAttachment, ImageAttachError> {
    let declared_len = std::fs::metadata(path)
        .map_err(|err| ImageAttachError::Io {
            path: path.to_path_buf(),
            detail: err.to_string(),
        })?
        .len();
    if declared_len > MAX_IMAGE_BYTES {
        return Err(ImageAttachError::TooLarge {
            path: Some(path.to_path_buf()),
            bytes: declared_len,
        });
    }

    let file = std::fs::File::open(path).map_err(|err| ImageAttachError::Io {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| ImageAttachError::Io {
            path: path.to_path_buf(),
            detail: err.to_string(),
        })?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageAttachError::TooLarge {
            path: Some(path.to_path_buf()),
            bytes: bytes.len() as u64,
        });
    }

    attach_from_bytes(bytes, Some(path.to_path_buf()))
}

/// Shared validation core for both [`load_image_path`] and
/// [`read_clipboard_image`]: sniff, bound, decode. `path` is `Some` only
/// for the error-message naming a path route gets; a clipboard read names
/// none.
fn attach_from_bytes(
    bytes: Vec<u8>,
    path: Option<PathBuf>,
) -> Result<ImageAttachment, ImageAttachError> {
    let media_type = sniff_media_type(&bytes).ok_or_else(|| match &path {
        Some(p) => ImageAttachError::UnsupportedFormat { path: p.clone() },
        None => ImageAttachError::UnsupportedFormat {
            path: PathBuf::from("(clipboard)"),
        },
    })?;
    let byte_len = bytes.len() as u64;
    if byte_len > MAX_IMAGE_BYTES {
        return Err(ImageAttachError::TooLarge {
            path,
            bytes: byte_len,
        });
    }
    let (width, height) = decode_dimensions(media_type, &bytes).unzip();
    Ok(ImageAttachment {
        media_type,
        bytes,
        width,
        height,
    })
}

/// Reads an image off the system clipboard.
///
/// **macOS:** tries `pngpaste -` first (a small, widely-installed
/// Homebrew utility -- `brew install pngpaste` -- that writes the
/// clipboard's image as PNG bytes straight to stdout, with no temp file).
/// If `pngpaste` is not on `PATH`, falls back to `osascript`, which ships
/// with every macOS install: it writes the clipboard's `«class PNGf»` data
/// to a temp file (AppleScript has no way to print raw binary to stdout
/// cleanly) that this function then reads and removes.
///
/// **Linux:** tries `wl-paste --type image/png` (Wayland; `wl-clipboard`
/// package) then `xclip -selection clipboard -t image/png -o` (X11).
///
/// **Everywhere else (including a Linux box with neither tool, or a
/// headless macOS CI runner with neither `pngpaste` nor `osascript`
/// usable):** [`ImageAttachError::ClipboardUnavailable`], naming what was
/// tried, so the operator knows exactly what to install -- never a
/// silent no-op.
pub fn read_clipboard_image() -> Result<ImageAttachment, ImageAttachError> {
    let raw = read_clipboard_bytes()?;
    attach_from_bytes(raw, None)
}

#[cfg(target_os = "macos")]
fn read_clipboard_bytes() -> Result<Vec<u8>, ImageAttachError> {
    if let Some(bytes) = run_capture_stdout("pngpaste", &["-"]) {
        if bytes.is_empty() {
            return Err(ImageAttachError::ClipboardEmpty);
        }
        return Ok(bytes);
    }
    if command_exists("osascript") {
        let tmp = std::env::temp_dir().join(format!("conway-clipboard-{}.png", std::process::id()));
        let script = format!(
            "set thePath to POSIX file \"{}\"\n\
             try\n\
             set pngData to the clipboard as «class PNGf»\n\
             set fileRef to open for access thePath with write permission\n\
             write pngData to fileRef\n\
             close access fileRef\n\
             on error\n\
             try\n\
             close access thePath\n\
             end try\n\
             return \"error\"\n\
             end try\n\
             return \"ok\"",
            tmp.display()
        );
        let output = Command::new("osascript").arg("-e").arg(&script).output();
        let ok = matches!(&output, Ok(o) if o.status.success()
            && String::from_utf8_lossy(&o.stdout).trim() == "ok");
        if ok {
            let bytes = std::fs::read(&tmp).unwrap_or_default();
            let _ = std::fs::remove_file(&tmp);
            if bytes.is_empty() {
                return Err(ImageAttachError::ClipboardEmpty);
            }
            return Ok(bytes);
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(ImageAttachError::ClipboardEmpty);
    }
    Err(ImageAttachError::ClipboardUnavailable {
        tried: vec!["pngpaste", "osascript"],
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_clipboard_bytes() -> Result<Vec<u8>, ImageAttachError> {
    if let Some(bytes) = run_capture_stdout("wl-paste", &["--type", "image/png", "--no-newline"]) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    if let Some(bytes) = run_capture_stdout(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    ) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    if command_exists("wl-paste") || command_exists("xclip") {
        return Err(ImageAttachError::ClipboardEmpty);
    }
    Err(ImageAttachError::ClipboardUnavailable {
        tried: vec!["wl-paste", "xclip"],
    })
}

#[cfg(not(unix))]
fn read_clipboard_bytes() -> Result<Vec<u8>, ImageAttachError> {
    Err(ImageAttachError::ClipboardUnavailable { tried: vec![] })
}

/// How long any clipboard shell-out may run before this module gives up on
/// it and kills it (review round 1, minor finding #4). With no bound, a
/// hung clipboard tool -- a stuck `osascript` permission dialog, a wedged
/// Wayland/X11 compositor -- would block the caller indefinitely once this
/// module is wired into the TUI's composer (Ctrl-V). 5 seconds is generous
/// for a local clipboard read (no network involved) while still bounded.
const CLIPBOARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs `program args` and returns its stdout bytes, only if `program`
/// both exists on `PATH`, exits successfully, AND does so within
/// [`CLIPBOARD_TIMEOUT`] -- `None` (never an `Err`) for "this tool isn't
/// the right one to use" in every one of those cases, so callers can try
/// several candidates in order. A tool that IS found but exits non-zero
/// (e.g. `pngpaste -` when the clipboard holds no image) also yields
/// `None` -- indistinguishable, at this layer, from "not installed"; the
/// caller's own `command_exists` check (tried only after every candidate
/// fails) is what turns that ambiguity into the right final error. A
/// timeout kills the child outright rather than leaving it running.
///
/// stdout is drained on its own thread, concurrently with waiting on the
/// child: `std::process::Command`'s pipe has a bounded OS buffer (as low
/// as 64 KB on some platforms), and a clipboard image is routinely larger
/// than that -- without a concurrent reader, a child blocked writing to a
/// full pipe while THIS function is only polling `try_wait` (never
/// reading) would deadlock against its own output, indistinguishable from
/// the hang this timeout exists to bound.
fn run_capture_stdout(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + CLIPBOARD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    };
    let bytes = reader.join().ok()?;
    if status.success() {
        Some(bytes)
    } else {
        None
    }
}

/// Whether `program` resolves on `PATH` -- `which`/`command -v`'s own
/// check, hand-rolled so this module has no new dependency for it.
fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review round 1, minor finding #4: a child that never exits must
    /// still yield `None` (never hang this function forever), and must be
    /// killed near the timeout bound rather than left running.
    #[test]
    fn run_capture_stdout_kills_a_hung_child_after_the_timeout() {
        let start = std::time::Instant::now();
        let result = run_capture_stdout("sh", &["-c", "sleep 30"]);
        let elapsed = start.elapsed();
        assert!(
            result.is_none(),
            "a child that never exits must yield None, not a hang"
        );
        assert!(
            elapsed < CLIPBOARD_TIMEOUT + std::time::Duration::from_secs(5),
            "must be killed near the configured timeout, not left running for the full sleep: \
             {elapsed:?}"
        );
    }

    /// The ordinary, fast-exiting case must still work unchanged: a large
    /// enough output to exceed a typical OS pipe buffer (64 KB) must still
    /// come through whole, proving the concurrent stdout-draining thread
    /// does not truncate or deadlock against a real child.
    #[test]
    fn run_capture_stdout_drains_output_larger_than_a_pipe_buffer() {
        let result = run_capture_stdout("sh", &["-c", "yes | head -c 200000"])
            .expect("a fast, well-behaved child must still succeed");
        assert_eq!(result.len(), 200_000);
    }

    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn fixture_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = PNG_SIG.to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes()); // IHDR chunk length
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, ...
        bytes.extend_from_slice(&[0, 0, 0, 0]); // fake CRC
        bytes
    }

    #[test]
    fn sniffs_png_jpeg_webp_and_rejects_garbage() {
        assert_eq!(sniff_media_type(&fixture_png(1, 1)), Some("image/png"));
        assert_eq!(
            sniff_media_type(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Some("image/jpeg")
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&0u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_media_type(&webp), Some("image/webp"));
        assert_eq!(sniff_media_type(b"not an image"), None);
        assert_eq!(sniff_media_type(&[]), None);
    }

    #[test]
    fn decodes_png_dimensions_from_the_ihdr_chunk() {
        let bytes = fixture_png(1280, 800);
        assert_eq!(decode_dimensions("image/png", &bytes), Some((1280, 800)));
    }

    /// A real (tiny, hand-verified) baseline JPEG: SOI, an APP0 segment,
    /// then an SOF0 (0xC0) segment whose body starts with 1 precision byte
    /// then big-endian height/width.
    #[test]
    fn decodes_jpeg_dimensions_from_the_first_sof_segment() {
        let mut bytes = vec![0xFF, 0xD8]; // SOI
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, len=4, 2 body bytes
                                                                        // SOF0: marker, length (2 + 1 + 2 + 2 + 1 = 8), precision,
                                                                        // height=600, width=800, 1 component (3 bytes, irrelevant here).
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x08, 0x08]);
        bytes.extend_from_slice(&600u16.to_be_bytes());
        bytes.extend_from_slice(&800u16.to_be_bytes());
        bytes.push(0x01);
        assert_eq!(
            decode_dimensions("image/jpeg", &bytes),
            Some((800, 600)),
            "width, then height, in that order"
        );
    }

    /// Review round 1, minor finding #3: the JPEG spec allows any number of
    /// `0xFF` fill bytes between the `0xFF` prefix and a segment's real
    /// marker byte. A real encoder can emit these (e.g. padding before
    /// `SOF0`); the pre-fix scanner misread a fill byte itself as the
    /// marker and desynced.
    #[test]
    fn decodes_jpeg_dimensions_past_a_run_of_fill_bytes_before_the_marker() {
        let mut bytes = vec![0xFF, 0xD8]; // SOI
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, len=4, 2 body bytes
        bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // fill bytes before the next marker
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x08, 0x08]); // SOF0
        bytes.extend_from_slice(&600u16.to_be_bytes());
        bytes.extend_from_slice(&800u16.to_be_bytes());
        bytes.push(0x01);
        assert_eq!(
            decode_dimensions("image/jpeg", &bytes),
            Some((800, 600)),
            "fill bytes before the marker must not desync the scan"
        );
    }

    #[test]
    fn decodes_webp_lossy_dimensions() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8 ");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk size (unused)
        bytes.extend_from_slice(&[0, 0, 0]); // 3-byte frame tag prefix (unused bits)
        bytes.extend_from_slice(&[0x9D, 0x01, 0x2A]); // start code
        bytes.extend_from_slice(&400u16.to_le_bytes()); // width (top 2 bits = scale)
        bytes.extend_from_slice(&300u16.to_le_bytes()); // height
        assert_eq!(decode_dimensions("image/webp", &bytes), Some((400, 300)));
    }

    #[test]
    fn decodes_webp_lossless_dimensions() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8L");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(0x2F);
        // width-1 = 639 (0x27F), height-1 = 479 (0x1DF), packed 14+14 bits LE.
        let bits: u32 = 639 | (479 << 14);
        bytes.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(decode_dimensions("image/webp", &bytes), Some((640, 480)));
    }

    #[test]
    fn unparseable_header_yields_none_dimensions_not_an_error() {
        assert_eq!(decode_dimensions("image/png", &[0x89, b'P']), None);
        assert_eq!(decode_dimensions("image/jpeg", &[0xFF, 0xD8]), None);
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        for sample in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foobar"[..],
            &[0u8, 1, 2, 3, 254, 255][..],
        ] {
            let encoded = encode_base64(sample);
            let decoded = decode_base64(&encoded).expect("valid base64 must decode");
            assert_eq!(decoded, sample, "round trip for {sample:?}");
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn load_image_path_attaches_a_real_png_fixture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("shot.png");
        std::fs::write(&path, fixture_png(1280, 800)).expect("write fixture");
        let attachment = load_image_path(&path).expect("fixture PNG must attach");
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!(attachment.width, Some(1280));
        assert_eq!(attachment.height, Some(800));
    }

    #[test]
    fn load_image_path_rejects_an_unrecognized_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"just some text, not an image").expect("write fixture");
        let err = load_image_path(&path).expect_err("plain text must be refused");
        assert!(matches!(err, ImageAttachError::UnsupportedFormat { .. }));
        assert!(err.to_string().contains("not a recognized"));
    }

    #[test]
    fn load_image_path_refuses_an_oversized_file_and_names_the_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.png");
        let mut bytes = fixture_png(1, 1);
        bytes.resize((MAX_IMAGE_BYTES as usize) + 1, 0);
        std::fs::write(&path, &bytes).expect("write fixture");
        let err = load_image_path(&path).expect_err("oversized file must be refused");
        match &err {
            ImageAttachError::TooLarge { path: p, bytes } => {
                assert_eq!(p.as_deref(), Some(path.as_path()));
                assert!(*bytes > MAX_IMAGE_BYTES);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
        assert!(err.to_string().contains("MB"));
    }

    /// Review round 1, significant finding #2: the size check must happen
    /// BEFORE the file is read into memory, not after. Proven with a
    /// SPARSE file (`File::set_len`, which reports a large length without
    /// actually allocating or writing that many bytes on disk): if
    /// `load_image_path` still read the whole file first (the pre-fix
    /// behavior), reading a sparse file this size would itself allocate a
    /// same-sized `Vec<u8>` in memory before the bound was ever checked --
    /// exactly the unbounded-allocation risk this fix closes. The returned
    /// `TooLarge.bytes` is asserted to equal the file's exact metadata
    /// length, which only the pre-read `fs::metadata` check (not a
    /// post-read `bytes.len()`) could have produced for a file this size.
    #[test]
    fn load_image_path_rejects_an_oversized_file_by_metadata_before_reading_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sparse.png");
        let file = std::fs::File::create(&path).expect("create sparse file");
        let sparse_len = MAX_IMAGE_BYTES * 20; // 100 MB of declared length, ~0 bytes on disk
        file.set_len(sparse_len).expect("set sparse length");
        drop(file);

        let err = load_image_path(&path).expect_err("a sparse file over the bound must refuse");
        match &err {
            ImageAttachError::TooLarge { path: p, bytes } => {
                assert_eq!(p.as_deref(), Some(path.as_path()));
                assert_eq!(
                    *bytes, sparse_len,
                    "must report the file's own declared length, proving the check ran on \
                     metadata rather than on bytes actually read"
                );
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// Review round 1, significant finding #2 (the other half): the read
    /// itself is capped at `MAX_IMAGE_BYTES + 1`, so a file that somehow
    /// grows between the metadata check and the read (a TOCTOU window --
    /// `fs::metadata` reporting a size that is already stale by the time
    /// `File::open`'s read happens) is still caught, and still never
    /// buffers more than one byte past the bound no matter how large the
    /// file actually is. Modeled here with a real (non-sparse) file whose
    /// ACTUAL bytes exceed the bound by one -- the same observable
    /// contract a mid-read growth would hit.
    #[test]
    fn load_image_path_caps_the_read_at_one_byte_past_the_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("grows.png");
        let mut bytes = fixture_png(1, 1);
        bytes.resize((MAX_IMAGE_BYTES + 1) as usize, 0);
        std::fs::write(&path, &bytes).expect("write fixture");

        let err = load_image_path(&path).expect_err("one byte over the bound must refuse");
        match &err {
            ImageAttachError::TooLarge { bytes, .. } => {
                assert_eq!(
                    *bytes,
                    MAX_IMAGE_BYTES + 1,
                    "the capped read must report exactly bound+1, never the file's full size"
                );
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn load_image_path_reports_a_missing_file_as_io_not_a_panic() {
        let err = load_image_path(Path::new("/nonexistent/path/shot.png"))
            .expect_err("a missing file must error, not panic");
        assert!(matches!(err, ImageAttachError::Io { .. }));
    }

    #[test]
    fn chip_renders_with_and_without_known_dimensions() {
        let with_dims = ImageAttachment {
            media_type: "image/png",
            bytes: vec![0; 240 * 1024],
            width: Some(1280),
            height: Some(800),
        };
        assert_eq!(
            with_dims.render_chip(1),
            "[image #1 \u{b7} 1280\u{d7}800 \u{b7} 240 KB]"
        );

        let no_dims = ImageAttachment {
            media_type: "image/jpeg",
            bytes: vec![0; 10],
            width: None,
            height: None,
        };
        assert_eq!(no_dims.render_chip(2), "[image #2 \u{b7} 1 KB]");
    }
}
