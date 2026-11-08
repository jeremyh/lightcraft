//! Camera colour profiles of our own, pooled from many photos of one camera model.
//!
//! A raw file without colour matrices (Sony ARW, Nikon NEF, Fujifilm RAF, Canon CRW) gets its look fitted to its own embedded camera
//! JPEG (`camera_preview`), but one photo shows too little of some colours: a lime shirt covering a
//! few dozen proxy pixels next to a hillside of foliage at the same hue. `lightcraft-cli calibrate`
//! pools the colour pairs of many photos per model and fits one matrix and hue/saturation/value
//! table; photos of that model then only fit their tone and chroma curves.
//!
//! Profiles are JSON files (`<model>.json`, or `<model>@<style>.json` for one picture style, [`style_key`]) in
//! [`dir`]: `LIGHTCRAFT_CAMERA_PROFILES`, else `<config>/camera-profiles`; a local profile replaces the one built in
//! ([`BUNDLED`], from `assets/camera-profiles/`). A fallback profile ([`CameraProfile::fallback_only`]) is used only
//! where a photo can't fit its own colour. They are read once per process; a damaged or hostile file is ignored with a
//! warning. They hold aggregate colour statistics only, never image content. `docs/camera-preview-colour.md`
//! describes how `calibrate --check` decides which kind of profile, if any, a group of photos gets.
use lightcraft_color::Mat3;
use lightcraft_raw::profile::HsvTable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Format version of the profile files.
const VERSION: u32 = 1;
/// Format version of a profile used only where a photo can't fit its own colour ([`CameraProfile::fallback_only`]):
/// LightCraft versions that read only version 1 ignore such a file instead of applying it to every photo.
const VERSION_FALLBACK: u32 = 2;
/// Largest profile file read (a 5 × 72 × 5 table is ~60 KB of JSON).
const MAX_FILE: u64 = 4 << 20;

/// Profiles built into LightCraft (`assets/camera-profiles/`, see `assets/ATTRIBUTION.md`):
/// `(model, JSON)`.
pub const BUNDLED: &[(&str, &str)] = &[
    ("Canon EOS D60", include_str!("../../../assets/camera-profiles/Canon_EOS_D60.json")),
    ("ILCA-99M2", include_str!("../../../assets/camera-profiles/ILCA-99M2.json")),
    ("ILCE-7CR", include_str!("../../../assets/camera-profiles/ILCE-7CR.json")),
    ("ILCE-7M4", include_str!("../../../assets/camera-profiles/ILCE-7M4.json")),
    ("ILCE-7RM5", include_str!("../../../assets/camera-profiles/ILCE-7RM5.json")),
    ("NIKON D5300", include_str!("../../../assets/camera-profiles/NIKON_D5300.json")),
    ("NIKON Z 6_2", include_str!("../../../assets/camera-profiles/NIKON_Z_6_2.json")),
    ("NIKON Z 8", include_str!("../../../assets/camera-profiles/NIKON_Z_8.json")),
    ("X-H2S", include_str!("../../../assets/camera-profiles/X-H2S.json")),
    ("X-T4", include_str!("../../../assets/camera-profiles/X-T4.json")),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraProfile {
    pub version: u32,
    /// The camera model as the files report it (Exif `Model`), e.g. `ILCE-7M4`.
    pub model: String,
    /// Photos and colour pairs the profile was fitted on.
    pub files: usize,
    pub samples: usize,
    /// White-balanced camera RGB (with the baseline exposure) → linear Rec.2020, rows.
    matrix: [[f64; 3]; 3],
    /// Hue/saturation/value correction after `matrix` (linear ProPhoto RGB).
    pub hue_sat: Option<HsvTable>,
    /// The picture style the profile was fitted on ([`style_key`]), `None` for any style. A style profile is saved
    /// as `<model>@<style>.json`, a name LightCraft versions without styles never look for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Used only where a photo can't fit its own colour from its camera JPEG: a black-and-white JPEG (a monochrome
    /// picture style), or a colour fit the acceptance gates reject. A photo whose own fit is accepted keeps it. Set
    /// for a profile that comes closer than the colour such photos get without it (the spectral matrices or the
    /// neutral fallback) but not closer than the photos' own fits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_only: bool,
}

impl CameraProfile {
    pub fn new(model: &str, files: usize, samples: usize, matrix: Mat3, hue_sat: Option<HsvTable>) -> CameraProfile {
        CameraProfile { version: VERSION, model: model.to_owned(), files, samples, matrix: matrix.0, hue_sat, style: None, fallback_only: false }
    }

    /// The profile, used only where a photo can't fit its own colour ([`Self::fallback_only`]).
    pub fn as_fallback(mut self) -> CameraProfile {
        self.fallback_only = true;
        self.version = VERSION_FALLBACK;
        self
    }

    /// The profile for photos shot in picture style `style` only.
    pub fn with_style(mut self, style: Option<String>) -> CameraProfile {
        self.style = style;
        self
    }

    pub fn matrix(&self) -> Mat3 {
        Mat3(self.matrix)
    }

    /// Whether the data is usable (bounded matrix, table shape matching its data).
    fn valid(&self) -> bool {
        let matrix = self.matrix.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0) && Mat3(self.matrix).inverse().is_some();
        let table = self.hue_sat.as_ref().is_none_or(|t| {
            let dims = (1..=4096).contains(&t.hue_divisions) && (2..=4096).contains(&t.sat_divisions) && (1..=4096).contains(&t.val_divisions);
            let len = t.hue_divisions.checked_mul(t.sat_divisions).and_then(|n| n.checked_mul(t.val_divisions));
            dims && len == Some(t.data.len()) && t.data.iter().flatten().all(|v| v.is_finite()) && t.data.iter().all(|e| e[1] >= 0.0 && e[2] >= 0.0)
        });
        let style = self.style.as_ref().is_none_or(|s| !s.trim().is_empty() && s.len() <= 64);
        let version = if self.fallback_only { VERSION_FALLBACK } else { VERSION };
        self.version == version && !self.model.is_empty() && matrix && table && style
    }
}

