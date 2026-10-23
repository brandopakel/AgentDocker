//! The judge: narrow, typed questions put to a TypeSafe System One model
//! (Jev) about text the daemon already holds, turned on in `[judge]` in
//! `agentd.toml` and off otherwise.
//!
//! The one judgment so far is `screens`: when an agent's screen has gone
//! quiet, its last [`SCREEN_LINES`] lines, stripped of terminal escapes,
//! are asked four yes/no questions — is an approval prompt open, is the
//! agent asking the person something, is work still running, is there a
//! typed draft nobody sent — and each answer comes back as the
//! probability of yes. [`decide`] turns the four into a promotion to
//! *waiting for you* or nothing, and [`raise`] applies a promotion: it
//! lifts working, idle or unknown to waiting, and leaves every other
//! state as it was. Nothing here can take an agent out of waiting, clear
//! a state or demote one; an answer the judge is unsure of does nothing.
//! The model's answers are advisory: AgentDocker reads no state from an
//! agent's words anywhere else.
//!
//! Everything in this module is pure: what is asked, how an answer is
//! read and what it may change. Reading screens, the key and the network
//! are the daemon's (`agentd::daemon::judge`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Activity, AgentRecord, WaitReason, wait::Judged};

/// TypeSafe's System One endpoint.
pub const DEFAULT_URL: &str = "https://api.typesafe.ai/v1/systemone";
/// A versioned model rather than an alias such as `jev-latest`, so the
/// thresholds keep the meaning they were set with when the alias moves.
/// This is the stable release `jev-latest` names.
pub const DEFAULT_MODEL: &str = "jev-1.13.0";
/// How many of a screen's last lines are judged.
pub const SCREEN_LINES: usize = 30;
/// The longest line sent, in characters; the rest of it is cut.
pub const LINE_CHARS: usize = 300;
/// How long a screen must stay unchanged before it is judged. A working
/// agent's screen changes at least every second or two — a spinner, an
/// elapsed-time counter, streamed output — so twenty seconds without a
/// change is well past any of those, and soon enough that a prompt left
/// open is noticed within half a minute.
pub const QUIET_SECS: u64 = 20;
/// Calls a minute unless the file says otherwise: a quiet screen is
/// judged once, so this binds only when several go quiet together.
pub const DEFAULT_CALLS_PER_MINUTE: u32 = 6;
pub const MAX_CALLS_PER_MINUTE: u32 = 60;
/// The least probability of yes that promotes, in percent, unless the
/// file says otherwise; it may be set from 50 to 99. Staged screens of
/// both providers' prompts put every real approval, question and draft at
/// 0.79 or more and every wrong yes at 0.56 or less; 75 sits between,
/// nearer the yes side, since a wrong promotion asks for the person.
pub const DEFAULT_THRESHOLD_PERCENT: u8 = 75;
/// Work the model reads as running at least this likely stops any
/// promotion: a busy screen is not one waiting on the person.
pub const RUNNING_VETO: f64 = 0.5;

/// What the judge may be asked to read, by the names `[judge]` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Judgment {
    /// A quiet agent's last screen lines, for whether it waits on the
    /// person.
    Screens,
}

impl Judgment {
    pub const ALL: [Self; 1] = [Self::Screens];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screens => "screens",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|j| j.as_str() == name)
    }
}

/// Text as a terminal would leave it on screen, without its escapes:
/// CSI, OSC, DCS and the other escape sequences go; a carriage return or
/// backspace moves back over what the line already holds, so a progress
/// line rewritten in place reads as it was last drawn; other control
/// characters go, and a tab is one space.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line: Vec<char> = Vec::new();
    let mut column = 0usize;
    let mut chars = text.chars().peekable();
    let flush = |line: &mut Vec<char>, out: &mut String| {
        out.extend(line.drain(..));
    };
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: up to BEL or ESC \.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' || c == '\u{9c}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Intermediates, then one final character.
                Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                    for c in chars.by_ref() {
                        if !('\u{20}'..='\u{2f}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            },
            '\u{9b}' => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            '\n' => {
                flush(&mut line, &mut out);
                out.push('\n');
                column = 0;
            }
            '\r' => column = 0,
            '\u{08}' => column = column.saturating_sub(1),
            c if c.is_control() && c != '\t' => {}
            c => {
                let c = if c == '\t' { ' ' } else { c };
                if column < line.len() {
                    line[column] = c;
                } else {
                    line.push(c);
                }
                column += 1;
            }
        }
    }
    flush(&mut line, &mut out);
    out
}

