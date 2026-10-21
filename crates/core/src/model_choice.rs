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

/// A model or effort the person's own arguments set, in the order given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    Model,
    Effort,
}

/// Every model and effort `given` sets for `runtime`, in argument order, in
/// each spelling the CLI takes: `-m X`, `-mX`, `-m=X`, `--model X`,
/// `--model=X`; for Codex also `-c`/`--config` with `model=` or
/// `model_reasoning_effort=` (spaces around `=` allowed, quotes taken off);
/// for Claude `--effort X` and `--effort=X`. The last of each is the one in
/// force.
fn settings(runtime: &str, given: &[String]) -> Vec<(Setting, String)> {
    let mut found = Vec::new();
    let mut args = given.iter();
    // The value an option takes: after `=`, attached, or the next argument.
    let value =
        |arg: &str, long: &str, short: Option<&str>, args: &mut std::slice::Iter<'_, String>| {
            if arg == long || short == Some(arg) {
                return args.next().cloned();
            }
            if let Some(rest) = arg.strip_prefix(long).and_then(|r| r.strip_prefix('=')) {
                return Some(rest.to_owned());
            }
            let short = short?;
            arg.strip_prefix(short)
                .filter(|rest| !rest.is_empty())
                .map(|rest| rest.strip_prefix('=').unwrap_or(rest).to_owned())
        };
    while let Some(arg) = args.next() {
        // Past `--` everything is the program's own text, never an option.
        if arg == "--" {
            break;
        }
        match runtime {
            "codex" => {
                if let Some(model) = value(arg, "--model", Some("-m"), &mut args) {
                    found.push((Setting::Model, model));
                } else if let Some(setting) = value(arg, "--config", Some("-c"), &mut args)
                    && let Some((key, raw)) = setting.split_once('=')
                {
                    let raw = raw.trim().trim_matches('"').trim_matches('\'').to_owned();
                    match key.trim() {
                        "model" => found.push((Setting::Model, raw)),
                        "model_reasoning_effort" => found.push((Setting::Effort, raw)),
                        _ => {}
                    }
                }
            }
            "claude-code" => {
                if let Some(model) = value(arg, "--model", None, &mut args) {
                    found.push((Setting::Model, model));
                } else if let Some(effort) = value(arg, "--effort", None, &mut args) {
                    found.push((Setting::Effort, effort));
                }
            }
            _ => {}
        }
    }
    found
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
    let set = settings(runtime, given);
    let says = |which: Setting| set.iter().any(|(setting, _)| *setting == which);
    let model = model.filter(|_| !says(Setting::Model));
    let effort = effort.filter(|_| !says(Setting::Effort));
    let mut args = Vec::new();
    match runtime {
        "codex" => {
            if let Some(model) = model {
                args.extend(["--model".to_owned(), model.to_owned()]);
            }
            if let Some(effort) = effort {
                args.extend([
                    "-c".to_owned(),
                    format!("model_reasoning_effort=\"{effort}\""),
                ]);
            }
        }
        "claude-code" => {
            if let Some(model) = model {
                args.extend(["--model".to_owned(), model.to_owned()]);
            }
            if let Some(effort) = effort {
                args.extend(["--effort".to_owned(), effort.to_owned()]);
            }
        }
        _ => {}
    }
    Ok(args)
}

/// The model and effort the person's own arguments `given` put in force
/// for `runtime`: the last of each, where they set one.
pub fn given_choices(runtime: &str, given: &[String]) -> (Option<String>, Option<String>) {
    let set = settings(runtime, given);
    let last = |which: Setting| {
        set.iter()
            .rev()
            .find(|(setting, _)| *setting == which)
            .map(|(_, value)| value.clone())
            .filter(|value| plain_choice(value))
    };
    (last(Setting::Model), last(Setting::Effort))
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
    fn every_spelling_of_a_setting_counts_and_the_last_is_in_force() {
        let codex = |args: &[&str]| given_choices("codex", &words(args));
        assert_eq!(
            codex(&["-c=model_reasoning_effort=low"]),
            (None, Some("low".into()))
        );
        assert_eq!(
            codex(&["-c", "model_reasoning_effort = \"high\""]),
            (None, Some("high".into()))
        );
        assert_eq!(codex(&["-mo9"]), (Some("o9".into()), None));
        assert_eq!(codex(&["-m=o9"]), (Some("o9".into()), None));
        assert_eq!(
            codex(&[
                "--config=model_reasoning_effort=low",
                "-c",
                "model_reasoning_effort=max",
                "-m",
                "a",
                "--model=b"
            ]),
            (Some("b".into()), Some("max".into())),
            "argument order, the last in force"
        );
        assert!(
            choice_args(
                "codex",
                None,
                Some("high"),
                &words(&["-c=model_reasoning_effort=low"])
            )
            .unwrap()
            .is_empty(),
            "the inline short form already says it"
        );
        assert_eq!(
            given_choices("claude-code", &words(&["--effort=low", "--effort", "max"])),
            (None, Some("max".into()))
        );
    }

    #[test]
    fn text_after_the_option_terminator_is_never_a_setting() {
        let given = words(&["--", "--model=sonnet", "--effort", "max"]);
        assert_eq!(given_choices("claude-code", &given), (None, None));
        assert_eq!(
            choice_args("claude-code", Some("opus"), Some("high"), &given).unwrap(),
            words(&["--model", "opus", "--effort", "high"]),
            "the chips still say it"
        );
        assert_eq!(
            given_choices("codex", &words(&["--", "-m", "o9"])),
            (None, None)
        );
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