/// LightCraft's configuration folder (settings, GPU marker, camera profiles); one helper for the app.
pub use crate::config::config_dir;

/// Where camera profiles are read from and written to.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_CAMERA_PROFILES").map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("camera-profiles")))
}

fn safe_name(text: &str) -> String {
    text.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// File name of `model`'s profile: letters, digits, `-` and `_` kept, anything else `_`.
pub fn file_name(model: &str) -> String {
    format!("{}.json", safe_name(model))
}

/// File name of `model`'s profile for picture style `style` (`<model>@<style>.json`, made safe as [`file_name`]).
pub fn style_file_name(model: &str, style: &str) -> String {
    format!("{}@{}.json", safe_name(model), safe_name(style))
}

/// The key a photo's picture style groups and finds profiles by: its name, or `None` for a style known to render in
/// black and white (its camera JPEG has no colour to fit; such a photo takes the colour of its model's any-style
/// profile and its tone from its own JPEG).
pub fn style_key(style: &lightcraft_meta::picture_style::PictureStyle) -> Option<String> {
    (style.monochrome != Some(true)).then(|| style.name.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Read and validate a profile file.
pub fn load(path: &Path) -> Result<CameraProfile, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_FILE {
        return Err(format!("{}: {size} bytes is too large for a camera profile", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes, &path.display().to_string())
}

/// Parse and validate a profile's JSON (`origin` names it in errors).
fn parse(json: &[u8], origin: &str) -> Result<CameraProfile, String> {
    let profile: CameraProfile = serde_json::from_slice(json).map_err(|e| format!("{origin}: {e}"))?;
    if !profile.valid() {
        return Err(format!("{origin}: not a usable camera profile (version {}, model {:?})", profile.version, profile.model));
    }
    Ok(profile)
}

/// The built-in profile for `model`, if any.
fn bundled(model: &str) -> Option<CameraProfile> {
    let (_, json) = BUNDLED.iter().find(|(m, _)| *m == model)?;
    parse(json.as_bytes(), &format!("built-in profile {model}")).inspect_err(|e| eprintln!("lightcraft: ignoring camera profile {e}")).ok()
}

/// Write `profile` to `dir` (created if needed) as `<model>.json`, or `<model>@<style>.json` for a style profile;
/// returns the path.
pub fn save(profile: &CameraProfile, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(match &profile.style {
        Some(style) => style_file_name(&profile.model, style),
        None => file_name(&profile.model),
    });
    let json = serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?;
    // a fresh temp file renamed over the profile: never a fixed `.tmp` name, which a planted link
    // would have written through
    lightcraft_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<CameraProfile>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<CameraProfile>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The profile for camera `model` and any picture style: from [`dir`], else built in; read once per process.
pub fn get(model: &str) -> Option<Arc<CameraProfile>> {
    get_styled(model, None)
}

/// The profile for camera `model` and picture style `style` ([`style_key`]): the local profile for that style, else
/// [`get`]'s. Read once per process.
pub fn get_styled(model: &str, style: Option<&str>) -> Option<Arc<CameraProfile>> {
    let model = model.trim();
    let style = style.map(str::trim).filter(|s| !s.is_empty());
    if model.is_empty() {
        return None;
    }
    let key = format!("{model}\u{0}{}", style.unwrap_or(""));
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let profile = find(dir().as_deref(), model, style);
    cache.insert(key, profile.clone());
    profile
}

/// [`get_styled`] without the cache, profiles read from `directory`: the local profile for `style`, else the local
/// any-style one, else the built-in one.
fn find(directory: Option<&Path>, model: &str, style: Option<&str>) -> Option<Arc<CameraProfile>> {
    let local = |name: String, style: Option<&str>| {
        let path = directory?.join(name);
        if !path.is_file() {
            return None;
        }
        match load(&path) {
            Ok(p) if p.model.trim() == model && p.style.as_deref() == style => Some(Arc::new(p)),
            Ok(p) => {
                eprintln!("lightcraft: ignoring camera profile {}: it is for {:?} {:?}, not {model:?} {style:?}", path.display(), p.model, p.style);
                None
            }
            Err(e) => {
                eprintln!("lightcraft: ignoring camera profile {e}");
                None
            }
        }
    };
    style.and_then(|s| local(style_file_name(model, s), Some(s))).or_else(|| local(file_name(model), None)).or_else(|| bundled(model).map(Arc::new))
}

/// Changes when the profiles folder's contents change (per process): part of the render cache
/// keys, so thumbnails rendered before a profile existed are not reused after.
pub fn cache_key() -> u64 {
    static KEY: OnceLock<u64> = OnceLock::new();
    *KEY.get_or_init(|| cache_key_for(dir().as_deref()))
}

fn cache_key_for(directory: Option<&Path>) -> u64 {
    let entries = directory.and_then(|d| std::fs::read_dir(d).ok());
    let mut files: Vec<(String, u64, std::time::Duration)> = entries
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), meta.len(), modified))
        })
        .collect();
    files.sort();
    let mut h = lightcraft_preview::Hasher128::new();
    for (name, len, modified) in &files {
        h.str(name).u64(*len).u64(modified.as_secs()).u64(u64::from(modified.subsec_nanos()));
    }
    for (model, json) in BUNDLED {
        h.str(model).str(json);
    }
    h.finish().0 as u64
}