/// The lines of a screen that are judged: escapes stripped, trailing
/// space cut, each line at most [`LINE_CHARS`] characters, blank lines
/// at either end left out, and only the last [`SCREEN_LINES`].
pub fn screen_tail(text: &str) -> Vec<String> {
    let stripped = strip_ansi(text);
    let mut lines: Vec<String> = stripped
        .split('\n')
        .map(|line| line.trim_end().chars().take(LINE_CHARS).collect())
        .collect();
    while lines.last().is_some_and(|line: &String| line.is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(SCREEN_LINES);
    let mut tail: Vec<String> = lines.split_off(start);
    let blank = tail.iter().take_while(|line| line.is_empty()).count();
    tail.drain(..blank);
    tail
}

/// What the state says the screen is, so each question can name it.
const ABOUT: &str = "The last lines of a coding agent's terminal, top to bottom; the last \
                     line is the bottom of the screen, where the agent is now.";

/// The four questions, by the names their answers come back under. The
/// names are for code and are not sent to the model as meaning.
fn questions() -> Value {
    json!({
        "approval": {
            "type": "noul",
            "instructions": "Is an approval prompt open at the bottom of `screen`, asking the \
                person to allow or deny an action (running a command, editing a file, using \
                a tool) before the agent goes on?",
            "criteria": {
                "true": "A permission or approval prompt is open and waiting for a choice, \
                    for example options such as Yes, Yes and don't ask again, No, Allow or Deny.",
                "false": "No approval prompt is waiting at the bottom of the screen: there is \
                    none, or one shown earlier has already been answered."
            }
        },
        "question": {
            "type": "noul",
            "instructions": "Does the bottom of `screen` show the agent asking the person a \
                question, or offering choices, that it is waiting for them to answer?",
            "criteria": {
                "true": "The agent's latest message asks the person something or lists \
                    options for them to pick, and nothing below it answers it.",
                "false": "The agent is not asking the person anything: it is reporting, \
                    working, or saying its work is done, or its question has already been \
                    answered. An empty input box waiting below a report is not a question."
            }
        },
        "running": {
            "type": "noul",
            "instructions": "Does the bottom of `screen` show work still running right now, \
                such as a spinner, a progress bar, an elapsed-time counter or a hint to press \
                Esc to interrupt?",
            "criteria": {
                "true": "Work is visibly in progress at the bottom of the screen.",
                "false": "Nothing is in progress: the agent has stopped at a prompt, a dialog \
                    or a finished message."
            }
        },
        "draft": {
            "type": "noul",
            "instructions": "Does the input area at the bottom of `screen` hold a message the \
                person typed and has not sent yet?",
            "criteria": {
                "true": "Words follow the prompt marker (such as > or ›) in the input area at \
                    the bottom: a box, or the prompt line just above hints such as ? for \
                    shortcuts. Text there has not been sent: a sent message moves up, above it.",
                "false": "The input area is empty, holds only a placeholder suggestion (such \
                    as one starting with Try, or Ask Codex to do anything), or there is no \
                    input area."
            }
        }
    })
}

/// The request for one screen: the model, the lines as the state, and
/// the four questions. Nothing else goes: no agent, project, path or
/// name beyond what is on the screen itself.
pub fn screen_request(model: &str, lines: &[String]) -> Value {
    json!({
        "model": model,
        "state": { "about": ABOUT, "screen": lines.join("\n") },
        "questions": questions(),
    })
}

/// The four answers, each the probability of yes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenAnswers {
    pub approval: f64,
    pub question: f64,
    pub running: f64,
    pub draft: f64,
}

