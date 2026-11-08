//! The picture style a raw was shot with: the look the camera rendered its own JPEG in (a Fujifilm film
//! simulation, a Nikon Picture Control). Raws whose colour is learned from their camera JPEGs are grouped by it, so
//! one profile isn't fitted to two different looks.
//!
//! Tag identities and value names come from the public ExifTool tag-name documentation (FujiFilm and Nikon tables);
//! layouts were confirmed on real files by comparing with exiv2's output as a black box. No third-party code was read.
//!
//! **Fujifilm** (the `FUJIFILM` maker note in the RAF's embedded JPEG, or in a camera JPEG):
//!
//! | Tag | Use |
//! |---|---|
//! | `0x1401` FilmMode | the colour film simulation (table in [`fujifilm_film`]) |
//! | `0x1003` Saturation | a black-and-white simulation (Monochrome, Acros, each with a colour filter, or Sepia) replaces the film mode; any other value is a saturation setting |
//!
//! **Nikon** (the `Nikon\0` maker note of a NEF or camera JPEG): tag `0x0023` PictureControlData. Its first four
//! bytes are a version (`0100`, `0200`, `0300`, `0310`…). Versions 1 and 2 hold the Picture Control name in bytes
//! 4–24 and the one it was based on in 24–44; from version 3 both start 4 bytes later (on Z bodies exiv2 reads the
//! name with `0300`/`0310` in front of it: a Z f shot with a custom control named `KP400` reads `0310KP400`). Names
//! are free text for custom controls, so black and white isn't inferred from them.
//!
//! **Olympus / OM System** (the `OLYMPUS` maker note): tag `0x0520` PictureMode of the CameraSettings sub-directory
//! (`0x2020`), its first value (table in [`olympus_mode`]; an E-M10 II shot in Monotone records 256, one in Natural 2).
//!
//! Panasonic bodies such as the GF1 record no picture style a black-and-white shot can be told by, so they give none.
//!
//! Anything else (another make, a missing or malformed tag, an unknown code) gives no style, never a guess.

use lightcraft_tiff::{Tiff, makernote, tags as t};
use serde::{Deserialize, Serialize};

/// The picture style a photo was shot with.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PictureStyle {
    /// The name the camera gives it (`Classic Chrome`, `Acros + R filter`, `STANDARD`, or a custom Picture Control's
    /// own name such as `KP400`).
    pub name: String,
    /// The built-in style a custom one was based on, when the camera records it (Nikon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// `Some(true)` when the style is known to render in black and white (toned or not), `Some(false)` when known
    /// to render in colour, `None` when the file doesn't say (a free-text name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monochrome: Option<bool>,
}

const FUJI_SATURATION: u16 = 0x1003;
const FUJI_FILM_MODE: u16 = 0x1401;
const NIKON_PICTURE_CONTROL: u16 = 0x0023;
const OLYMPUS_CAMERA_SETTINGS: u16 = 0x2020;
const OLYMPUS_PICTURE_MODE: u16 = 0x0520;

/// The picture mode an Olympus `PictureMode` code names, and whether it renders in black and white; `None` for a code
/// the documentation doesn't list.
pub fn olympus_mode(code: u64) -> Option<(&'static str, bool)> {
    Some(match code {
        1 => ("Vivid", false),
        2 => ("Natural", false),
        3 => ("Muted", false),
        4 => ("Portrait", false),
        5 => ("i-Enhance", false),
        6 => ("e-Portrait", false),
        7 => ("Color Creator", false),
        8 => ("Underwater", false),
        256 => ("Monotone", true),
        512 => ("Sepia", true),
        _ => return None,
    })
}

/// The colour film simulation a Fujifilm `FilmMode` code names, or `None` for a code the documentation doesn't list.
pub fn fujifilm_film(code: u64) -> Option<&'static str> {
    Some(match code {
        0x000 => "Provia/Standard",
        0x100 => "Studio Portrait",
        0x110 => "Studio Portrait Enhanced Saturation",
        0x120 => "Astia/Soft",
        0x130 => "Studio Portrait Increased Sharpness",
        0x200 => "Velvia/Vivid",
        0x300 => "Studio Portrait Ex",
        0x400 => "Velvia",
        0x500 => "Pro Neg. Std",
        0x501 => "Pro Neg. Hi",
        0x600 => "Classic Chrome",
        0x700 => "Eterna/Cinema",
        0x800 => "Classic Neg.",
        0x900 => "Eterna Bleach Bypass",
        0xa00 => "Nostalgic Neg.",
        0xb00 => "Reala Ace",
        _ => return None,
    })
}

