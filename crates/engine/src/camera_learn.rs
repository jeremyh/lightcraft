//! Learning a camera's colour from the user's own photos (the Profile panel's "Improve colour for this camera…", and
//! `lightcraft-cli calibrate --check`).
//!
//! A raw whose camera LightCraft has no colour data for, and that can't fit its own colour to its camera JPEG (a
//! black-and-white picture style, a rejected fit), starts from approximate colour. Other photos of the same camera
//! can teach it: [`check_group`] fits a camera profile on most of their shooting days and measures the photos of the
//! held-out days ([`crate::camera_profiles::check_photo`]); [`Decision`] says whether the profile is used for every
//! photo, only as a fallback, or not at all.
//!
//! [`CameraLearn`] is the app's job for one camera: it **scans** the library's photos of that camera, and any other
//! folders the user points it at, reading only the first [`HEADER_BYTES`] of each file (camera, day, picture style),
//! so the dialog can say how many usable photos and days there are before anything is decoded. It then **learns**
//! from up to [`MAX_LEARN`] colour photos spread over those days, writes the profiles that pass to the profiles folder
//! (keeping what they replace, for [`CameraLearn::undo`]) and asks the session to read them afresh. Files are only
//! read, never imported or changed. Both steps run off the UI thread with a row in the activity stack.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::Serialize;

use crate::camera_profiles::{CameraProfile, Group, Pool, check_photo};

/// Bytes read from a file to learn its camera, day and picture style: a Nikon keeps its Picture Control record up to
/// about 300 KB in; Fujifilm and Panasonic raws need 64 KB.
pub const HEADER_BYTES: u64 = 1 << 20;
/// The same for an Olympus ORF, whose maker note carries the camera's preview JPEG ahead of its picture mode: up to
/// about 1.5 MB on the 88 ORFs checked (E-1 to E-M10 II).
pub const ORF_HEADER_BYTES: u64 = 4 << 20;
/// A profile is checked on every this-many-th shooting day (in date order).
pub const CHECK_EVERY_DAY: usize = 5;
/// Shooting days a group needs for a check: two held out ([`Pool::split_by_day`]).
pub const MIN_DAYS: usize = CHECK_EVERY_DAY / 2 + CHECK_EVERY_DAY + 1;
/// Photos a profile is fitted on, at least.
pub const MIN_FILES: usize = 5;
/// Colour photos decoded to learn from, at most, spread over the days.
pub const MAX_LEARN: usize = 600;
/// Raw files larger than this are left out (no supported raw comes near it).
const MAX_FILE: u64 = 512 << 20;
/// Raw formats whose colour can be learned (their colour is fitted to their camera JPEG).
pub const RAW_EXTENSIONS: &[&str] = &["arw", "nef", "nrw", "raf", "crw", "rw2", "rwl", "orf"];

/// Whether `path` has one of [`RAW_EXTENSIONS`].
pub fn is_raw_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| RAW_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// What the start of a raw says about it ([`read_header`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    /// Exif `Model`, trimmed.
    pub model: String,
    /// The day it was shot, `year * 10000 + month * 100 + day`.
    pub day: Option<u32>,
    pub style: Option<lightcraft_meta::picture_style::PictureStyle>,
}

/// The camera, day and picture style a raw records, from its first [`HEADER_BYTES`] ([`ORF_HEADER_BYTES`] for an ORF).
pub fn read_header(path: &Path) -> Result<Header, String> {
    use std::io::Read;
    let orf = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("orf"));
    let limit = if orf { ORF_HEADER_BYTES } else { HEADER_BYTES };
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(limit).read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = lightcraft_meta::extract(&bytes);
    let model = meta.model.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).ok_or_else(|| format!("{}: no camera model", path.display()))?;
    Ok(Header {
        model,
        day: meta.capture_time.map(|t| u32::from(t.year) * 10_000 + u32::from(t.month) * 100 + u32::from(t.day)),
        style: lightcraft_meta::picture_style::read(&bytes),
    })
}