/// A group of photos a profile is fitted to: one camera model, and one picture style when grouping by style.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Group {
    pub model: String,
    /// [`style_key`]; `None` for photos without a known colour style, or for every photo when not grouping by style.
    pub style: Option<String>,
}

impl std::fmt::Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.style {
            Some(style) => write!(f, "{} · {style}", self.model),
            None => f.write_str(&self.model),
        }
    }
}

/// One photo's colour pairs, the day it was shot (`year * 10000 + month * 100 + day`, `None` without a capture date)
/// and the caller's tag for it (an index into its file list).
struct Photo {
    tag: usize,
    day: Option<u32>,
    pairs: Vec<([f64; 3], [f64; 3])>,
}

/// Per group (camera model, picture style): the photos read and their colour pairs.
#[derive(Default)]
pub struct Pool {
    by_style: bool,
    groups: HashMap<Group, Vec<Photo>>,
}

/// Most colour pairs kept per photo, so a few busy photos can't dominate a profile.
const PAIRS_PER_FILE: usize = 4000;

/// A profile fitted on some days of a group's photos, to be checked on the photos of the other days.
pub struct Split {
    pub profile: CameraProfile,
    /// Days the profile was fitted on, and the tags of the photos shot on the held-out days.
    pub train_days: usize,
    pub test_days: usize,
    pub test: Vec<usize>,
}

impl Pool {
    /// A pool grouping photos by camera model and, with `by_style`, by picture style.
    pub fn new(by_style: bool) -> Pool {
        Pool { by_style, groups: HashMap::new() }
    }