/// The black-and-white simulation a Fujifilm `Saturation` code names (`None` for a saturation setting or an
/// unknown code).
pub fn fujifilm_monochrome(code: u64) -> Option<&'static str> {
    Some(match code {
        0x300 => "Monochrome",
        0x301 => "Monochrome + R filter",
        0x302 => "Monochrome + Ye filter",
        0x303 => "Monochrome + G filter",
        0x310 => "Sepia",
        0x500 => "Acros",
        0x501 => "Acros + R filter",
        0x502 => "Acros + Ye filter",
        0x503 => "Acros + G filter",
        _ => return None,
    })
}

/// The Exif TIFF stream holding a file's maker note: a TIFF-based raw is its own; a RAF's is in its embedded JPEG,
/// a JPEG's in its APP1 segment. Borrowed where possible: a raw isn't copied.
fn exif_stream(bytes: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    if bytes.starts_with(b"FUJIFILMCCD-RAW") || bytes.starts_with(&[0xff, 0xd8]) {
        return crate::container::embedded(bytes).exif.map(std::borrow::Cow::Owned);
    }
    Tiff::sniff(bytes).map(|_| std::borrow::Cow::Borrowed(bytes))
}

/// The picture style a whole file (raw or camera JPEG) records, or `None` (see the module notes).
pub fn read(bytes: &[u8]) -> Option<PictureStyle> {
    let stream = exif_stream(bytes)?;
    let tiff = Tiff::parse(&stream).ok()?;
    let make = tiff.ifds.first()?.string(t::MAKE)?;
    let e = tiff.exif()?.get(t::MAKER_NOTE)?;
    let note = makernote::parse_makernote(&stream, e.offset, e.count() as u64, tiff.order, &make)?;
    match note.kind {
        makernote::MakerNoteKind::Fujifilm => fujifilm(&note.ifd),
        makernote::MakerNoteKind::NikonV3 => nikon(note.ifd.bytes(NIKON_PICTURE_CONTROL)?),
        makernote::MakerNoteKind::OlympusNew | makernote::MakerNoteKind::OlympusOld => olympus(&stream, &note),
        _ => None,
    }
}

/// The Olympus CameraSettings sub-directory's PictureMode: the sub-directory is an IFD pointer (relative to the note's
/// base) in new-style notes, or stored inline in old-style ones.
fn olympus(stream: &[u8], note: &makernote::MakerNote) -> Option<PictureStyle> {
    let e = note.ifd.get(OLYMPUS_CAMERA_SETTINGS)?;
    let at = match &e.value {
        lightcraft_tiff::Value::Undefined(_) | lightcraft_tiff::Value::Byte(_) => e.offset,
        _ => note.base.checked_add(note.ifd.u64(OLYMPUS_CAMERA_SETTINGS)?)?,
    };
    let opts = lightcraft_tiff::ParseOptions { max_ifds: 1, max_depth: 1, follow_children: false, ..Default::default() };
    let (settings, _) = lightcraft_tiff::parse_ifd_at(stream, at, note.order, note.base, false, &opts).ok()?;
    let (name, monochrome) = olympus_mode(settings.value(OLYMPUS_PICTURE_MODE)?.get_u64(0)?)?;
    Some(PictureStyle { name: name.into(), base: None, monochrome: Some(monochrome) })
}

fn fujifilm(ifd: &lightcraft_tiff::Ifd) -> Option<PictureStyle> {
    if let Some(name) = ifd.value(FUJI_SATURATION).and_then(|v| v.get_u64(0)).and_then(fujifilm_monochrome) {
        return Some(PictureStyle { name: name.into(), base: None, monochrome: Some(true) });
    }
    let name = fujifilm_film(ifd.value(FUJI_FILM_MODE)?.get_u64(0)?)?;
    Some(PictureStyle { name: name.into(), base: None, monochrome: Some(false) })
}