/// How a profile did on held-out photos (`(today, profile)` mean CIEDE2000 to each camera JPEG).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub photos: usize,
    pub before: f64,
    pub after: f64,
    pub median_before: f64,
    pub median_after: f64,
    /// Photos the profile brought closer.
    pub closer: usize,
    /// The largest increase (negative: every photo came closer).
    pub worst: f64,
    /// Closer on average and in the median, and closer on more than half of the photos.
    pub better: bool,
}

impl Verdict {
    /// The verdict over `scores` (non-finite ones left out); `None` without any.
    pub fn of(scores: &[(f64, f64)]) -> Option<Verdict> {
        let scores: Vec<(f64, f64)> = scores.iter().copied().filter(|(a, b)| a.is_finite() && b.is_finite()).collect();
        if scores.is_empty() {
            return None;
        }
        let n = scores.len() as f64;
        let median = |f: fn(&(f64, f64)) -> f64| {
            let mut v: Vec<f64> = scores.iter().map(f).collect();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
        };
        let before = scores.iter().map(|s| s.0).sum::<f64>() / n;
        let after = scores.iter().map(|s| s.1).sum::<f64>() / n;
        let (median_before, median_after) = (median(|s| s.0), median(|s| s.1));
        let closer = scores.iter().filter(|s| s.1 < s.0).count();
        let worst = scores.iter().map(|s| s.1 - s.0).fold(f64::NEG_INFINITY, f64::max);
        let better = after < before && median_after < median_before && closer * 2 > scores.len();
        Some(Verdict { photos: scores.len(), before, after, median_before, median_after, closer, worst, better })
    }

    pub fn summary(&self) -> String {
        format!(
            "mean ΔE00 {:.2} → {:.2}, median {:.2} → {:.2}, closer on {} of {}, worst {:+.2}: {}",
            self.before,
            self.after,
            self.median_before,
            self.median_after,
            self.closer,
            self.photos,
            self.worst,
            if self.better { "better" } else { "not better" }
        )
    }
}

/// What a checked profile is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    /// It beat the photos' own colour fits: every photo of its group uses it.
    Everywhere,
    /// It beat only the colour photos get when they can't fit their own (black-and-white JPEGs, rejected fits).
    Fallback,
    /// It didn't help: not written.
    NotUsed,
}

/// One group's check ([`check_group`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCheck {
    /// The group, as "model · style".
    pub group: String,
    pub train_photos: usize,
    pub train_days: usize,
    pub test_days: usize,
    /// As colour photos: their own fit (or today's profile) against the candidate.
    pub colour: Option<Verdict>,
    /// As if their camera JPEG were black and white: today's colour for such a photo against the candidate.
    pub black_and_white: Option<Verdict>,
    pub decision: Decision,
}

/// Fit `group`'s profile on most of its shooting days and measure it on the photos of the others (read with `read`,
/// by the tag they were added with), on up to `workers` threads. `Err` with the reason when the group can't be
/// checked (too few photos or days, no usable fit) or the check was cancelled.
pub fn check_group(
    pool: &Pool,
    group: &Group,
    read: &(dyn Fn(usize) -> Option<Vec<u8>> + Sync),
    workers: usize,
    cancel: &AtomicBool,
) -> Result<GroupCheck, String> {
    let split = match pool.split_by_day(group, CHECK_EVERY_DAY, MIN_FILES) {
        Some(split) => split?,
        None => return Err(format!("needs colour photos from {MIN_DAYS} or more days ({} days)", pool.days(group))),
    };
    let next = AtomicUsize::new(0);
    let checks: Vec<crate::camera_profiles::PhotoCheck> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.max(1))
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    while let Some(&tag) = split.test.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let checked = read(tag).and_then(|bytes| {
                            let _permit = crate::memory::work_gate().acquire(bytes.len().saturating_mul(6));
                            crate::guard::catch("checking a camera profile", || check_photo(&bytes, &split.profile)).ok()?.ok()?
                        });
                        out.extend(checked);
                        crate::memory::release();
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).flatten().collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let colour = Verdict::of(&checks.iter().map(|c| c.colour).collect::<Vec<_>>());
    let black_and_white = Verdict::of(&checks.iter().map(|c| c.black_and_white).collect::<Vec<_>>());
    let decision = match (&colour, &black_and_white) {
        (Some(c), _) if c.better => Decision::Everywhere,
        (_, Some(b)) if b.better => Decision::Fallback,
        _ => Decision::NotUsed,
    };
    Ok(GroupCheck {
        group: group.to_string(),
        train_photos: split.profile.files,
        train_days: split.train_days,
        test_days: split.test_days,
        colour,
        black_and_white,
        decision,
    })
}

