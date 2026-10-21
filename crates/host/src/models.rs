//! What a provider's CLI offers on this machine for its model and effort,
//! and what it uses unless told otherwise, read from its own files: Codex's
//! `config.toml` and `models_cache.json`, Claude Code's `settings.json`.
//! Nothing is fetched; a file that is missing or unreadable offers less.

use std::path::{Path, PathBuf};

use agentdocker_core::model_choice::{CLAUDE_EFFORTS, CLAUDE_MODELS, ModelChoices, ModelOption};

/// A provider file is small; one larger than this is not read.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// What `runtime` offers here; nothing for a runtime with no such choices.
pub fn choices(runtime: &str) -> ModelChoices {
    match runtime {
        "codex" => {
            let home = codex_home();
            codex(
                home.as_deref()
                    .and_then(|h| read(&h.join("config.toml")))
                    .as_deref(),
                home.as_deref()
                    .and_then(|h| read(&h.join("models_cache.json")))
                    .as_deref(),
            )
        }
        "claude-code" => claude(claude_settings().and_then(|p| read(&p)).as_deref()),
        _ => ModelChoices::default(),
    }
}

fn codex_home() -> Option<PathBuf> {
    crate::codex_trust::config_path().and_then(|config| config.parent().map(Path::to_path_buf))
}

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
fn claude_settings() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|p| !p.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir).join("settings.json")),
        None => std::env::home_dir().map(|home| home.join(".claude").join("settings.json")),
    }
}

fn read(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    (meta.is_file() && meta.len() <= MAX_FILE)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

/// Codex: the defaults its configuration names, and the models its cache
/// lists to be shown, each with the efforts it takes.
pub fn codex(config: Option<&str>, cache: Option<&str>) -> ModelChoices {
    let config = config.and_then(|text| text.parse::<toml::Table>().ok());
    let setting = |key: &str| {
        config
            .as_ref()
            .and_then(|table| table.get(key))
            .and_then(toml::Value::as_str)
            .filter(|value| agentdocker_core::model_choice::plain_choice(value))
            .map(str::to_owned)
    };
    let models = cache
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|cache| cache["models"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|model| model["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|model| {
            let id = model["slug"]
                .as_str()
                .filter(|id| agentdocker_core::model_choice::plain_choice(id))?;
            Some(ModelOption {
                id: id.to_owned(),
                name: model["display_name"].as_str().unwrap_or(id).to_owned(),
                efforts: model["supported_reasoning_levels"]
                    .as_array()
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(|level| level["effort"].as_str().or(level.as_str()))
                            .filter(|effort| agentdocker_core::model_choice::plain_choice(effort))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    ModelChoices {
        default_model: setting("model"),
        default_effort: setting("model_reasoning_effort"),
        models,
        efforts: Vec::new(),
    }
}

/// Claude Code: its aliases and efforts, and the defaults its settings name.
pub fn claude(settings: Option<&str>) -> ModelChoices {
    let settings = settings.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
    let setting = |key: &str| {
        settings
            .as_ref()
            .and_then(|s| s[key].as_str())
            .filter(|value| agentdocker_core::model_choice::plain_choice(value))
            .map(str::to_owned)
    };
    ModelChoices {
        default_model: setting("model"),
        default_effort: setting("effortLevel"),
        models: CLAUDE_MODELS
            .iter()
            .map(|(id, name)| ModelOption {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
                efforts: Vec::new(),
            })
            .collect(),
        efforts: CLAUDE_EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_offers_what_its_cache_shows_with_each_models_efforts() {
        let cache = r#"{"models":[
            {"slug":"gpt-6-astra","display_name":"GPT-6-Astra","visibility":"list",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"xhigh"}]},
            {"slug":"hidden-one","visibility":"hide","supported_reasoning_levels":[]},
            {"slug":"--not a model","visibility":"list"}
        ]}"#;
        let config = "model = \"gpt-6-astra\"\nmodel_reasoning_effort = \"xhigh\"\n";
        let choices = codex(Some(config), Some(cache));
        assert_eq!(choices.default_model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(choices.default_effort.as_deref(), Some("xhigh"));
        assert_eq!(
            choices.models.len(),
            1,
            "hidden and malformed models are left out"
        );
        assert_eq!(choices.models[0].name, "GPT-6-Astra");
        assert_eq!(choices.efforts_for(Some("gpt-6-astra")), ["low", "xhigh"]);
        assert_eq!(
            codex(None, None),
            ModelChoices::default(),
            "nothing to read"
        );
        assert_eq!(codex(Some("not = [toml"), Some("{")).models, vec![]);
    }

    #[test]
    fn claude_offers_its_aliases_and_efforts_with_its_settings_defaults() {
        let choices = claude(Some(r#"{"model":"opus","effortLevel":"xhigh"}"#));
        assert_eq!(choices.default_model.as_deref(), Some("opus"));
        assert_eq!(choices.default_effort.as_deref(), Some("xhigh"));
        assert!(choices.models.iter().any(|m| m.id == "sonnet"));
        assert_eq!(choices.efforts_for(Some("opus")), CLAUDE_EFFORTS);
        assert_eq!(claude(None).default_model, None);
    }
}
