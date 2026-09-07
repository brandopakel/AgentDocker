//! Telling projects apart at a glance.
//!
//! A window showing one project needs none of this. A window showing
//! four — which is the case this app exists for — needs the answer to
//! "whose is that?" to be available without reading, because the reason
//! you opened it was to scan rather than to study.
//!
//! So every project gets a colour, and the colour is the same one every
//! time: derived from the project id, which is a content fingerprint and
//! therefore stable across restarts, across machines, and across the two
//! agents that happen to be in the same repository. Nothing is stored
//! and nothing is assigned in order of appearance, so a project does not
//! change colour because a different agent started first today.

use egui::Color32;

/// A project's colour, from its id.
///
/// Hue is the only thing the id chooses. Saturation and lightness are
/// fixed at values that stay legible on both a light and a dark ground,
/// so a project is never a colour the viewer cannot read — which is what
/// happens when you hash into RGB directly.
pub fn colour(project_id: &str) -> Color32 {
    let hue = hash(project_id) % 360;
    // Deliberately not the full circle of saturation: these sit beside
    // text, and a fully saturated swatch shouts.
    from_hsl(hue as f32, 0.55, 0.55)
}

/// FNV-1a. Small, stable, and — the point here — the same everywhere,
/// which `DefaultHasher` is explicitly not: its output may change
/// between Rust releases, and a project changing colour on a toolchain
/// upgrade is exactly the surprise this is meant to avoid.
fn hash(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// HSL to RGB, for `l` and `s` in 0..=1 and `h` in degrees.
fn from_hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(byte(r), byte(g), byte(b))
}

/// The mark that carries a project's identity into a row: a filled dot
/// in the project's colour, sized to sit beside text.
pub fn dot(ui: &mut egui::Ui, project_id: &str) {
    let size = egui::vec2(10.0, 10.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .circle_filled(rect.center(), 4.0, colour(project_id));
    }
    let _ = response;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_keeps_its_colour() {
        // The same id is the same colour, every time and every run.
        assert_eq!(colour("865c840ee2ef"), colour("865c840ee2ef"));
        // And a different project is a different one.
        assert_ne!(colour("865c840ee2ef"), colour("102d56b1a4cc"));
    }

    #[test]
    fn the_hash_is_the_one_we_chose_rather_than_the_toolchains() {
        // FNV-1a's published test vectors. Pinning them is the point: a
        // project must not change colour because Rust changed its
        // default hasher.
        assert_eq!(hash(""), 0x811c_9dc5);
        assert_eq!(hash("a"), 0xe40c_292c);
        assert_eq!(hash("foobar"), 0xbf9c_f968);
    }

    #[test]
    fn every_colour_is_legible_rather_than_merely_distinct() {
        // Hashing into RGB directly gives near-black and near-white
        // swatches that vanish against one ground or the other. Fixing
        // saturation and lightness bounds how dark or bright any of them
        // can be, and this is the assertion that keeps it that way.
        for i in 0..500 {
            let c = colour(&format!("project-{i}"));
            let luma =
                0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b());
            assert!(
                (60.0..=210.0).contains(&luma),
                "project-{i} is {c:?}, luma {luma} — unreadable on one ground or the other"
            );
        }
    }

    #[test]
    fn the_hues_actually_spread() {
        // A hash that clustered would give four projects four shades of
        // the same colour, which is worse than no colour at all.
        let colours: std::collections::HashSet<_> = (0..40)
            .map(|i| {
                let c = colour(&format!("{i:016x}"));
                (c.r(), c.g(), c.b())
            })
            .collect();
        assert!(colours.len() > 35, "only {} distinct", colours.len());
    }

    #[test]
    fn hsl_corners_are_the_colours_they_should_be() {
        assert_eq!(from_hsl(0.0, 1.0, 0.5), Color32::from_rgb(255, 0, 0));
        assert_eq!(from_hsl(120.0, 1.0, 0.5), Color32::from_rgb(0, 255, 0));
        assert_eq!(from_hsl(240.0, 1.0, 0.5), Color32::from_rgb(0, 0, 255));
        // No saturation is grey, whatever the hue.
        assert_eq!(from_hsl(200.0, 0.0, 0.5), Color32::from_rgb(128, 128, 128));
    }
}
