//! Desktop notifications, so a question put to the human is noticed.
//!
//! An agent that asks a person something and gets no answer is stuck, and
//! a person cannot answer a question they never saw. The daemon has no
//! window of its own, so it borrows the desktop's: `terminal-notifier` or
//! `osascript` on macOS, `notify-send` on Linux.
//!
//! Best-effort by design. A headless box has none of these, and that is
//! not an error — the message is still queued, still in the inbox, still
//! on the event stream. Nothing here waits for the notification to be
//! dismissed, and nothing here reports whether anyone looked.

use std::process::{Command, Stdio};

/// What a person should see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// Post a notification, returning whether a tool was found to post it
/// with. Blocking: the child is waited for, because the tools are
/// short-lived and a stray unreaped child is worse than a millisecond.
pub fn post(notification: &Notification) -> bool {
    for argv in candidates(notification) {
        let Some((program, args)) = argv.split_first() else {
            continue;
        };
        let ran = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(status) = ran
            && status.success()
        {
            return true;
        }
    }
    false
}

/// The command lines worth trying on this platform, best first.
fn candidates(notification: &Notification) -> Vec<Vec<String>> {
    let Notification { title, body } = notification;
    if cfg!(target_os = "macos") {
        vec![
            vec![
                "terminal-notifier".into(),
                "-title".into(),
                title.clone(),
                "-message".into(),
                body.clone(),
                // Grouping by a fixed id replaces our previous notification
                // rather than stacking a column of them.
                "-group".into(),
                "dev.agentdocker".into(),
            ],
            vec!["osascript".into(), "-e".into(), applescript(title, body)],
        ]
    } else {
        vec![vec![
            "notify-send".into(),
            "--app-name=AgentDocker".into(),
            title.clone(),
            body.clone(),
        ]]
    }
}

/// AppleScript has no parameters, so the text goes into the source. Quote
/// it properly rather than hoping: a message is arbitrary text and may
/// well contain a quote or a backslash.
fn applescript(title: &str, body: &str) -> String {
    format!(
        "display notification {} with title {}",
        quote(body),
        quote(title)
    )
}

fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            // A literal newline ends the AppleScript statement.
            '\n' | '\r' => quoted.push(' '),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Trim a message to something a notification can show, on a word
/// boundary where there is one.
pub fn summarise(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= limit {
        return text;
    }
    let keep = limit.saturating_sub(1);
    let cut: String = text.chars().take(keep).collect();
    // A cut that already fell between two words needs no trimming back.
    let head = if text.chars().nth(keep) == Some(' ') {
        cut.as_str()
    } else {
        match cut.rsplit_once(' ') {
            // Trimming back to a boundary is only worth it when a useful
            // amount of the text survives; otherwise cut mid-word.
            Some((head, _)) if head.chars().count() >= limit / 2 => head,
            _ => cut.as_str(),
        }
    };
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_survives_quotes_and_newlines() {
        let script = applescript("a \"title\"", "line one\nline \\ two");
        assert_eq!(
            script,
            r#"display notification "line one line \\ two" with title "a \"title\"""#
        );
        assert!(!script.contains('\n'), "one statement, one line: {script}");
    }

    #[test]
    fn summarise_trims_on_a_word_boundary() {
        assert_eq!(summarise("short enough", 40), "short enough");
        assert_eq!(
            summarise("the quick brown fox jumps over it", 20),
            "the quick brown fox…"
        );
        // No usable boundary: cut mid-word rather than return almost nothing.
        assert_eq!(summarise("aaaaaaaaaaaaaaaaaaaa b", 10), "aaaaaaaaa…");
        // Whitespace, including newlines, is collapsed first.
        assert_eq!(summarise("two\n\nlines", 40), "two lines");
    }

    #[test]
    fn candidates_are_platform_shaped() {
        let candidates = candidates(&Notification {
            title: "t".into(),
            body: "b".into(),
        });
        assert!(!candidates.is_empty());
        let first = &candidates[0][0];
        if cfg!(target_os = "macos") {
            assert_eq!(first, "terminal-notifier");
        } else {
            assert_eq!(first, "notify-send");
        }
    }
}