/// The profile to write for a checked group: fitted on all of its photos, as a fallback when that is all it beat.
/// `None` when it isn't used.
pub fn profile_for(pool: &Pool, group: &Group, check: &GroupCheck) -> Option<Result<CameraProfile, String>> {
    let profile = match check.decision {
        Decision::NotUsed => return None,
        Decision::Everywhere | Decision::Fallback => pool.fit_group(group)?,
    };
    Some(profile.map(|p| if check.decision == Decision::Fallback { p.as_fallback() } else { p }))
}

/// Photo counts the scan found ([`Report`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    /// Raws of the camera.
    pub photos: usize,
    /// Of those, the ones not known to be shot in a black-and-white style (the ones colour can be learned from: a
    /// camera that doesn't record its picture style counts all; learning leaves out the black-and-white JPEGs among
    /// them).
    pub colour: usize,
    /// Days those photos were shot on.
    pub colour_days: usize,
}

/// Where the scan looked and what it found there.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// A folder path, or empty for the library.
    pub folder: String,
    pub tally: Tally,
}

/// What [`CameraLearn`] is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    #[default]
    Idle,
    Scanning,
    Scanned,
    Learning,
    Done,
    Cancelled,
    Failed,
}

/// The job's state, for the dialog (`cameraColour.status`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The camera (Exif `Model`).
    pub model: String,
    pub stage: Stage,
    /// The library first, then each folder looked in.
    pub places: Vec<Place>,
    /// All places together (a photo found twice counts once).
    pub total: Tally,
    /// Enough colour photos and days to learn ([`MIN_DAYS`]); else what is missing, in `needs`.
    pub enough: bool,
    pub needs: Option<String>,
    /// Files done and to do in the current step.
    pub done: usize,
    pub of: usize,
    /// Per group: how its profile did (after learning).
    pub checks: Vec<GroupCheck>,
    /// Groups that couldn't be checked, and why.
    pub skipped: Vec<(String, String)>,
    /// Profile files written by the last learning, by name.
    pub written: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    day: Option<u32>,
    colour: bool,
}

#[derive(Default)]
struct Shared {
    report: Report,
    candidates: Vec<Candidate>,
    /// Profiles written and what they replaced (`None`: no file before), for [`CameraLearn::undo`]; taken by
    /// [`CameraLearn::pump`] once, which reloads the profiles.
    written: Vec<(PathBuf, Option<Vec<u8>>)>,
    reload: bool,
    running: bool,
}

/// Learning one camera's colour from the user's photos (see the module notes).
#[derive(Default)]
pub struct CameraLearn {
    shared: std::sync::Arc<std::sync::Mutex<Shared>>,
    cancel: std::sync::Arc<AtomicBool>,
    /// The last learning's profiles and what they replaced, until undone.
    undo: Vec<(PathBuf, Option<Vec<u8>>)>,
}

fn lock(shared: &std::sync::Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a set of candidates adds up to.
fn tally<'a>(candidates: impl Iterator<Item = &'a Candidate>) -> Tally {
    let mut t = Tally::default();
    let mut days = std::collections::BTreeSet::new();
    for c in candidates {
        t.photos += 1;
        if c.colour {
            t.colour += 1;
            days.extend(c.day);
        }
    }
    t.colour_days = days.len();
    t
}

