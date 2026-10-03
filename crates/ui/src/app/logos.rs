//! The agent tools' own marks.
//!
//! An agent is shown with the mark of the tool it runs in — Claude for
//! Claude Code, OpenAI for Codex, and so on — so a list of sessions reads
//! at a glance by tool. A tool without a mark here keeps the identity
//! monogram every other row uses.
//!
//! The marks are the vendors' trademarks, shown only to say which tool a
//! session is. They come from the svgl.app library of official logos
//! (`claude-ai-icon`, `openai`/`openai_dark`, `gemini`, `copilot`/
//! `copilot_dark`, `cursor_light`/`cursor_dark`, `windsurf-light`/
//! `windsurf-dark`, `vscode`, `opencode`/`opencode-dark`; OpenCode's
//! background square removed), rasterised once to 96-pixel transparent
//! PNGs with resvg and embedded, the way the cube mark is: the build has no
//! SVG renderer, and one small decode per mark is all they cost. Marks
//! drawn in one colour come in a light and a dark variant.
use iced::widget::image::Handle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Logo {
    Claude,
    OpenAi,
    Gemini,
    Copilot,
    Cursor,
    Windsurf,
    VsCode,
    OpenCode,
}

impl Logo {
    const ALL: [Logo; 8] = [
        Logo::Claude,
        Logo::OpenAi,
        Logo::Gemini,
        Logo::Copilot,
        Logo::Cursor,
        Logo::Windsurf,
        Logo::VsCode,
        Logo::OpenCode,
    ];

    /// The mark of a runtime by its registry name (`claude-code`, `codex`,
    /// …), falling back to its vendor for a runtime this list does not name.
    pub fn for_runtime(runtime: &str) -> Option<Logo> {
        match runtime {
            "claude-code" | "claude-desktop" | "claude-browser" => Some(Logo::Claude),
            "codex" | "codex-desktop" | "chatgpt" | "chatgpt-browser" => Some(Logo::OpenAi),
            "gemini-cli" => Some(Logo::Gemini),
            "copilot" => Some(Logo::Copilot),
            "cursor" => Some(Logo::Cursor),
            "windsurf" => Some(Logo::Windsurf),
            "vscode" => Some(Logo::VsCode),
            "opencode" => Some(Logo::OpenCode),
            _ => agentdocker_core::runtime::spec(runtime)
                .and_then(|spec| Logo::for_vendor(spec.vendor)),
        }
    }

    /// The mark of a model provider as usage reports name it.
    pub fn for_vendor(vendor: &str) -> Option<Logo> {
        match vendor.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Logo::Claude),
            "openai" => Some(Logo::OpenAi),
            "google" | "gemini" => Some(Logo::Gemini),
            "github" => Some(Logo::Copilot),
            "cursor" => Some(Logo::Cursor),
            "codeium" | "windsurf" => Some(Logo::Windsurf),
            "opencode" => Some(Logo::OpenCode),
            _ => None,
        }
    }

    /// The mark of a model by its name: `claude-…`, `gpt-…`, `o3`, `gemini-…`.
    pub fn for_model(model: &str) -> Option<Logo> {
        let model = model.to_ascii_lowercase();
        let model = model.rsplit('/').next().unwrap_or(&model);
        if model.starts_with("claude") {
            Some(Logo::Claude)
        } else if model.starts_with("gpt")
            || model.starts_with("codex")
            || model.starts_with("chatgpt")
            || (model.starts_with('o') && model[1..].starts_with(|ch: char| ch.is_ascii_digit()))
        {
            Some(Logo::OpenAi)
        } else if model.starts_with("gemini") {
            Some(Logo::Gemini)
        } else {
            None
        }
    }

    fn bytes(self, dark: bool) -> &'static [u8] {
        match (self, dark) {
            (Logo::Claude, _) => include_bytes!("../logos/claude.png"),
            (Logo::OpenAi, false) => include_bytes!("../logos/openai-light.png"),
            (Logo::OpenAi, true) => include_bytes!("../logos/openai-dark.png"),
            (Logo::Gemini, _) => include_bytes!("../logos/gemini.png"),
            (Logo::Copilot, false) => include_bytes!("../logos/copilot-light.png"),
            (Logo::Copilot, true) => include_bytes!("../logos/copilot-dark.png"),
            (Logo::Cursor, false) => include_bytes!("../logos/cursor-light.png"),
            (Logo::Cursor, true) => include_bytes!("../logos/cursor-dark.png"),
            (Logo::Windsurf, false) => include_bytes!("../logos/windsurf-light.png"),
            (Logo::Windsurf, true) => include_bytes!("../logos/windsurf-dark.png"),
            (Logo::VsCode, _) => include_bytes!("../logos/vscode.png"),
            (Logo::OpenCode, false) => include_bytes!("../logos/opencode-light.png"),
            (Logo::OpenCode, true) => include_bytes!("../logos/opencode-dark.png"),
        }
    }

    /// The mark for one appearance, decoded once per mark and appearance.
    pub fn handle(self, dark: bool) -> Handle {
        static HANDLES: std::sync::OnceLock<Vec<Handle>> = std::sync::OnceLock::new();
        let handles = HANDLES.get_or_init(|| {
            Logo::ALL
                .iter()
                .flat_map(|logo| [false, true].map(|dark| decode(logo.bytes(dark))))
                .collect()
        });
        let index = Logo::ALL
            .iter()
            .position(|logo| *logo == self)
            .expect("listed");
        handles[index * 2 + usize::from(dark)].clone()
    }
}