/// A NUL-padded ASCII field of a Picture Control record, trimmed; `None` when empty or not printable ASCII.
fn ascii_field(data: &[u8], start: usize, len: usize) -> Option<String> {
    let field = data.get(start..start.checked_add(len)?)?;
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let text = field.get(..end)?;
    if !text.iter().all(|b| (0x20..0x7f).contains(b)) {
        return None;
    }
    let text = std::str::from_utf8(text).ok()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn nikon(data: &[u8]) -> Option<PictureStyle> {
    let version = data.get(..4)?;
    if !version.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let at = match version.first()? {
        b'0' if matches!(version.get(1)?, b'1' | b'2') => 4,
        b'0' => 8,
        _ => return None,
    };
    let name = ascii_field(data, at, 20)?;
    let base = ascii_field(data, at + 20, 20);
    Some(PictureStyle { name, base, monochrome: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{Entry, Ifd, Value};

    fn fuji_ifd(entries: &[(u16, u64)]) -> Ifd {
        let mut entries: Vec<_> = entries.iter().map(|&(tag, v)| Entry { tag, value: Value::Short(vec![v as u16]), offset: 0 }).collect();
        entries.sort_by_key(|e| e.tag);
        Ifd { entries, ..Ifd::default() }
    }

    #[test]
    fn fujifilm_reads_the_film_simulation_and_lets_black_and_white_replace_it() {
        let style = fujifilm(&fuji_ifd(&[(FUJI_FILM_MODE, 0x600), (FUJI_SATURATION, 0)])).unwrap();
        assert_eq!((style.name.as_str(), style.monochrome), ("Classic Chrome", Some(false)));
        // Acros with a red filter: the film mode (still recorded, or not) doesn't name the look
        let style = fujifilm(&fuji_ifd(&[(FUJI_FILM_MODE, 0x000), (FUJI_SATURATION, 0x501)])).unwrap();
        assert_eq!((style.name.as_str(), style.monochrome), ("Acros + R filter", Some(true)));
        assert_eq!(fujifilm(&fuji_ifd(&[(FUJI_SATURATION, 0x300)])).unwrap().name, "Monochrome");
        // a saturation setting isn't a style; an unknown film code or none at all gives nothing
        assert_eq!(fujifilm(&fuji_ifd(&[(FUJI_FILM_MODE, 0x200), (FUJI_SATURATION, 0x80)])).unwrap().name, "Velvia/Vivid");
        assert_eq!(fujifilm(&fuji_ifd(&[(FUJI_FILM_MODE, 0x777)])), None);
        assert_eq!(fujifilm(&fuji_ifd(&[(FUJI_SATURATION, 0)])), None);
    }

    fn picture_control(version: &[u8; 4], name: &str, base: &str) -> Vec<u8> {
        let at = if version[1] >= b'3' { 8 } else { 4 };
        let mut data = vec![0u8; at + 40 + 10];
        data[..4].copy_from_slice(version);
        if at == 8 {
            data[4..8].copy_from_slice(version);
        }
        data[at..at + name.len()].copy_from_slice(name.as_bytes());
        data[at + 20..at + 20 + base.len()].copy_from_slice(base.as_bytes());
        data
    }

    #[test]
    fn nikon_reads_the_picture_control_name_of_every_version() {
        let style = nikon(&picture_control(b"0310", "KP400", "STANDARD")).unwrap();
        assert_eq!((style.name.as_str(), style.base.as_deref(), style.monochrome), ("KP400", Some("STANDARD"), None));
        let style = nikon(&picture_control(b"0100", "VIVID", "")).unwrap();
        assert_eq!((style.name.as_str(), style.base), ("VIVID", None));
        assert_eq!(nikon(&picture_control(b"0200", "Portra 160NC", "")).unwrap().name, "Portra 160NC");
    }

    #[test]
    fn nikon_refuses_malformed_records() {
        assert_eq!(nikon(b""), None);
        assert_eq!(nikon(b"03"), None);
        assert_eq!(nikon(&picture_control(b"0310", "", "STANDARD")), None, "no name");
        assert_eq!(nikon(b"ABCD\0\0\0\0KP400\0"), None, "not a version");
        let mut binary = picture_control(b"0310", "KP400", "");
        binary[9] = 0xff;
        assert_eq!(nikon(&binary), None, "not text");
        // every truncation of a valid record reads or refuses, never panics
        let full = picture_control(b"0310", "KP400", "STANDARD");
        for n in 0..full.len() {
            let _ = nikon(&full[..n]);
        }
    }

    #[test]
    fn olympus_picture_modes_name_the_look_and_monotone_is_black_and_white() {
        assert_eq!(olympus_mode(2), Some(("Natural", false)));
        assert_eq!(olympus_mode(1), Some(("Vivid", false)));
        assert_eq!(olympus_mode(256), Some(("Monotone", true)));
        assert_eq!(olympus_mode(512), Some(("Sepia", true)));
        assert_eq!(olympus_mode(9), None, "an undocumented code is no style");
    }

    #[test]
    fn whole_files_without_a_style_give_none() {
        assert_eq!(read(b""), None);
        assert_eq!(read(b"FUJIFILMCCD-RAW 0201"), None);
        assert_eq!(read(&[0xff, 0xd8, 0xff, 0xd9]), None);
        // a TIFF with no maker note
        let mut tiff = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        tiff.resize(32, 0);
        assert_eq!(read(&tiff), None);
    }
}