/// Whether there is enough to learn from, and what is missing.
fn needs(total: &Tally) -> Option<String> {
    if total.photos > 0 && total.colour == 0 {
        Some("all of these photos are black and white: colour can only be learned from colour photos of this camera".into())
    } else if total.colour_days < MIN_DAYS || total.colour < MIN_FILES * 2 {
        Some(format!("photos from at least {MIN_DAYS} different days are needed, not counting black-and-white ones ({} found)", total.colour_days))
    } else {
        None
    }
}

impl CameraLearn {
    /// The job's state.
    pub fn report(&self) -> Report {
        lock(&self.shared).report.clone()
    }

    pub fn running(&self) -> bool {
        lock(&self.shared).running
    }

    /// Stop the scan or learning in progress (profiles already written stay).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether learning wrote or removed profiles since the last call: the profiles are then read afresh
    /// ([`crate::camera_profiles::reload`]) and the caller drops decoded photos so they open with them.
    pub fn pump(&mut self) -> bool {
        let mut shared = lock(&self.shared);
        if !std::mem::take(&mut shared.reload) {
            return false;
        }
        let written = std::mem::take(&mut shared.written);
        drop(shared);
        if !written.is_empty() {
            self.undo = written;
        }
        crate::camera_profiles::reload();
        true
    }

    /// Put back what the last learning replaced (removing the profiles it added). `Ok(false)` when there is nothing
    /// to undo.
    pub fn undo(&mut self) -> Result<bool, String> {
        if self.running() {
            return Err("wait for the current step to finish".into());
        }
        if self.undo.is_empty() {
            return Ok(false);
        }
        for (path, before) in std::mem::take(&mut self.undo) {
            match before {
                Some(bytes) => lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?,
                None => match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("{}: {e}", path.display())),
                },
            }
        }
        let mut shared = lock(&self.shared);
        shared.report.written.clear();
        shared.reload = true;
        Ok(true)
    }

    /// Look for raws of camera `model`: the library's `library` paths (the first time for this camera) and every
    /// folder in `folders` (with what the earlier scans found kept). Starts a background scan.
    pub fn scan(&mut self, model: &str, library: Vec<PathBuf>, folders: Vec<PathBuf>, activity: &crate::activity::Activity) -> Result<(), String> {
        let model = model.trim().to_owned();
        if model.is_empty() {
            return Err("no camera to learn".into());
        }
        let mut shared = lock(&self.shared);
        if shared.running {
            return Err("wait for the current step to finish".into());
        }
        let fresh = shared.report.model != model;
        if fresh {
            *shared = Shared::default();
            shared.report.model = model.clone();
        }
        shared.running = true;
        shared.report.stage = Stage::Scanning;
        shared.report.error = None;
        drop(shared);
        self.cancel = std::sync::Arc::new(AtomicBool::new(false));
        let (shared, cancel) = (self.shared.clone(), self.cancel.clone());
        let guard = activity.start("camera", "Looking for photos from this camera", crate::activity::Cancel::Flag(cancel.clone()));
        let library = if fresh { library } else { Vec::new() };
        let work = move || {
            let result =
                crate::guard::catch("looking for photos from a camera", || scan_places(&shared, &cancel, &model, fresh, library, folders, &guard));
            let mut s = lock(&shared);
            s.running = false;
            s.report.stage = match result {
                _ if cancel.load(Ordering::Relaxed) => Stage::Cancelled,
                Ok(()) => Stage::Scanned,
                Err(e) => {
                    s.report.error = Some(e.to_string());
                    Stage::Failed
                }
            };
        };
        spawn("lc-camera-scan", work).inspect_err(|_| lock(&self.shared).running = false)
    }

    /// Learn the camera's colour from what the scan found: fit, check and write its profiles in the background into
    /// `directory` (the profiles folder).
    pub fn learn(&mut self, directory: PathBuf, activity: &crate::activity::Activity) -> Result<(), String> {
        let mut shared = lock(&self.shared);
        if shared.running {
            return Err("wait for the current step to finish".into());
        }
        if !matches!(shared.report.stage, Stage::Scanned | Stage::Done | Stage::Cancelled | Stage::Failed) || shared.report.places.is_empty() {
            return Err("look for photos from the camera first".into());
        }
        if let Some(needs) = &shared.report.needs {
            return Err(needs.clone());
        }
        let candidates = spread(&shared.candidates, MAX_LEARN);
        shared.running = true;
        shared.report.stage = Stage::Learning;
        shared.report.checks.clear();
        shared.report.skipped.clear();
        shared.report.written.clear();
        shared.report.error = None;
        drop(shared);
        self.cancel = std::sync::Arc::new(AtomicBool::new(false));
        let (shared, cancel) = (self.shared.clone(), self.cancel.clone());
        let guard = activity.start("camera", "Learning camera colour", crate::activity::Cancel::Flag(cancel.clone()));
        let work = move || {
            let result = crate::guard::catch("learning camera colour", || learn_from(&shared, &cancel, &candidates, &directory, &guard));
            let mut s = lock(&shared);
            s.running = false;
            s.reload = !s.written.is_empty();
            s.report.stage = match result {
                _ if cancel.load(Ordering::Relaxed) => Stage::Cancelled,
                Ok(Ok(())) => Stage::Done,
                Ok(Err(e)) => {
                    s.report.error = Some(e);
                    Stage::Failed
                }
                Err(e) => {
                    s.report.error = Some(e.to_string());
                    Stage::Failed
                }
            };
        };
        spawn("lc-camera-learn", work).inspect_err(|_| lock(&self.shared).running = false)
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new().name(name.into()).spawn(work).map(|_| ()).map_err(|e| format!("could not start: {e}"))
}

