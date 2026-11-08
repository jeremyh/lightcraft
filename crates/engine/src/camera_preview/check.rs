//! How close a raw's starting colour comes to its camera JPEG with a given camera profile: the check a profile must
//! pass on photos it wasn't fitted on before it is used (`lightcraft-cli calibrate --check`).

use super::look_fit::{fit_preview_with, look_proxies, without_clipped};
use super::mono::fit_tone_only_with;
use super::pairs::displayed;
use super::proxy::LensGeometry;
use super::tone::{HighlightCheck, highlights};
use super::{CameraLook, D65, HueSat, REC2020, RawImage, Rgb32f, ToneMap};

/// Mean CIEDE2000 between the photo as it starts with `profile` (`None`: its own colour alone) and its camera JPEG,
/// over the look fit's proxy, leaving out pixels with a clipped sensor channel (their colour isn't recorded). A photo
/// whose look isn't accepted is measured as it then renders, with the neutral fallback. `None` without usable proxies
/// (no camera JPEG, another format, nothing finite to compare).
///
/// The photo still fits its own tone and chroma curves, as it does when it opens, so the difference is the colour
/// model's. Measured against the camera's own rendering, not absolute colour accuracy.
pub(crate) fn delta_e_to_camera_jpeg(
    raw: &RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    profile: Option<&crate::camera_profiles::CameraProfile>,
) -> Option<f64> {
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (look, _) = fit_preview_with(raw, bytes, &transform, lens, None, profile);
    let (sensor, reference, clipped) = look_proxies(raw, bytes, &transform, lens, None)?;
    score(look.as_ref(), &sensor, &reference, &clipped)
}

/// The same for the photo as if its camera JPEG were black and white (a monochrome picture style): it fits only its
/// tone from the JPEG, on top of `profile`'s colour, or without one the colour such a photo gets today (the model's
/// spectral matrices, else the neutral fallback). Measured against the colour the JPEG does have: what a camera
/// profile gives the photos whose JPEG can't teach their colour, on photos where the colour is known.
pub(crate) fn delta_e_as_black_and_white(
    raw: &mut RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    profile: Option<&crate::camera_profiles::CameraProfile>,
) -> Option<f64> {
    let original = raw.color.clone();
    if profile.is_none()
        && let Some(camera) = raw.metadata.model.as_deref().and_then(|model| lightcraft_raw::spectral::find(raw.metadata.make.as_deref(), model))
    {
        camera.fill(&mut raw.color);
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let result = (|| {
        let colour = match profile {
            Some(p) => Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())),
            None => None,
        };
        let (sensor, reference, clipped) = look_proxies(raw, bytes, &transform, lens, None)?;
        let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
        let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro, grey: true };
        let unclipped = without_clipped(&sensor, &clipped);
        let look = fit_tone_only_with(unclipped.as_ref().unwrap_or(&sensor), &reference, colour, &bright);
        score(look.as_ref(), &sensor, &reference, &clipped)
    })();
    raw.color = original;
    result
}

/// Mean CIEDE2000 between `sensor` rendered with `look` (`None`: the neutral fallback) and `reference`, leaving out
/// `clipped` pixels.
fn score(look: Option<&CameraLook>, sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<f64> {
    let tone = look.map_or_else(|| ToneMap::new(0.0, 0.0, 0.0), |l| ToneMap::camera(&l.tone, 0.0, 0.0, 0.0));
    let correction = look.and_then(|l| l.hue_sat.as_ref()).and_then(HueSat::new);
    let to_xyz = REC2020.to_xyz();
    let white = D65.to_xyz();
    let lab = |rgb: [f64; 3]| lightcraft_color::difference::lab(to_xyz.apply(rgb.map(|v| v.clamp(0.0, 1.0))), white);
    let (mut sum, mut count) = (0.0, 0usize);
    for ((pixel, target), clipped) in sensor.data.iter().zip(&reference.data).zip(clipped) {
        if *clipped || !pixel.iter().chain(target).all(|v| v.is_finite()) {
            continue;
        }
        let scene = pixel.map(f64::from);
        let scene = look.map_or(scene, |l| l.matrix.apply(scene));
        let scene = correction.as_ref().map_or(scene, |c| c.apply(scene.map(|v| v as f32)).map(f64::from));
        let e = lightcraft_color::difference::ciede2000(lab(displayed(scene, &tone)), lab(target.map(f64::from)));
        if e.is_finite() {
            sum += e;
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f64)
}

/// How a raw's colour starts, with the camera profiles installed ([`crate::camera_profiles::ColourStart`]).
pub(crate) fn colour_start(raw: &RawImage, bytes: &[u8], lens: Option<&LensGeometry>) -> crate::camera_profiles::ColourStart {
    use crate::camera_profiles::ColourStart;
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    if !transform.matrix_is_fallback {
        return ColourStart::OwnMatrices;
    }
    if !super::file_local_look(raw.format) {
        return ColourStart::NotFitted;
    }
    if look_proxies(raw, bytes, &transform, lens, None).is_none() {
        return ColourStart::NoCameraJpeg;
    }
    match super::look_fit::fit_preview(raw, bytes, &transform, lens, None) {
        (Some(_), _) => ColourStart::Fitted,
        (None, true) => ColourStart::BlackAndWhite,
        (None, false) => ColourStart::Rejected,
    }
}
