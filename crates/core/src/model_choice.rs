//! The model and effort a person picks when launching a provider's CLI,
//! and the arguments that carry them. What a machine offers is read by the
//! host crate; this is the pure part.

use serde::{Deserialize, Serialize};

/// The label that keeps the effort a session was launched with; its model
/// is `AgentSpec::model`.
pub const EFFORT_LABEL: &str = "effort";

/// Claude Code's `--effort` levels.
pub const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Claude Code's `--model` aliases, each the latest of its kind.
pub const CLAUDE_MODELS: &[(&str, &str)] = &[
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
    ("fable", "Fable"),
];

/// One model a provider offers, with the efforts it takes (none listed:
/// the provider's own).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub efforts: Vec<String>,
}

/// What a provider offers on this machine, and what it uses unless told
/// otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoices {
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
    pub models: Vec<ModelOption>,
    /// The efforts of a model that lists none of its own.
    pub efforts: Vec<String>,
}

impl ModelChoices {
    /// The efforts `model` takes: its own when the provider lists them,
    /// else the provider's.
    pub fn efforts_for(&self, model: Option<&str>) -> &[String] {
        model
            .and_then(|model| self.models.iter().find(|m| m.id == model))
            .map(|m| m.efforts.as_slice())
            .filter(|efforts| !efforts.is_empty())
            .unwrap_or(&self.efforts)
    }
}

/// A model or effort as the CLI takes it: one word of letters, digits and
/// `. _ - : / [ ]`, so it can never be read as another option.
pub fn plain_choice(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '[' | ']')
        })
}

/// The arguments that make `runtime`'s CLI use `model` and `effort`, to go
/// before the person's own `given` ones. A choice the person's arguments
/// already make is left to them, so nothing is said twice. A runtime with
/// no such options takes none.
pub fn choice_args(
    runtime: &str,
    model: Option<&str>,
    effort: Option<&str>,
    given: &[String],
) -> Result<Vec<String>, &'static str> {
    for value in [model, effort].into_iter().flatten() {
        if !plain_choice(value) {
            return Err("A model or effort is one word");
        }
    }
    let has = |flags: &[&str]| {
        given.iter().any(|arg| {
            flags
                .iter()
                .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
        })
    };
    // `-c key=value` and `--config key=value` set Codex configuration.
    let configures = |key: &str| {
        given.windows(2).any(|pair| {
            matches!(pair[0].as_str(), "-c" | "--config")
                && pair[1].trim_start().starts_with(&format!("{key}="))
        }) || given
            .iter()
            .any(|arg| arg.starts_with(&format!("--config={key}=")))
    };
    let mut args = Vec::new();
    match runtime {
        "codex" => {
            if let Some(model) = model
                && !has(&["-m", "--model"])
                && !configures("model")
            {
                args.extend(["--model".to_owned(), model.to_owned()]);
            }
            if let Some(effort) = effort
                && !configures("model_reasoning_effort")
            {
                args.extend([
                    "-c".to_owned(),
                    format!("model_reasoning_effort=\"{effort}\""),
                ]);
            }
        }
        "claude-code" => {
            if let Some(model) = model
                && !has(&["--model"])
            {
                args.extend(["--model".to_owned(), model.to_owned()]);
            }
            if let Some(effort) = effort
                && !has(&["--effort"])
            {
                args.extend(["--effort".to_owned(), effort.to_owned()]);
            }
        }
        _ => {}
    }
    Ok(args)
}