/// Up to `max` colour candidates, spread evenly over them in date order (every day keeps a share).
fn spread(candidates: &[Candidate], max: usize) -> Vec<Candidate> {
    let mut colour: Vec<&Candidate> = candidates.iter().filter(|c| c.colour).collect();
    colour.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.path.cmp(&b.path)));
    let n = colour.len();
    if n <= max {
        return colour.into_iter().cloned().collect();
    }
    (0..max).filter_map(|k| colour.get(k * n / max).map(|c| (*c).clone())).collect()
}

/// The scan: the library paths, then each folder's raws, keeping those of `model`.
fn scan_places(
    shared: &std::sync::Mutex<Shared>,
    cancel: &AtomicBool,
    model: &str,
    with_library: bool,
    library: Vec<PathBuf>,
    folders: Vec<PathBuf>,
    guard: &crate::activity::TaskGuard,
) {
    let mut places: Vec<(String, Vec<PathBuf>)> = Vec::new();
    if with_library {
        places.push((String::new(), library.into_iter().filter(|p| is_raw_path(p)).collect()));
    }
    for folder in folders {
        let walked = crate::walk::files_in(&folder, None, crate::walk::Limits::default(), |p| is_raw_path(p) && p.is_file());
        places.push((folder.display().to_string(), walked.files));
    }
    let total: usize = places.iter().map(|(_, files)| files.len()).sum();
    {
        let mut s = lock(shared);
        s.report.done = 0;
        s.report.of = total;
    }
    guard.progress(0, total as u64);
    let mut done = 0;
    for (folder, files) in places {
        let mut found = Vec::new();
        for path in files {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            // (a file this camera can't have shot, or one that can't be read, is passed over)
            if let Ok(h) = read_header(&path)
                && h.model == model
            {
                let colour = h.style.as_ref().is_none_or(|s| s.monochrome != Some(true));
                found.push(Candidate { path, day: h.day, colour });
            }
            done += 1;
            if done % 16 == 0 {
                guard.progress(done as u64, total as u64);
                lock(shared).report.done = done;
            }
        }
        let mut s = lock(shared);
        let place = Place { folder, tally: tally(found.iter()) };
        let known: std::collections::HashSet<PathBuf> = s.candidates.iter().map(|c| c.path.clone()).collect();
        s.candidates.extend(found.into_iter().filter(|c| !known.contains(&c.path)));
        s.report.places.push(place);
        let total = tally(s.candidates.iter());
        s.report.needs = needs(&total);
        s.report.enough = s.report.needs.is_none();
        s.report.total = total;
        s.report.done = done;
    }
}

