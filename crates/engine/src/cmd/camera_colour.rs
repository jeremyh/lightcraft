//! Learning a camera's colour from the user's photos (Profile panel ▸ Improve colour for this camera…): `cameraColour.*`
//! drive [`crate::camera_learn::CameraLearn`].

use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, photo_id, str_param};
use crate::{Result, Session};

/// The camera of photo `id` (else the active photo), from its file's header.
#[cfg(not(target_arch = "wasm32"))]
fn photo_model(s: &Session, p: &Value, command: &str) -> Result<String> {
    let id = photo_id(p, command)?.or_else(|| s.active()).ok_or_else(|| bad(command, "no photo"))?;
    let photo = s.catalog.photo(id).ok_or_else(|| bad(command, "photo missing"))?;
    let lightcraft_catalog::Source::File { path } = &photo.source else { return Err(bad(command, "the photo has no file")) };
    crate::camera_learn::read_header(std::path::Path::new(path)).map(|h| h.model).map_err(|e| bad(command, e))
}

/// The library's raws whose camera (as the catalog names it, maker then model) ends with `model`.
#[cfg(not(target_arch = "wasm32"))]
fn library_raws(s: &Session, model: &str) -> Vec<std::path::PathBuf> {
    let model = model.to_lowercase();
    s.catalog
        .photos()
        .filter(|ph| ph.in_library() && ph.meta.camera.trim().to_lowercase().ends_with(&model))
        .filter_map(|ph| match &ph.source {
            lightcraft_catalog::Source::File { path } => Some(std::path::PathBuf::from(path)),
            lightcraft_catalog::Source::Demo { .. } => None,
        })
        .filter(|p| crate::camera_learn::is_raw_path(p))
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn scan(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cameraColour.scan";
    let model = match str_param(p, "model") {
        Some(m) => m.trim().to_owned(),
        None => photo_model(s, p, C)?,
    };
    let folders: Vec<std::path::PathBuf> = match p.get("folders") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) if list.len() <= 64 => {
            list.iter().map(|v| v.as_str().map(std::path::PathBuf::from).ok_or_else(|| bad(C, "folders must be paths"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(C, "folders must be a list of at most 64 paths")),
    };
    let library = library_raws(s, &model);
    s.camera_learn.scan(&model, library, folders, &s.activity).map_err(|e| bad(C, e))?;
    Ok(json!({"model": model}))
}

#[cfg(not(target_arch = "wasm32"))]
fn learn(s: &mut Session, _: &Value) -> Result<Value> {
    const C: &str = "cameraColour.learn";
    let directory = crate::camera_profiles::dir().ok_or_else(|| bad(C, "camera profile folder is unavailable"))?;
    s.camera_learn.learn(directory, &s.activity).map_err(|e| bad(C, e))?;
    Ok(Value::Null)
}

#[cfg(target_arch = "wasm32")]
fn scan(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("cameraColour.scan", "learning camera colour needs the photos' files on disk"))
}

#[cfg(target_arch = "wasm32")]
fn learn(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("cameraColour.learn", "learning camera colour needs the photos' files on disk"))
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    let reloaded = s.camera_colour_pump();
    let mut v = serde_json::to_value(s.camera_learn.report()).map_err(|e| bad("cameraColour.status", e.to_string()))?;
    v["running"] = json!(s.camera_learn.running());
    v["reloaded"] = json!(reloaded);
    Ok(v)
}

fn cancel(s: &mut Session, _: &Value) -> Result<Value> {
    s.camera_learn.cancel();
    Ok(Value::Null)
}

fn undo(s: &mut Session, _: &Value) -> Result<Value> {
    let undone = s.camera_learn.undo().map_err(|e| bad("cameraColour.undo", e))?;
    s.camera_colour_pump();
    Ok(json!({"undone": undone}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "cameraColour.scan",
            "Look for Photos from This Camera",
            [],
            None,
            "{model?, id?, folders?: [path]} → {model} — in the background, look for raws of a camera (`model`, else the camera of photo `id` or the active photo): the library's photos of it the first time, then each folder in `folders` (read-only; files are neither imported nor changed). Reads only the start of each file (camera, day, picture style). Poll `cameraColour.status` for what was found",
            super::always,
            scan
        ),
        cmd!(
            "cameraColour.learn",
            "Learn Camera Colour",
            [],
            None,
            "{} — in the background, learn the scanned camera's colour from up to 600 of its colour photos spread over their days: a profile per picture style and one for the camera, each fitted on most days and checked on the photos of the others against their camera JPEGs; profiles that come closer are written to the camera profiles folder (as a fallback when they only beat the colour photos get without a usable camera JPEG). Needs colour photos from 8 or more days. Poll `cameraColour.status`",
            super::always,
            learn
        ),
        cmd!(
            query "cameraColour.status",
            "Camera Colour Learning Status",
            [],
            None,
            "{} → {model, stage: idle|scanning|scanned|learning|done|cancelled|failed, running, places: [{folder ('' = library), tally: {photos, colour, colourDays}}], total, enough, needs?, done, of, checks: [{group, trainPhotos, trainDays, testDays, colour?, blackAndWhite?: {photos, before, after, medianBefore, medianAfter, closer, worst, better}, decision: everywhere|fallback|notUsed}], skipped: [[group, reason]], written: [file], error?, reloaded} — also reads the camera profiles afresh after learning wrote them (`reloaded`)",
            super::always,
            status
        ),
        cmd!("cameraColour.cancel", "Stop Learning Camera Colour", [], None, "{} — stop the scan or learning in progress", super::always, cancel),
        cmd!(
            "cameraColour.undo",
            "Undo Learned Camera Colour",
            [],
            None,
            "{} → {undone} — put back the camera profiles the last learning replaced, and remove the ones it added",
            super::always,
            undo
        ),
    ]
}