    /// Add one raw file's colour pairs, tagged `tag`. `Ok(None)` when the file can't contribute (not an ARW, NEF or
    /// RAF without colour matrices, no usable camera JPEG, too little colour).
    pub fn add(&mut self, bytes: &[u8], tag: usize) -> Result<Option<Group>, String> {
        let mut raw = lightcraft_raw::decode(bytes).map_err(|e| e.to_string())?;
        // colour only: geometric lens corrections would stop the sensor proxy from binning (the pairs still follow
        // the warp the camera applied to its JPEG)
        let lens = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        let Some(model) = raw.metadata.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) else { return Ok(None) };
        let style = if self.by_style { lightcraft_meta::picture_style::read(bytes).as_ref().and_then(style_key) } else { None };
        let Some(pairs) = crate::camera_preview::profile_pairs(&raw, bytes, lens.as_ref()) else { return Ok(None) };
        let step = pairs.len().div_ceil(PAIRS_PER_FILE).max(1);
        let day = raw.metadata.capture_time.map(|t| u32::from(t.year) * 10_000 + u32::from(t.month) * 100 + u32::from(t.day));
        let group = Group { model: model.to_owned(), style };
        self.groups.entry(group.clone()).or_default().push(Photo { tag, day, pairs: pairs.into_iter().step_by(step).collect() });
        Ok(Some(group))
    }

    /// Fit a profile per group with at least `min_files` photos.
    pub fn fit(&self, min_files: usize) -> Vec<Result<CameraProfile, String>> {
        let mut groups: Vec<_> = self.groups.iter().collect();
        groups.sort_by(|a, b| a.0.cmp(b.0));
        groups.into_iter().filter(|(_, photos)| photos.len() >= min_files).map(|(group, photos)| fit_photos(group, photos.iter())).collect()
    }

    /// For `group`: a profile fitted on the photos of most of its days, holding out every `every`-th day (in date
    /// order, starting with the middle of the first `every`). A photo is never fitted and checked; nor is any other
    /// photo of its day, since photos of one shoot share their light and subjects and would flatter the profile.
    /// `None` with fewer than 2 held-out days, fewer than `min_files` photos left to fit, or no usable fit.
    pub fn split_by_day(&self, group: &Group, every: usize, min_files: usize) -> Option<Result<Split, String>> {
        let photos = self.groups.get(group)?;
        let every = every.max(2);
        let mut days: Vec<u32> = photos.iter().filter_map(|p| p.day).collect();
        days.sort_unstable();
        days.dedup();
        let held: std::collections::HashSet<u32> = days.iter().copied().skip(every / 2).step_by(every).collect();
        if held.len() < 2 {
            return None;
        }
        // photos without a date can't be placed on a day: they are fitted on, never checked
        let (test, train): (Vec<&Photo>, Vec<&Photo>) = photos.iter().partition(|p| p.day.is_some_and(|d| held.contains(&d)));
        if train.len() < min_files {
            return None;
        }
        Some(fit_photos(group, train.into_iter()).map(|profile| Split {
            profile,
            train_days: days.len() - held.len(),
            test_days: held.len(),
            test: test.iter().map(|p| p.tag).collect(),
        }))
    }

    /// A profile fitted on all of `group`'s photos (`None` for a group the pool doesn't have).
    pub fn fit_group(&self, group: &Group) -> Option<Result<CameraProfile, String>> {
        self.groups.get(group).map(|photos| fit_photos(group, photos.iter()))
    }

    /// Add another pool's photos and pairs (pools filled on separate threads).
    pub fn merge(&mut self, other: Pool) {
        for (group, photos) in other.groups {
            self.groups.entry(group).or_default().extend(photos);
        }
    }

    /// The same photos grouped by camera model alone (the any-style profiles black-and-white photos use).
    pub fn into_by_model(self) -> Pool {
        let mut pool = Pool::new(false);
        for (group, photos) in self.groups {
            pool.groups.entry(Group { model: group.model, style: None }).or_default().extend(photos);
        }
        pool
    }

    /// Photos read per group, in order.
    pub fn files(&self) -> Vec<(Group, usize)> {
        let mut v: Vec<_> = self.groups.iter().map(|(g, photos)| (g.clone(), photos.len())).collect();
        v.sort();
        v
    }

    /// Distinct days the photos of `group` were shot on.
    pub fn days(&self, group: &Group) -> usize {
        let mut days: Vec<u32> = self.groups.get(group).into_iter().flatten().filter_map(|p| p.day).collect();
        days.sort_unstable();
        days.dedup();
        days.len()
    }
}