/// An embedded RGBA PNG as an image handle.
fn decode(bytes: &[u8]) -> Handle {
    let (width, height, rgba) = pixels(bytes);
    Handle::from_rgba(width, height, rgba)
}

fn pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("embedded logo");
    let mut rgba = vec![0; reader.output_buffer_size().expect("logo buffer")];
    let info = reader.next_frame(&mut rgba).expect("embedded PNG");
    rgba.truncate(info.buffer_size());
    (info.width, info.height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_mark_is_a_square_rgba_image() {
        for logo in Logo::ALL {
            for dark in [false, true] {
                let (width, height, rgba) = pixels(logo.bytes(dark));
                assert_eq!((width, height), (96, 96), "{logo:?} dark={dark}");
                assert_eq!(rgba.len(), 96 * 96 * 4, "{logo:?} is RGBA, dark={dark}");
                assert!(
                    rgba.chunks(4).any(|px| px[3] > 0),
                    "{logo:?} draws something, dark={dark}"
                );
                // Transparent corners: the mark sits on whatever tile holds it.
                assert_eq!(rgba[3], 0, "{logo:?} corner is transparent, dark={dark}");
            }
        }
    }

    #[test]
    fn runtimes_models_and_vendors_find_their_marks() {
        assert_eq!(Logo::for_runtime("claude-code"), Some(Logo::Claude));
        assert_eq!(Logo::for_runtime("codex"), Some(Logo::OpenAi));
        assert_eq!(Logo::for_runtime("chatgpt-browser"), Some(Logo::OpenAi));
        assert_eq!(Logo::for_runtime("gemini-cli"), Some(Logo::Gemini));
        assert_eq!(Logo::for_runtime("copilot"), Some(Logo::Copilot));
        assert_eq!(Logo::for_runtime("opencode"), Some(Logo::OpenCode));
        // No mark for these: the monogram stays.
        assert_eq!(Logo::for_runtime("aider"), None);
        assert_eq!(Logo::for_runtime("goose"), None);
        assert_eq!(Logo::for_runtime("human"), None);
        assert_eq!(Logo::for_runtime("no-such-tool"), None);
        assert_eq!(Logo::for_model("claude-opus-4-5"), Some(Logo::Claude));
        assert_eq!(
            Logo::for_model("anthropic/claude-sonnet"),
            Some(Logo::Claude)
        );
        assert_eq!(Logo::for_model("gpt-5.1-codex"), Some(Logo::OpenAi));
        assert_eq!(Logo::for_model("o3"), Some(Logo::OpenAi));
        assert_eq!(
            Logo::for_model("opus"),
            None,
            "an o not followed by a digit"
        );
        assert_eq!(Logo::for_model("gemini-2.5-pro"), Some(Logo::Gemini));
        assert_eq!(Logo::for_model("llama-3"), None);
        assert_eq!(Logo::for_vendor("Anthropic"), Some(Logo::Claude));
        assert_eq!(Logo::for_vendor("openai"), Some(Logo::OpenAi));
        assert_eq!(Logo::for_vendor("Aider"), None);
    }
}