/// The learning: pool the candidates' colour pairs, check each picture style and then the camera as a whole, write
/// the profiles that pass.
fn learn_from(
    shared: &std::sync::Mutex<Shared>,
    cancel: &AtomicBool,
    candidates: &[Candidate],
    directory: &Path,
    guard: &crate::activity::TaskGuard,
) -> Result<(), String> {
    let read = |tag: usize| -> Option<Vec<u8>> {
        let path = &candidates.get(tag)?.path;
        let len = std::fs::metadata(path).ok()?.len();
        (len <= MAX_FILE).then(|| std::fs::read(path).ok()).flatten()
    };
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    let total = candidates.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    {
        let mut s = lock(shared);
        s.report.done = 0;
        s.report.of = total;
    }
    guard.progress(0, total as u64);
    let pools: Vec<Pool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut pool = Pool::new(true);
                    loop {
                        let tag = next.fetch_add(1, Ordering::Relaxed);
                        if tag >= total || cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Some(bytes) = read(tag) {
                            let _permit = crate::memory::work_gate().acquire(bytes.len().saturating_mul(6));
                            // a file that can't contribute (or a decoder bug in it) is passed over
                            let _ = crate::guard::catch("learning camera colour", || pool.add(&bytes, tag));
                        }
                        crate::memory::release();
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        guard.progress(n as u64, total as u64);
                        lock(shared).report.done = n;
                    }
                    pool
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut pool = Pool::new(true);
    for p in pools {
        pool.merge(p);
    }
    guard.detail("checking on held-out days");
    // each picture style, then the camera as a whole (the profile black-and-white photos and unknown styles use)
    let styles = check_and_write(shared, cancel, &pool, &read, workers, directory, true)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let whole = check_and_write(shared, cancel, &pool.into_by_model(), &read, workers, directory, false)?;
    if styles + whole == 0 {
        lock(shared).report.error = Some("no profile came closer to the camera's colour than the photos have now".into());
    }
    Ok(())
}