/// A model name the file or an answer may carry: 1–64 letters, digits,
/// dots, hyphens or underscores.
pub fn valid_model(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Read a System One answer: the versioned model that answered and the
/// four probabilities. Anything the contract does not have — a missing
/// answer, another type, a probability outside 0–1 — is not an answer.
pub fn read_answers(body: &Value) -> Result<(String, ScreenAnswers), String> {
    let model = body["model"]
        .as_str()
        .filter(|m| valid_model(m))
        .ok_or("the answer names no model")?
        .to_owned();
    let noul = |name: &str| -> Result<f64, String> {
        let answer = &body["answers"][name];
        if answer["type"].as_str() != Some("noul") {
            return Err(format!("the answer has no yes/no answer for `{name}`"));
        }
        answer["noul"]
            .as_f64()
            .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
            .ok_or_else(|| format!("the answer for `{name}` is not a probability"))
    };
    Ok((
        model,
        ScreenAnswers {
            approval: noul("approval")?,
            question: noul("question")?,
            running: noul("running")?,
            draft: noul("draft")?,
        },
    ))
}

/// Where a promotion starts and what stops one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// The least probability of yes that promotes.
    pub promote: f64,
    /// Running work at least this likely stops a promotion.
    pub running_veto: f64,
}

impl Thresholds {
    pub fn percent(promote: u8) -> Self {
        Self {
            promote: f64::from(promote) / 100.0,
            running_veto: RUNNING_VETO,
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::percent(DEFAULT_THRESHOLD_PERCENT)
    }
}

/// What four answers come to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// The screen waits on the person, for this, this likely.
    Promote {
        reason: WaitReason,
        probability: f64,
    },
    /// Nothing changes.
    Nothing,
}

/// The rule: work read as running stops everything; otherwise the likeliest
/// of an approval prompt, a question and an unsent draft promotes when it
/// reaches the threshold, and the earlier of those wins a tie. A
/// probability that is not one, on either side, does nothing.
pub fn decide(answers: &ScreenAnswers, thresholds: &Thresholds) -> Outcome {
    let probability = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
    let all = [
        answers.approval,
        answers.question,
        answers.running,
        answers.draft,
        thresholds.promote,
        thresholds.running_veto,
    ];
    if !all.into_iter().all(probability) || answers.running >= thresholds.running_veto {
        return Outcome::Nothing;
    }
    let (reason, likeliest) = [
        (WaitReason::Question, answers.question),
        (WaitReason::Draft, answers.draft),
    ]
    .into_iter()
    .fold((WaitReason::Permission, answers.approval), |best, next| {
        if next.1 > best.1 { next } else { best }
    });
    if likeliest >= thresholds.promote {
        Outcome::Promote {
            reason,
            probability: likeliest,
        }
    } else {
        Outcome::Nothing
    }
}

/// A promotion as the agent's record keeps it: what for, how sure, and
/// which screen of which process — the screen by digest, never its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenJudgment {
    pub reason: WaitReason,
    /// The answer's probability of yes, in thousandths.
    pub permille: u16,
    /// When the judged screen last changed: it has been quiet since.
    pub since: DateTime<Utc>,
    /// When the judged screen was read.
    pub judged_at: DateTime<Utc>,
    /// The process whose screen it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<DateTime<Utc>>,
    /// A digest of the judged lines.
    pub screen: String,
    /// The versioned model that answered.
    pub model: String,
}

impl ScreenJudgment {
    /// A probability in thousandths, rounded.
    pub fn permille(probability: f64) -> u16 {
        if probability.is_finite() {
            (probability.clamp(0.0, 1.0) * 1000.0).round() as u16
        } else {
            0
        }
    }

