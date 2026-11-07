//! The picture style read from real raws in `corpus/raw` (git-ignored, CC0; fetched with `cargo xtask corpus
//! --download`). Skips when the corpus is absent. `LIGHTCRAFT_CORPUS` overrides the corpus root.

use lightcraft_meta::picture_style::read;
use std::path::{Path, PathBuf};

fn corpus_raw() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")).join("raw")
}

#[test]
fn corpus_raws_report_the_picture_style_they_were_shot_with() {
    let dir = corpus_raw();
    if !dir.is_dir() {
        lightcraft_testkit::skip!(Corpus, "{} absent", dir.display());
        return;
    }
    // (file, style name, base, black and white); exiv2 shows the same names (from Picture Control version 3 with
    // the version in front of them)
    let expected: &[(&str, Option<&str>, Option<&str>, Option<bool>)] = &[
        ("raf-fujifilm-x-a3.raf", Some("Classic Chrome"), None, Some(false)),
        ("raf-fujifilm-gfx100s-ii.raf", Some("Eterna/Cinema"), None, Some(false)),
        ("raf-fujifilm-x100v.raf", Some("Velvia/Vivid"), None, Some(false)),
        ("nef-nikon-d5300.nef", Some("STANDARD-02"), Some("STANDARD"), None),
        ("nef-nikon-1-v3.nef", Some("VIVID"), Some("VIVID"), None),
        ("coverage-3581.nef", Some("AUTO"), Some("AUTO"), None),
        // before Picture Controls: nothing to read
        ("nef-nikon-d200.nef", None, None, None),
    ];
    let mut checked = 0;
    for &(file, name, base, monochrome) in expected {
        let Ok(bytes) = std::fs::read(dir.join(file)) else {
            lightcraft_testkit::skip!(Corpus, "{file} absent");
            continue;
        };
        let style = read(&bytes);
        assert_eq!(style.as_ref().map(|s| s.name.as_str()), name, "{file}");
        assert_eq!(style.as_ref().and_then(|s| s.base.as_deref()), base, "{file}");
        assert_eq!(style.as_ref().and_then(|s| s.monochrome), monochrome, "{file}");
        // a truncated copy reads or refuses, never panics
        for n in [0, 16, 1024, bytes.len() / 2] {
            let _ = read(bytes.get(..n).unwrap_or(&bytes));
        }
        checked += 1;
    }
    eprintln!("picture styles checked on {checked} corpus raws");
}