/// The model and effort the person's own arguments `given` set for
/// `runtime`, where they set one.
pub fn given_choices(runtime: &str, given: &[String]) -> (Option<String>, Option<String>) {
    let value_of = |flags: &[&str]| {
        given.iter().enumerate().find_map(|(at, arg)| {
            flags.iter().find_map(|flag| {
                if arg == flag {
                    given.get(at + 1).cloned()
                } else {
                    arg.strip_prefix(&format!("{flag}=")).map(str::to_owned)
                }
            })
        })
    };
    let configured = |key: &str| {
        let prefix = format!("{key}=");
        given
            .windows(2)
            .filter(|pair| matches!(pair[0].as_str(), "-c" | "--config"))
            .map(|pair| pair[1].as_str())
            .chain(given.iter().filter_map(|arg| arg.strip_prefix("--config=")))
            .find_map(|setting| setting.trim().strip_prefix(&prefix))
            .map(|value| value.trim().trim_matches('"').to_owned())
    };
    let plain = |value: Option<String>| value.filter(|v| plain_choice(v));
    match runtime {
        "codex" => (
            plain(value_of(&["-m", "--model"]).or_else(|| configured("model"))),
            plain(configured("model_reasoning_effort")),
        ),
        "claude-code" => (
            plain(value_of(&["--model"])),
            plain(value_of(&["--effort"])),
        ),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn each_cli_is_told_its_model_and_effort_its_own_way() {
        assert_eq!(
            choice_args("codex", Some("gpt-6-astra"), Some("high"), &[]).unwrap(),
            words(&[
                "--model",
                "gpt-6-astra",
                "-c",
                "model_reasoning_effort=\"high\""
            ])
        );
        assert_eq!(
            choice_args("claude-code", Some("opus"), Some("max"), &[]).unwrap(),
            words(&["--model", "opus", "--effort", "max"])
        );
        assert_eq!(
            choice_args("claude-code", None, None, &[]).unwrap(),
            Vec::<String>::new(),
            "no choice, the provider's own"
        );
        assert_eq!(
            choice_args("aider", Some("x"), Some("high"), &[]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn what_the_persons_own_arguments_say_is_left_to_them() {
        let given = words(&["-m", "o9", "--config", "model_reasoning_effort=\"low\""]);
        assert!(
            choice_args("codex", Some("gpt-6-astra"), Some("high"), &given)
                .unwrap()
                .is_empty()
        );
        let given = words(&["--model=sonnet"]);
        assert_eq!(
            choice_args("claude-code", Some("opus"), Some("high"), &given).unwrap(),
            words(&["--effort", "high"])
        );
    }

    #[test]
    fn the_persons_own_arguments_say_which_model_and_effort() {
        assert_eq!(
            given_choices(
                "codex",
                &words(&["-m", "o9", "-c", "model_reasoning_effort=\"low\""])
            ),
            (Some("o9".into()), Some("low".into()))
        );
        assert_eq!(
            given_choices("codex", &words(&["--config=model=\"o8\""])),
            (Some("o8".into()), None)
        );
        assert_eq!(
            given_choices("claude-code", &words(&["--model=opus", "--effort", "max"])),
            (Some("opus".into()), Some("max".into()))
        );
        assert_eq!(given_choices("claude-code", &[]), (None, None));
    }

    #[test]
    fn a_choice_is_one_word_never_another_option() {
        for bad in ["", "--dangerously-bypass", "a b", "x;rm", &"m".repeat(65)] {
            assert!(
                choice_args("codex", Some(bad), None, &[]).is_err(),
                "{bad:?}"
            );
        }
        assert!(plain_choice("claude-opus-5-5[1m]"));
        assert!(plain_choice("org/model:tag"));
    }

    #[test]
    fn a_model_takes_its_own_efforts_or_the_providers() {
        let choices = ModelChoices {
            models: vec![
                ModelOption {
                    id: "a".into(),
                    name: "A".into(),
                    efforts: vec!["low".into(), "ultra".into()],
                },
                ModelOption {
                    id: "b".into(),
                    name: "B".into(),
                    efforts: vec![],
                },
            ],
            efforts: vec!["medium".into()],
            ..ModelChoices::default()
        };
        assert_eq!(choices.efforts_for(Some("a")), ["low", "ultra"]);
        assert_eq!(choices.efforts_for(Some("b")), ["medium"]);
        assert_eq!(choices.efforts_for(None), ["medium"]);
    }
}