    /// Whether it still stands for `record`: the same live process, and
    /// nothing its own adapter reported since the screen was read. A
    /// report from the agent is first-hand and outranks any reading of
    /// its screen.
    pub fn stands_for(&self, record: &AgentRecord) -> bool {
        record.status.is_live()
            && record.process_started_at == self.process_started_at
            && record
                .reported_activity
                .as_ref()
                .is_none_or(|observation| observation.observed_at <= self.judged_at)
    }
}

/// Apply a standing promotion to what the agent is otherwise doing. It
/// lifts working, idle and unknown to waiting for the person, marked as
/// judged; every other state — a wait its adapter reported, a block on a
/// lease, a start, an end — is returned as it was. It never lowers one.
pub fn raise(activity: Activity, judgment: &ScreenJudgment) -> Activity {
    match activity {
        Activity::Working { .. } | Activity::Idle { .. } | Activity::Unknown => Activity::Waiting {
            since: judgment.since,
            reason: Some(judgment.reason),
            judged: Some(Judged {
                permille: judgment.permille.min(1000),
            }),
        },
        other => other,
    }
}

/// How the judge stands, as `ping` reports it and `daemon status` prints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JudgeStatus {
    /// No judgment is named, or no `[judge]` at all: nothing is read for
    /// judging and nothing is sent.
    Off,
    /// Judging, with the model named and the calls in the last minute
    /// against the budget; `problem` is the last call's failure, in words
    /// that name neither the address nor the key, until a call succeeds.
    On {
        judgments: Vec<String>,
        model: String,
        calls_last_minute: u32,
        calls_per_minute: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// Judgments are named but cannot run: the file or the key is wrong.
    /// Nothing is read or sent until it is put right.
    Unusable { reason: String },
    /// A state this build does not know, from a newer daemon.
    #[serde(other)]
    Unknown,
}

impl JudgeStatus {
    /// One line for a person.
    pub fn summary(&self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::On {
                judgments,
                model,
                calls_last_minute,
                calls_per_minute,
                problem,
            } => {
                let mut line = format!(
                    "on: {} with {model}, {calls_last_minute} of {calls_per_minute} calls in the last minute",
                    judgments.join(", ")
                );
                if let Some(problem) = problem {
                    line.push_str("; last call ");
                    line.push_str(problem);
                }
                line
            }
            Self::Unusable { reason } => format!("not running: {reason}"),
            Self::Unknown => "in a state this build does not know".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActivityObservation, AgentSpec, AgentStatus, ReportedActivity, ResourceKey};
    use proptest::prelude::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn answers(approval: f64, question: f64, running: f64, draft: f64) -> ScreenAnswers {
        ScreenAnswers {
            approval,
            question,
            running,
            draft,
        }
    }

    fn judgment(reason: WaitReason, permille: u16) -> ScreenJudgment {
        ScreenJudgment {
            reason,
            permille,
            since: at(0),
            judged_at: at(20),
            process_started_at: Some(at(-60)),
            screen: "0123456789abcdef".into(),
            model: DEFAULT_MODEL.into(),
        }
    }

