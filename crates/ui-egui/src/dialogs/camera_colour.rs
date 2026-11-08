//! Improve Camera Colour (Profile panel ▸ "Improve colour for this camera…"): what the scan of the camera's photos
//! found, a folder to look in too, Learn, and what learning did. The work is the engine's `cameraColour.*` job; this
//! only shows its report and starts its steps.

use lightcraft_engine::camera_learn::{Decision, Report, Stage};
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::text_button;

/// Whether a photo with this colour source is offered the dialog: its colour is approximate (no colour data for its
/// camera), or a black-and-white camera JPEG left it on the spectral matrices, which a learned profile beat on most
/// cameras checked.
pub fn offered(source: &lightcraft_pipeline::ColourSource) -> bool {
    use lightcraft_pipeline::ColourBasis;
    matches!(source.basis, ColourBasis::Neutral) || (source.black_and_white && matches!(source.basis, ColourBasis::Spectral))
}

/// One line for what a place holds.
fn place_line(folder: &str, tally: &lightcraft_engine::camera_learn::Tally) -> String {
    // (`colour` counts every photo not known to be black and white: some cameras don't record their picture style)
    let found = crate::i18n::tr_format!("{n} photos from {days} days", n = tally.colour, days = tally.colour_days);
    let found = if tally.photos > tally.colour {
        crate::i18n::tr_format!("{found}, and {bw} black-and-white ones it can't learn from", found = found, bw = tally.photos - tally.colour)
    } else {
        found
    };
    if folder.is_empty() {
        crate::i18n::tr_format!("In your library: {found}", found = found)
    } else {
        crate::i18n::tr_format!("In {folder}: {found}", folder = folder, found = found)
    }
}

/// What learning decided for one group, in a line.
fn check_line(check: &lightcraft_engine::camera_learn::GroupCheck) -> String {
    let closer = |v: &Option<lightcraft_engine::camera_learn::Verdict>| {
        v.as_ref().map(|v| crate::i18n::tr_format!("closer on {closer} of {n} test photos", closer = v.closer, n = v.photos)).unwrap_or_default()
    };
    match check.decision {
        Decision::Everywhere => {
            crate::i18n::tr_format!("{group}: used for all its photos ({closer})", group = check.group, closer = closer(&check.colour))
        }
        Decision::Fallback => crate::i18n::tr_format!(
            "{group}: used for photos that can't take their colour from their camera JPEG ({closer})",
            group = check.group,
            closer = closer(&check.black_and_white)
        ),
        Decision::NotUsed => crate::i18n::tr_format!("{group}: not used (no closer than now)", group = check.group),
    }
}

fn progress(ui: &mut egui::Ui, text: String, report: &Report) {
    ui.label(text);
    let fraction = if report.of > 0 { report.done as f32 / report.of as f32 } else { 0.0 };
    ui.add(egui::ProgressBar::new(fraction).text(format!("{} / {}", report.done, report.of)));
}

pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let report = app.session.camera_learn.report();
    let running = app.session.camera_learn.running();
    let dim = |s: &str| egui::RichText::new(s.to_owned()).font(t.font(12.0)).color(t.text_dim);
    ui.label(dim(crate::i18n::tr(
        "LightCraft can learn this camera's colour from your colour photos taken with it, by comparing each raw with the JPEG the camera stored inside it. Photos are only read: nothing is imported, changed or uploaded.",
    )));
    ui.add_space(4.0);
    if !report.model.is_empty() {
        ui.strong(&report.model);
    }
    match report.stage {
        Stage::Scanning => progress(ui, crate::i18n::tr("Looking for photos from this camera…").to_owned(), &report),
        Stage::Learning => progress(ui, crate::i18n::tr("Learning the camera's colour…").to_owned(), &report),
        _ => {}
    }
    for place in &report.places {
        ui.label(place_line(&place.folder, &place.tally));
    }
    if report.places.len() > 1 {
        ui.label(crate::i18n::tr_format!("In all: {n} photos from {days} days", n = report.total.colour, days = report.total.colour_days));
    }
    if !running && !report.places.is_empty() && report.checks.is_empty() {
        match &report.needs {
            Some(needs) => ui.label(egui::RichText::new(crate::i18n::tr_format!("Not enough yet: {needs}", needs = needs)).color(t.caution)),
            None => ui.label(crate::i18n::tr("Enough photos to learn from.")),
        };
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!running && !report.model.is_empty(), |ui| {
            if text_button(ui, "cameraColourAddFolder", "Look in Another Folder…", false).clicked() {
                let _ = app.run("cameraColour.addFolder", json!({}));
            }
        });
        ui.add_enabled_ui(!running && report.enough, |ui| {
            if text_button(ui, "cameraColourLearn", "Learn", false).clicked() {
                let _ = app.run("cameraColour.learn", json!({}));
            }
        });
        if running && text_button(ui, "cameraColourStop", "Stop", false).clicked() {
            let _ = app.run("cameraColour.cancel", json!({}));
        }
    });
    if !report.checks.is_empty() || !report.skipped.is_empty() {
        ui.add_space(6.0);
        for check in &report.checks {
            ui.label(check_line(check));
        }
        for (group, reason) in &report.skipped {
            ui.label(dim(&crate::i18n::tr_format!("{group}: not checked ({reason})", group = group, reason = reason)));
        }
    }
    if report.stage == Stage::Done && !report.written.is_empty() {
        ui.add_space(4.0);
        ui.label(crate::i18n::tr("Photos from this camera now open with the learned colour."));
        if text_button(ui, "cameraColourUndo", "Undo", false).clicked() {
            let _ = app.run("cameraColour.undo", json!({}));
        }
    }
    if let Some(error) = &report.error {
        ui.label(egui::RichText::new(error.clone()).color(t.reject));
    }
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::camera_learn::{GroupCheck, Tally, Verdict};
    use lightcraft_pipeline::{ColourBasis, ColourSource};

    #[test]
    fn the_dialog_is_offered_where_colour_is_approximate() {
        let source = |basis, black_and_white| ColourSource { basis, black_and_white };
        assert!(offered(&source(ColourBasis::Neutral, false)));
        assert!(offered(&source(ColourBasis::Spectral, true)));
        assert!(!offered(&source(ColourBasis::Spectral, false)));
        assert!(!offered(&source(ColourBasis::CameraJpeg, false)));
        assert!(!offered(&source(ColourBasis::FileMatrices, false)));
    }

    #[test]
    fn places_and_results_read_as_lines() {
        let tally = Tally { photos: 50, colour: 47, colour_days: 3 };
        assert_eq!(place_line("", &tally), "In your library: 47 photos from 3 days, and 3 black-and-white ones it can't learn from");
        assert_eq!(place_line("/photos/2019", &Tally { photos: 4, colour: 4, colour_days: 2 }), "In /photos/2019: 4 photos from 2 days");
        let verdict = Verdict { photos: 15, before: 6.7, after: 2.6, median_before: 7.8, median_after: 2.7, closer: 15, worst: -1.2, better: true };
        let check = GroupCheck {
            group: "DMC-GF1".into(),
            train_photos: 32,
            train_days: 8,
            test_days: 2,
            colour: None,
            black_and_white: Some(verdict),
            decision: Decision::Fallback,
        };
        assert_eq!(
            check_line(&check),
            "DMC-GF1: used for photos that can't take their colour from their camera JPEG (closer on 15 of 15 test photos)"
        );
    }
}