/// [`check_group`] and [`profile_for`] for every group of `pool` (`styles`: only the groups of a known style), writing
/// the profiles that pass; returns how many were written.
fn check_and_write(
    shared: &std::sync::Mutex<Shared>,
    cancel: &AtomicBool,
    pool: &Pool,
    read: &(dyn Fn(usize) -> Option<Vec<u8>> + Sync),
    workers: usize,
    directory: &Path,
    styles: bool,
) -> Result<usize, String> {
    let mut written = 0;
    for (group, _) in pool.files() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if styles && group.style.is_none() {
            continue;
        }
        let check = match check_group(pool, &group, read, workers, cancel) {
            Ok(check) => check,
            Err(reason) => {
                lock(shared).report.skipped.push((group.to_string(), reason));
                continue;
            }
        };
        let profile = profile_for(pool, &group, &check);
        lock(shared).report.checks.push(check);
        let Some(profile) = profile else { continue };
        let profile = profile?;
        let path = directory.join(match &profile.style {
            Some(style) => crate::camera_profiles::style_file_name(&profile.model, style),
            None => crate::camera_profiles::file_name(&profile.model),
        });
        let before = std::fs::read(&path).ok();
        let path = crate::camera_profiles::save(&profile, directory)?;
        let mut s = lock(shared);
        if !s.written.iter().any(|(p, _)| p == &path) {
            s.written.push((path.clone(), before));
        }
        s.report.written.push(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_needs_the_mean_the_median_and_most_photos() {
        let v = Verdict::of(&[(5.0, 2.0), (4.0, 2.5), (3.0, 3.5)]).unwrap();
        assert!(v.better && v.closer == 2 && (v.worst - 0.5).abs() < 1e-9);
        // closer on average, but on only one of three photos
        assert!(!Verdict::of(&[(9.0, 1.0), (2.0, 2.5), (2.0, 2.5)]).unwrap().better);
        assert!(Verdict::of(&[]).is_none());
        assert!(Verdict::of(&[(f64::NAN, 1.0)]).is_none(), "non-finite scores are left out");
        assert!(v.summary().ends_with(": better"));
    }

    fn candidate(day: u32, colour: bool) -> Candidate {
        Candidate { path: PathBuf::from(format!("/photos/{day}-{colour}.raf")), day: Some(day), colour }
    }

    #[test]
    fn the_tally_counts_colour_photos_and_their_days() {
        let c = [candidate(1, true), candidate(1, true), candidate(2, true), candidate(3, false)];
        assert_eq!(tally(c.iter()), Tally { photos: 4, colour: 3, colour_days: 2 });
        assert!(needs(&tally(c.iter())).is_some_and(|n| n.contains("8 different days")));
        let black_and_white = [candidate(1, false), candidate(2, false)];
        assert!(needs(&tally(black_and_white.iter())).is_some_and(|n| n.contains("black and white")));
        let enough: Vec<Candidate> = (1..=8).flat_map(|d| [candidate(d, true), candidate(d + 100, true)]).collect();
        assert_eq!(needs(&tally(enough.iter())), None);
    }

    #[test]
    fn learning_spreads_over_the_days_and_skips_black_and_white_photos() {
        let c: Vec<Candidate> =
            (0..100).map(|i| Candidate { path: PathBuf::from(format!("/p/{i:03}.nef")), day: Some(i / 10), colour: i % 7 != 0 }).collect();
        let picked = spread(&c, 20);
        assert_eq!(picked.len(), 20);
        assert!(picked.iter().all(|c| c.colour));
        let days: std::collections::BTreeSet<_> = picked.iter().filter_map(|c| c.day).collect();
        assert_eq!(days.len(), 10, "every day keeps a share");
        assert_eq!(spread(&c[..5], 20).len(), 4);
    }

    #[test]
    fn scanning_and_learning_refuse_what_they_cannot_do() {
        let activity = crate::activity::Activity::default();
        let mut job = CameraLearn::default();
        assert!(job.scan("  ", Vec::new(), Vec::new(), &activity).is_err());
        assert!(job.learn(std::env::temp_dir(), &activity).is_err(), "nothing scanned yet");
        assert_eq!(job.undo(), Ok(false));
        assert!(!job.pump());
    }

    #[test]
    fn a_scan_finds_only_the_cameras_raws_and_undo_restores_the_profiles_folder() {
        let dir = std::env::temp_dir().join(format!("lc-camera-learn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("not-a-raw.nef"), b"junk").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let activity = crate::activity::Activity::default();
        let mut job = CameraLearn::default();
        job.scan("X-T2", vec![dir.join("not-a-raw.nef")], vec![dir.clone()], &activity).unwrap();
        while job.running() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let report = job.report();
        assert_eq!(report.stage, Stage::Scanned);
        assert_eq!(report.places.len(), 2, "the library, then the folder");
        assert_eq!(report.total, Tally::default());
        assert!(!report.enough);
        // undo puts back a replaced profile and removes an added one
        let profiles = dir.join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(profiles.join("A.json"), b"old").unwrap();
        std::fs::write(profiles.join("A.json"), b"new").unwrap();
        std::fs::write(profiles.join("B.json"), b"added").unwrap();
        job.undo = vec![(profiles.join("A.json"), Some(b"old".to_vec())), (profiles.join("B.json"), None)];
        assert_eq!(job.undo(), Ok(true));
        assert_eq!(std::fs::read(profiles.join("A.json")).unwrap(), b"old");
        assert!(!profiles.join("B.json").exists());
        assert!(job.pump(), "the profiles are read afresh after an undo");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