    #[test]
    fn escapes_go_and_a_rewritten_line_reads_as_last_drawn() {
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m done"), "ok done");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}text"), "text");
        assert_eq!(strip_ansi("\u{1b}]8;;http://x\u{1b}\\link"), "link");
        assert_eq!(strip_ansi("\u{1b}(Bplain\u{1b}=x"), "plainx");
        assert_eq!(strip_ansi("\u{9b}2Jclear"), "clear");
        assert_eq!(strip_ansi("50%\r60%"), "60%");
        assert_eq!(strip_ansi("abcdef\rxy"), "xycdef");
        assert_eq!(strip_ansi("ab\u{08}c"), "ac");
        assert_eq!(strip_ansi("a\tb\u{07}\u{0}c"), "a bc");
        assert_eq!(strip_ansi("one\r\ntwo"), "one\ntwo");
        assert_eq!(strip_ansi("❯ 1. Yes"), "❯ 1. Yes");
        assert_eq!(strip_ansi("cut \u{1b}["), "cut ");
    }

    #[test]
    fn the_tail_is_the_last_thirty_lines_without_blank_ends() {
        let text: String = (1..=50).map(|i| format!("line {i}   \n")).collect();
        let tail = screen_tail(&format!("{text}\n\n\n"));
        assert_eq!(tail.len(), SCREEN_LINES);
        assert_eq!(tail.first().unwrap(), "line 21");
        assert_eq!(tail.last().unwrap(), "line 50");
        assert_eq!(screen_tail("\n\n  \nonly\n\n"), ["only"]);
        assert!(screen_tail("").is_empty());
        let long = "x".repeat(LINE_CHARS * 2);
        assert_eq!(screen_tail(&long)[0].chars().count(), LINE_CHARS);
    }

    /// Only the lines and the questions are sent: the state has the lines
    /// and a fixed description, the questions are the four nouls.
    #[test]
    fn a_request_carries_the_lines_and_the_questions_only() {
        let lines = vec!["Do you want to proceed?".to_owned(), "❯ 1. Yes".to_owned()];
        let request = screen_request(DEFAULT_MODEL, &lines);
        assert_eq!(request["model"], DEFAULT_MODEL);
        let state = request["state"].as_object().unwrap();
        assert_eq!(
            state.keys().collect::<Vec<_>>(),
            ["about", "screen"],
            "nothing but the screen and what it is"
        );
        assert_eq!(state["screen"], "Do you want to proceed?\n❯ 1. Yes");
        let questions = request["questions"].as_object().unwrap();
        let mut names: Vec<_> = questions.keys().collect();
        names.sort();
        assert_eq!(names, ["approval", "draft", "question", "running"]);
        for question in questions.values() {
            assert_eq!(question["type"], "noul");
            assert!(
                question["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("`screen`")
            );
            assert!(question["criteria"]["true"].is_string());
            assert!(question["criteria"]["false"].is_string());
        }
        let mut fields: Vec<_> = request.as_object().unwrap().keys().collect();
        fields.sort();
        assert_eq!(fields, ["model", "questions", "state"]);
    }

    #[test]
    fn an_answer_is_read_only_when_it_is_the_contracts() {
        let good = json!({
            "model": "jev-1.13.0",
            "answers": {
                "approval": {"type": "noul", "noul": 0.97},
                "question": {"type": "noul", "noul": 0.4},
                "running": {"type": "noul", "noul": 0.02},
                "draft": {"type": "noul", "noul": 0.0}
            },
            "usage": {"input_tokens": 700, "output_tokens": 20}
        });
        let (model, read) = read_answers(&good).unwrap();
        assert_eq!(model, "jev-1.13.0");
        assert_eq!(read, answers(0.97, 0.4, 0.02, 0.0));

        let mut missing = good.clone();
        missing["answers"].as_object_mut().unwrap().remove("draft");
        assert!(read_answers(&missing).is_err());
        let mut other_type = good.clone();
        other_type["answers"]["running"] = json!({"type": "score", "score": 0.5});
        assert!(read_answers(&other_type).is_err());
        let mut out_of_range = good.clone();
        out_of_range["answers"]["approval"]["noul"] = json!(1.2);
        assert!(read_answers(&out_of_range).is_err());
        let mut not_a_number = good.clone();
        not_a_number["answers"]["approval"]["noul"] = json!("0.9");
        assert!(read_answers(&not_a_number).is_err());
        let mut no_model = good;
        no_model["model"] = json!("jev latest");
        assert!(read_answers(&no_model).is_err());
        assert!(read_answers(&json!("not an object")).is_err());
    }

    #[test]
    fn a_clear_answer_promotes_for_the_likeliest_reason() {
        let t = Thresholds::default();
        assert_eq!(
            decide(&answers(0.96, 0.7, 0.03, 0.01), &t),
            Outcome::Promote {
                reason: WaitReason::Permission,
                probability: 0.96
            }
        );
        assert_eq!(
            decide(&answers(0.1, 0.9, 0.1, 0.2), &t),
            Outcome::Promote {
                reason: WaitReason::Question,
                probability: 0.9
            }
        );
        assert_eq!(
            decide(&answers(0.05, 0.1, 0.1, 0.85), &t),
            Outcome::Promote {
                reason: WaitReason::Draft,
                probability: 0.85
            }
        );
        // A tie goes to the earlier: an approval prompt is also a choice.
        assert_eq!(
            decide(&answers(0.9, 0.9, 0.0, 0.0), &t),
            Outcome::Promote {
                reason: WaitReason::Permission,
                probability: 0.9
            }
        );
    }

    #[test]
    fn running_work_or_an_unsure_answer_does_nothing() {
        let t = Thresholds::default();
        assert_eq!(
            decide(&answers(0.99, 0.99, 0.6, 0.99), &t),
            Outcome::Nothing
        );
        assert_eq!(decide(&answers(0.74, 0.5, 0.0, 0.3), &t), Outcome::Nothing);
        assert_eq!(
            decide(&answers(f64::NAN, 0.0, 0.0, 0.0), &t),
            Outcome::Nothing
        );
        assert_eq!(
            decide(
                &answers(0.99, 0.0, 0.0, 0.0),
                &Thresholds {
                    promote: f64::INFINITY,
                    running_veto: 0.5
                }
            ),
            Outcome::Nothing
        );
        assert_eq!(
            decide(&answers(0.85, 0.0, 0.0, 0.0), &Thresholds::percent(90)),
            Outcome::Nothing
        );
    }

    fn every_activity() -> Vec<Activity> {
        vec![
            Activity::Unknown,
            Activity::Starting,
            Activity::Working { since: at(1) },
            Activity::Blocked {
                resource: ResourceKey::new("task:x"),
                held_by: vec![],
                since: at(1),
            },
            Activity::Idle { since: at(1) },
            Activity::Waiting {
                since: at(1),
                reason: Some(WaitReason::Question),
                judged: None,
            },
            Activity::Finished,
        ]
    }

    #[test]
    fn a_promotion_raises_working_idle_and_unknown_and_nothing_else() {
        let judged = judgment(WaitReason::Permission, 960);
        for activity in every_activity() {
            let raised = raise(activity.clone(), &judged);
            match activity {
                Activity::Working { .. } | Activity::Idle { .. } | Activity::Unknown => {
                    assert_eq!(
                        raised,
                        Activity::Waiting {
                            since: at(0),
                            reason: Some(WaitReason::Permission),
                            judged: Some(Judged { permille: 960 })
                        }
                    )
                }
                other => assert_eq!(raised, other, "left as it was"),
            }
        }
        assert_eq!(Judged { permille: 960 }.percent(), "96%");
        assert_eq!(Judged { permille: 1000 }.percent(), "100%");
        assert_eq!(ScreenJudgment::permille(0.9549), 955);
        assert_eq!(ScreenJudgment::permille(f64::NAN), 0);
    }

    #[test]
    fn a_judgment_stands_until_the_agent_says_otherwise_or_is_another_process() {
        let mut record = AgentRecord::new(AgentSpec::default(), false, at(-60));
        record.status = AgentStatus::Running;
        record.process_started_at = Some(at(-60));
        let judged = judgment(WaitReason::Question, 900);
        assert!(judged.stands_for(&record));
        // Read as the wire carries it, so fields added to an observation
        // later need nothing here.
        let observation: ActivityObservation =
            serde_json::from_value(json!({"activity": "working", "observed_at": at(10)})).unwrap();
        assert_eq!(observation.activity, ReportedActivity::Working);
        record.reported_activity = Some(observation);
        assert!(
            judged.stands_for(&record),
            "a report before the screen was read"
        );
        record.reported_activity.as_mut().unwrap().observed_at = at(21);
        assert!(!judged.stands_for(&record), "a report after it outranks it");
        record.reported_activity = None;
        record.process_started_at = Some(at(-5));
        assert!(!judged.stands_for(&record), "another process");
        record.process_started_at = Some(at(-60));
        record.status = AgentStatus::Exited { code: Some(0) };
        assert!(!judged.stands_for(&record), "an ended agent");
    }

    #[test]
    fn judgments_are_named_as_the_file_names_them() {
        assert_eq!(Judgment::parse("screens"), Some(Judgment::Screens));
        assert_eq!(Judgment::parse("summaries"), None);
        assert_eq!(Judgment::Screens.as_str(), "screens");
    }

    #[test]
    fn the_status_reads_and_writes_and_tolerates_newer_states() {
        let on = JudgeStatus::On {
            judgments: vec!["screens".into()],
            model: DEFAULT_MODEL.into(),
            calls_last_minute: 2,
            calls_per_minute: 6,
            problem: Some("timed out".into()),
        };
        let text = serde_json::to_string(&on).unwrap();
        assert_eq!(serde_json::from_str::<JudgeStatus>(&text).unwrap(), on);
        assert_eq!(
            on.summary(),
            "on: screens with jev-1.13.0, 2 of 6 calls in the last minute; last call timed out"
        );
        assert_eq!(
            serde_json::from_str::<JudgeStatus>(r#"{"state":"sleeping"}"#).unwrap(),
            JudgeStatus::Unknown
        );
        assert_eq!(JudgeStatus::Off.summary(), "off");
    }

    fn probability() -> impl Strategy<Value = f64> {
        prop_oneof![
            8 => 0.0f64..=1.0,
            1 => Just(0.0),
            1 => Just(1.0),
            1 => Just(f64::NAN),
            1 => -1.0f64..2.0,
        ]
    }

    proptest! {
        /// Whatever the answers and wherever the threshold, a promotion
        /// never lowers or clears a state: the result is the state given,
        /// or waiting, and waiting only from working, idle or unknown.
        #[test]
        fn it_never_demotes(
            approval in probability(),
            question in probability(),
            running in probability(),
            draft in probability(),
            percent in 50u8..=99,
            which in 0usize..7,
        ) {
            let activity = every_activity()[which].clone();
            let outcome = decide(&answers(approval, question, running, draft), &Thresholds::percent(percent));
            let after = match outcome {
                Outcome::Promote { reason, probability } => {
                    let judged = ScreenJudgment {
                        permille: ScreenJudgment::permille(probability),
                        ..judgment(reason, 0)
                    };
                    raise(activity.clone(), &judged)
                }
                Outcome::Nothing => activity.clone(),
            };
            match (&activity, &after) {
                (before, after) if before == after => {}
                (Activity::Working { .. } | Activity::Idle { .. } | Activity::Unknown,
                 Activity::Waiting { judged: Some(_), .. }) => {}
                (before, after) => prop_assert!(false, "{before:?} became {after:?}"),
            }
        }

        /// Below the threshold, or with work read as running, nothing.
        #[test]
        fn below_the_threshold_nothing_happens(
            approval in 0.0f64..1.0,
            question in 0.0f64..1.0,
            running in 0.0f64..1.0,
            draft in 0.0f64..1.0,
            percent in 50u8..=99,
        ) {
            let t = Thresholds::percent(percent);
            let outcome = decide(&answers(approval, question, running, draft), &t);
            let likeliest = approval.max(question).max(draft);
            if likeliest < t.promote || running >= t.running_veto {
                prop_assert_eq!(outcome, Outcome::Nothing);
            } else {
                match outcome {
                    Outcome::Promote { probability, .. } => {
                        prop_assert!(probability >= t.promote);
                        prop_assert_eq!(probability, likeliest);
                    }
                    Outcome::Nothing => prop_assert!(false, "a clear answer promotes"),
                }
            }
        }
    }
}
