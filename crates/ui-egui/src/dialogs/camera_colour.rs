//! Improve Camera Colour (Profile panel ▸ "Improve colour for this camera…"): what the scan of the camera's photos
//! found, a folder to look in too, Learn, and what learning did. The work is the engine's `cameraColour.*` job; this
//! only shows its report and starts its steps.

use lightcraft_engine::camera_learn::{Decision, Report, Stage};
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;

/// Whether a photo with this colour source is offered the dialog: its colour is approximate (no colour data for its
/// camera), or a black-and-white camera JPEG left it on the spectral matrices, which a learned profile beat on most
/// cameras checked.
pub fn offered(source: &lightcraft_pipeline::ColourSource) -> bool {
    use lightcraft_pipeline::ColourBasis;
    matches!(source.basis, ColourBasis::Neutral) || (source.black_and_white && matches!(source.basis, ColourBasis::Spectral))
}

/// A place as the dialog names it: the library, or a folder shortened to its last two levels (the full path is its
/// hover text).
fn place_name(folder: &str) -> String {
    if folder.is_empty() {
        return crate::i18n::tr("Your library").to_owned();
    }
    let parts: Vec<&str> = folder.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).take(3).collect();
    match parts.as_slice() {
        [last, parent, _] => format!("…/{parent}/{last}"),
        _ => folder.to_owned(),
    }
}

/// What a place holds, for the right of its row: photos that can teach colour and their days, then the
/// black-and-white ones the file says are (`colour` counts every other photo: some cameras don't record their style).
fn tally_text(tally: &lightcraft_engine::camera_learn::Tally) -> (String, Option<String>) {
    let photos = match tally.colour {
        1 => crate::i18n::tr("1 photo").to_owned(),
        n => crate::i18n::tr_format!("{n} photos", n = n),
    };
    let days = match tally.colour_days {
        1 => crate::i18n::tr("1 day").to_owned(),
        n => crate::i18n::tr_format!("{n} days", n = n),
    };
    let found = format!("{photos} · {days}");
    let bw = tally.photos.saturating_sub(tally.colour);
    (found, (bw > 0).then(|| crate::i18n::tr_format!("{bw} black and white, not used", bw = bw)))
}

/// What learning decided for one group: its outcome, and the evidence for it.
fn check_lines(check: &lightcraft_engine::camera_learn::GroupCheck) -> (String, Option<String>) {
    let closer = |v: &Option<lightcraft_engine::camera_learn::Verdict>| {
        v.as_ref().map(|v| crate::i18n::tr_format!("closer to the camera on {closer} of {n} test photos", closer = v.closer, n = v.photos))
    };
    match check.decision {
        Decision::Everywhere => (crate::i18n::tr("Used for all its photos").to_owned(), closer(&check.colour)),
        Decision::Fallback => (crate::i18n::tr("Used where colour was approximate").to_owned(), closer(&check.black_and_white)),
        Decision::NotUsed => (crate::i18n::tr("Not used: no closer than now").to_owned(), None),
    }
}

/// A box of rows on the dialog's inset colour.
fn panel<R>(ui: &mut egui::Ui, t: &Tokens, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(t.inset)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(12.5)).color(t.text_label));
            add(ui)
        })
        .inner
}

/// A row: `left`, and `right` against the right edge.
fn row(ui: &mut egui::Ui, left: egui::RichText, right: egui::RichText) -> egui::Response {
    ui.horizontal(|ui| {
        let r = ui.label(left);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(right));
        r
    })
    .inner
}

fn progress(ui: &mut egui::Ui, t: &Tokens, text: &str, report: &Report) {
    ui.label(egui::RichText::new(crate::i18n::tr(text)).color(t.text_label));
    let fraction = if report.of > 0 { report.done as f32 / report.of as f32 } else { 0.0 };
    ui.add(egui::ProgressBar::new(fraction).desired_height(6.0).text(egui::RichText::new(format!("{} / {}", report.done, report.of)).size(10.0)));
}

pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::button::Button;
    let report = app.session.camera_learn.report();
    let running = app.session.camera_learn.running();
    let dim = |s: &str| egui::RichText::new(s.to_owned()).font(t.font(12.0)).color(t.text_dim);
    ui.spacing_mut().item_spacing.y = 4.0;
    if !report.model.is_empty() {
        ui.label(egui::RichText::new(&report.model).font(t.semibold(15.0)).color(t.text));
    }
    ui.label(dim(crate::i18n::tr(
        "LightCraft learns the camera's colour from your photos taken with it, comparing each raw with the JPEG the camera stored inside it.",
    )));
    ui.label(dim(crate::i18n::tr("Files are only read: nothing is imported, changed or uploaded.")));
    ui.add_space(8.0);

    panel(ui, t, "Photos found", |ui| {
        if report.places.is_empty() {
            ui.label(dim(crate::i18n::tr("Looking in your library…")));
        }
        for place in &report.places {
            let (found, bw) = tally_text(&place.tally);
            let r = row(ui, egui::RichText::new(place_name(&place.folder)).color(t.text), egui::RichText::new(found).color(t.text));
            if !place.folder.is_empty() {
                r.on_hover_text(&place.folder);
            }
            if let Some(bw) = bw {
                ui.label(dim(&bw));
            }
        }
        if report.places.len() > 1 {
            ui.separator();
            let (found, _) = tally_text(&report.total);
            row(ui, egui::RichText::new(crate::i18n::tr("In all")).font(t.semibold(12.5)), egui::RichText::new(found).font(t.semibold(12.5)));
        }
        if report.stage == Stage::Scanning {
            progress(ui, t, "Looking for photos from this camera…", &report);
        } else if !report.places.is_empty() && report.stage != Stage::Learning {
            match &report.needs {
                Some(needs) => ui.label(egui::RichText::new(crate::i18n::tr_format!("Not enough yet: {needs}.", needs = needs)).color(t.caution)),
                None => ui.label(egui::RichText::new(crate::i18n::tr("Enough to learn from.")).color(t.accent)),
            };
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if Button::new("Add a Folder…")
                .secondary()
                .small()
                .enabled(!running && !report.model.is_empty())
                .widget("button:cameraColourAddFolder")
                .show(ui)
                .clicked()
            {
                let _ = app.run("cameraColour.addFolder", json!({}));
            }
            ui.label(dim(crate::i18n::tr("Photos from more days help the most.")));
        });
    });

    if report.stage == Stage::Learning {
        ui.add_space(8.0);
        progress(ui, t, "Learning the camera's colour…", &report);
    }
    if running && Button::new("Stop").secondary().small().widget("button:cameraColourStop").show(ui).clicked() {
        let _ = app.run("cameraColour.cancel", json!({}));
    }

    if !report.checks.is_empty() || !report.skipped.is_empty() {
        ui.add_space(8.0);
        panel(ui, t, "Result", |ui| {
            for check in &report.checks {
                let (outcome, evidence) = check_lines(check);
                let colour = if check.decision == Decision::NotUsed { t.text_dim } else { t.accent };
                row(ui, egui::RichText::new(&check.group).color(t.text), egui::RichText::new(outcome).color(colour));
                if let Some(evidence) = evidence {
                    ui.label(dim(&evidence));
                }
            }
            for (group, reason) in &report.skipped {
                row(ui, egui::RichText::new(group).color(t.text), dim(crate::i18n::tr("Not checked")));
                ui.label(dim(reason));
            }
            if report.stage == Stage::Done && !report.written.is_empty() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(crate::i18n::tr("Photos now open with the learned colour.")).color(t.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if Button::new("Undo").secondary().small().widget("button:cameraColourUndo").show(ui).clicked() {
                            let _ = app.run("cameraColour.undo", json!({}));
                        }
                    });
                });
            }
        });
    }
    if let Some(error) = &report.error {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(error.clone()).color(t.reject));
    }
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
}

/// Whether Learn can be pressed (the dialog's confirm button).
pub fn can_learn(app: &LightcraftApp) -> bool {
    app.session.camera_learn.report().enough && !app.session.camera_learn.running()
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
    fn places_and_results_read_as_short_lines() {
        assert_eq!(place_name(""), "Your library");
        assert_eq!(place_name("/home/me/Pictures/2019/summer"), "…/2019/summer");
        assert_eq!(place_name("/photos"), "/photos");
        let tally = Tally { photos: 50, colour: 47, colour_days: 3 };
        assert_eq!(tally_text(&tally), ("47 photos · 3 days".to_string(), Some("3 black and white, not used".to_string())));
        assert_eq!(tally_text(&Tally { photos: 4, colour: 4, colour_days: 2 }).1, None);
        assert_eq!(tally_text(&Tally { photos: 1, colour: 1, colour_days: 1 }).0, "1 photo · 1 day");
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
            check_lines(&check),
            ("Used where colour was approximate".to_string(), Some("closer to the camera on 15 of 15 test photos".to_string()))
        );
    }
}