/// One held-out photo's mean CIEDE2000 to its camera JPEG ([`check_photo`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhotoCheck {
    /// As a colour photo: with the colour it gets today (the profile [`get_styled`] finds for its model and picture
    /// style, else its own colour fit) and with the candidate profile.
    pub colour: (f64, f64),
    /// As if its camera JPEG were black and white, fitting only its tone: with today's colour for such a photo (the
    /// model's spectral matrices or the neutral fallback, or the profile `get_styled` finds) and with the candidate.
    pub black_and_white: (f64, f64),
}

/// How close one raw comes to its camera JPEG with the colour it gets today and with `candidate` ([`PhotoCheck`]).
/// `Ok(None)` when the file can't be measured (no usable camera JPEG, a format with its own colour matrices).
pub fn check_photo(bytes: &[u8], candidate: &CameraProfile) -> Result<Option<PhotoCheck>, String> {
    let mut raw = lightcraft_raw::decode(bytes).map_err(|e| e.to_string())?;
    if lightcraft_raw::color::has_matrix(&raw.color) {
        return Ok(None);
    }
    let lens = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    let style = lightcraft_meta::picture_style::read(bytes);
    let today = raw.metadata.model.as_deref().and_then(|m| get_styled(m, style.as_ref().and_then(style_key).as_deref()));
    let colour = crate::camera_preview::delta_e_to_camera_jpeg(&raw, bytes, lens.as_ref(), today.as_deref())
        .zip(crate::camera_preview::delta_e_to_camera_jpeg(&raw, bytes, lens.as_ref(), Some(candidate)));
    let black_and_white = crate::camera_preview::delta_e_as_black_and_white(&mut raw, bytes, lens.as_ref(), today.as_deref())
        .zip(crate::camera_preview::delta_e_as_black_and_white(&mut raw, bytes, lens.as_ref(), Some(candidate)));
    Ok(colour.zip(black_and_white).map(|(colour, black_and_white)| PhotoCheck { colour, black_and_white }))
}

fn fit_photos<'a>(group: &Group, photos: impl Iterator<Item = &'a Photo>) -> Result<CameraProfile, String> {
    let mut files = 0;
    let mut pairs = Vec::new();
    for photo in photos {
        files += 1;
        pairs.extend_from_slice(&photo.pairs);
    }
    let (matrix, hue_sat) = crate::camera_preview::fit_profile(&pairs).ok_or_else(|| format!("{group}: no usable colour fit"))?;
    Ok(CameraProfile::new(&group.model, files, pairs.len(), matrix, hue_sat).with_style(group.style.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CameraProfile {
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[5.0, 1.2, 1.0]; 8], srgb_value: false };
        CameraProfile::new("ILCE-7M4", 12, 3456, Mat3([[1.6, -0.5, -0.1], [-0.2, 1.3, -0.1], [-0.1, -0.2, 1.3]]), Some(table))
    }

    #[test]
    fn profiles_round_trip_and_reject_bad_files() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profiles-{}", std::process::id()));
        let path = save(&profile(), &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "ILCE-7M4.json");
        assert_eq!(load(&path).unwrap(), profile());
        let bad = |p: CameraProfile| {
            let path = dir.join("bad.json");
            std::fs::write(&path, serde_json::to_vec(&p).unwrap()).unwrap();
            load(&path).is_err()
        };
        let mut p = profile();
        p.matrix[0][0] = f64::NAN;
        assert!(bad(p), "non-finite matrix");
        let mut p = profile();
        p.matrix = [[0.0; 3]; 3];
        assert!(bad(p), "singular matrix");
        let mut p = profile();
        if let Some(t) = p.hue_sat.as_mut() {
            t.data.truncate(3);
        }
        assert!(bad(p), "table data doesn't match its shape");
        let mut p = profile();
        p.version = 99;
        assert!(bad(p), "unknown version");
        std::fs::write(dir.join("junk.json"), b"{not json").unwrap();
        assert!(load(&dir.join("junk.json")).is_err());
        assert!(load(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_profiles_are_valid_and_named_after_their_model() {
        assert!(!BUNDLED.is_empty());
        for (model, json) in BUNDLED {
            let p = parse(json.as_bytes(), model).unwrap();
            assert_eq!(p.model, *model);
            assert!(p.files >= 5 && p.hue_sat.is_some(), "{model}: {} photos", p.files);
            assert_eq!(bundled(model), Some(p));
        }
        assert!(bundled("No Such Camera").is_none());
        // a model name with spaces is stored under its safe file name
        assert!(bundled("Canon EOS D60").is_some_and(|p| p.files > 1000));
        assert_eq!(file_name("Canon EOS D60"), "Canon_EOS_D60.json");
    }

    #[test]
    fn same_second_profile_replacement_changes_render_cache_key() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profile-cache-{}", std::process::id()));
        let mut first = profile();
        first.model = "ILCE-7CR".into();
        let path = save(&first, &dir).unwrap();
        let set_modified = |millis| {
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000) + std::time::Duration::from_millis(millis);
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
            file.metadata().unwrap()
        };
        let before = set_modified(100);
        assert_eq!(load(&path).unwrap(), first);
        let before_key = cache_key_for(Some(&dir));

        let mut second = first;
        second.matrix[0][0] = 1.7;
        save(&second, &dir).unwrap();
        let after = set_modified(350);
        assert_eq!(load(&path).unwrap(), second);
        assert_eq!(before.len(), after.len());
        let before_time = before.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();
        let after_time = after.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();
        assert_eq!(before_time.as_secs(), after_time.as_secs());
        assert_ne!(before_time.subsec_nanos(), after_time.subsec_nanos());
        let after_key = cache_key_for(Some(&dir));
        let _ = std::fs::remove_dir_all(dir);

        assert_ne!(before_key, after_key, "a changed profile must not reuse the previous render cache");
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("ILCE-7M4"), "ILCE-7M4.json");
        assert_eq!(file_name(" DSC-RX100M3 "), "DSC-RX100M3.json");
        assert_eq!(file_name("../../etc/passwd"), "______etc_passwd.json");
        assert_eq!(file_name("Ω 1/2"), "__1_2.json");
    }

    #[test]
    fn style_profiles_have_their_own_safe_file_names_and_black_and_white_has_no_key() {
        assert_eq!(style_file_name("X-T2", "Classic Chrome"), "X-T2@Classic_Chrome.json");
        assert_eq!(style_file_name("NIKON Z f", "../KP400"), "NIKON_Z_f@___KP400.json");
        let style = |name: &str, monochrome| lightcraft_meta::picture_style::PictureStyle { name: name.into(), base: None, monochrome };
        assert_eq!(style_key(&style("Classic Chrome", Some(false))).as_deref(), Some("Classic Chrome"));
        assert_eq!(style_key(&style("KP400", None)).as_deref(), Some("KP400"));
        assert_eq!(style_key(&style("Acros", Some(true))), None);
        assert_eq!(style_key(&style("  ", None)), None);
    }

    #[test]
    fn a_style_profile_wins_over_the_any_style_one_and_is_saved_beside_it() {
        let dir = std::env::temp_dir().join(format!("lc-camera-profiles-style-{}", std::process::id()));
        let mut any = profile();
        any.model = "TEST-CAMERA".into();
        let vivid = any.clone().with_style(Some("Vivid".into()));
        let mut vivid = vivid;
        vivid.matrix[0][0] = 1.7;
        assert_eq!(save(&any, &dir).unwrap().file_name().unwrap(), "TEST-CAMERA.json");
        assert_eq!(save(&vivid, &dir).unwrap().file_name().unwrap(), "TEST-CAMERA@Vivid.json");
        assert_eq!(load(&dir.join("TEST-CAMERA@Vivid.json")).unwrap(), vivid);
        let found = |style| find(Some(&dir), "TEST-CAMERA", style).map(|p| (*p).clone());
        assert_eq!(found(Some("Vivid")), Some(vivid.clone()));
        assert_eq!(found(Some("Standard")), Some(any.clone()), "no profile for the style: the any-style one");
        assert_eq!(found(None), Some(any.clone()));
        // a file under a style's name that holds another style's profile is not used for it
        std::fs::copy(dir.join("TEST-CAMERA@Vivid.json"), dir.join("TEST-CAMERA@Portrait.json")).unwrap();
        assert_eq!(found(Some("Portrait")), Some(any));
        assert_eq!(find(Some(&dir), "OTHER-CAMERA", Some("Vivid")), None);
        // a built-in profile is used when there is no local one, for every style
        assert!(find(Some(&dir), "ILCE-7M4", Some("Vivid")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fallback_profile_is_version_2_and_older_versions_reject_it() {
        let fallback = profile().as_fallback();
        let json = serde_json::to_vec(&fallback).unwrap();
        let back = parse(&json, "fallback").unwrap();
        assert!(back.fallback_only && back.version == 2);
        // the flag and the version go together: a version 1 file can't be a fallback, nor a version 2 file an override
        let mut p = fallback.clone();
        p.version = 1;
        assert!(parse(&serde_json::to_vec(&p).unwrap(), "v1 fallback").is_err());
        let mut p = profile();
        p.version = 2;
        assert!(parse(&serde_json::to_vec(&p).unwrap(), "v2 override").is_err());
        // a profile without the field is an override, as every file written before it
        let plain = serde_json::to_string(&profile()).unwrap();
        assert!(!plain.contains("fallback_only") && !parse(plain.as_bytes(), "plain").unwrap().fallback_only);
    }

    /// A pool of `days.len()` photos (one per entry, shot on that day) whose pairs come from one camera matrix.
    fn pool_of(days: &[Option<u32>]) -> (Pool, Group) {
        let camera = Mat3([[1.5, -0.4, -0.1], [-0.2, 1.4, -0.2], [0.0, -0.3, 1.3]]);
        let group = Group { model: "TEST-CAMERA".into(), style: Some("Standard".into()) };
        let mut pool = Pool::new(true);
        for (tag, day) in days.iter().enumerate() {
            let pairs = (0..400)
                .map(|i| {
                    let t = (i * 7 + tag * 13) as f64;
                    let x = [0.05 + 0.4 * (t * 0.37).sin().abs(), 0.05 + 0.4 * (t * 0.53).cos().abs(), 0.05 + 0.4 * (t * 0.71).sin().abs()];
                    (x, camera.apply(x))
                })
                .collect();
            pool.groups.entry(group.clone()).or_default().push(Photo { tag, day: *day, pairs });
        }
        (pool, group)
    }

    #[test]
    fn a_split_by_day_never_fits_and_checks_photos_of_the_same_day() {
        // 12 days with 2 photos each, and one photo without a date
        let mut days: Vec<Option<u32>> = (0..24).map(|i| Some(20240101 + i / 2)).collect();
        days.push(None);
        let (pool, group) = pool_of(&days);
        let split = pool.split_by_day(&group, 5, 5).unwrap().unwrap();
        // every fifth day from the third: days 2 and 7 (0-based) are held out
        assert_eq!((split.train_days, split.test_days), (10, 2));
        assert_eq!(split.test, vec![4, 5, 14, 15]);
        assert_eq!(split.profile.files, 25 - 4, "the undated photo is fitted on, never checked");
        assert_eq!(split.profile.style.as_deref(), Some("Standard"));
        assert_eq!(pool.days(&group), 12);
        assert_eq!(pool.fit_group(&group).unwrap().unwrap().files, 25);
    }

    #[test]
    fn a_split_needs_two_held_out_days_and_enough_photos_left() {
        // 6 days: only one is held out
        let (pool, group) = pool_of(&(0..12).map(|i| Some(20240101 + i / 2)).collect::<Vec<_>>());
        assert!(pool.split_by_day(&group, 5, 5).is_none());
        // 7 days: still one held out (the third); 8 days hold out two
        let (pool, group) = pool_of(&(0..7).map(|i| Some(20240101 + i)).collect::<Vec<_>>());
        assert!(pool.split_by_day(&group, 5, 5).is_none());
        let (pool, group) = pool_of(&(0..8).map(|i| Some(20240101 + i)).collect::<Vec<_>>());
        assert_eq!(pool.split_by_day(&group, 5, 5).unwrap().unwrap().test, vec![2, 7]);
        // two held out, but too few photos left to fit
        assert!(pool.split_by_day(&group, 5, 7).is_none());
        assert!(pool.split_by_day(&Group { model: "NONE".into(), style: None }, 5, 5).is_none());
    }

    #[test]
    fn pool_skips_files_that_cannot_contribute() {
        let mut pool = Pool::new(true);
        assert!(pool.add(b"not a raw file", 0).is_err());
        assert!(pool.fit(1).is_empty());
        assert!(pool.files().is_empty());
    }
}
