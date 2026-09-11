//! Where an agent lives, when that is somebody else's terminal.
//!
//! We do not write a multiplexer. `tmux` exists, `screen` exists, herdr
//! exists, and a multiplexer is not the working set. What is worth
//! having is the ability to *notice* one: an agent discovered running
//! inside a tmux pane or a herdr session is not homeless, it is
//! somebody's guest, and saying so lets a person attach with the tool
//! they already use instead of ours.
//!
//! This module is the conventions themselves: what each multiplexer
//! calls its own variables, and what a session read out of them looks
//! like. Reading another process's environment, and the weaker ancestry
//! fallback, are host I/O and live in `agentdocker-host::multiplexer`.
//!
//! Nothing here guesses. A multiplexer we have no evidence for is
//! reported as no session at all rather than as a maybe.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where an agent lives, when that is somebody else's terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// `tmux`, `screen`, `zellij`, `herdr`.
    pub kind: String,
    /// The session as its own tool names it, where it says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The pane or window inside that session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// How we know: `environment` is what the multiplexer told its own
    /// child; `ancestry` is one seen between the agent and its shell.
    pub evidence: Evidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// The multiplexer's own variables, in the agent's environment.
    Environment,
    /// A multiplexer process between the agent and its shell.
    Ancestry,
}

impl Session {
    /// One line for a column: `tmux:%3` or `herdr (ancestry)`.
    pub fn describe(&self) -> String {
        let mut text = self.kind.clone();
        if let Some(pane) = &self.pane {
            text.push(':');
            text.push_str(pane);
        } else if let Some(session) = &self.session {
            text.push(':');
            text.push_str(session);
        }
        if self.evidence == Evidence::Ancestry {
            text.push_str(" (ancestry)");
        }
        text
    }
}

/// What the environment of a process says about where it lives.
///
/// Pure, so every multiplexer's convention can be tested without having
/// that multiplexer installed.
pub fn from_environment(env: &BTreeMap<String, String>) -> Option<Session> {
    let get = |key: &str| env.get(key).map(String::as_str).filter(|v| !v.is_empty());

    // tmux sets TMUX to `<socket>,<pid>,<session>` and TMUX_PANE to the
    // pane id. The pane is what a person needs to attach to.
    if let Some(pane) = get("TMUX_PANE") {
        return Some(Session {
            kind: "tmux".to_owned(),
            session: get("TMUX").and_then(|tmux| tmux.rsplit(',').next().map(str::to_owned)),
            pane: Some(pane.to_owned()),
            evidence: Evidence::Environment,
        });
    }
    if get("TMUX").is_some() {
        return Some(Session {
            kind: "tmux".to_owned(),
            session: get("TMUX").and_then(|tmux| tmux.rsplit(',').next().map(str::to_owned)),
            pane: None,
            evidence: Evidence::Environment,
        });
    }
    // zellij names the session and, in recent versions, the pane.
    if let Some(session) = get("ZELLIJ_SESSION_NAME") {
        return Some(Session {
            kind: "zellij".to_owned(),
            session: Some(session.to_owned()),
            pane: get("ZELLIJ_PANE_ID").map(str::to_owned),
            evidence: Evidence::Environment,
        });
    }
    if get("ZELLIJ").is_some() {
        return Some(Session {
            kind: "zellij".to_owned(),
            session: None,
            pane: get("ZELLIJ_PANE_ID").map(str::to_owned),
            evidence: Evidence::Environment,
        });
    }
    // screen sets STY to `<pid>.<tty>.<host>` and WINDOW to the window
    // number.
    if let Some(sty) = get("STY") {
        return Some(Session {
            kind: "screen".to_owned(),
            session: Some(sty.to_owned()),
            pane: get("WINDOW").map(str::to_owned),
            evidence: Evidence::Environment,
        });
    }
    // herdr (0.9) marks every pane process with HERDR_ENV=1 and names the
    // pane in HERDR_PANE_ID (`w1:p1`). HERDR_SESSION is set only inside a
    // named session; the default session has no name, so the pane id has
    // to carry the evidence on its own there.
    let herdr_pane = get("HERDR_PANE").or_else(|| get("HERDR_PANE_ID"));
    let herdr_session = ["HERDR_SESSION", "HERDR_SESSION_ID", "HERDR"]
        .into_iter()
        .find_map(get);
    if herdr_session.is_some() || (get("HERDR_ENV") == Some("1") && herdr_pane.is_some()) {
        return Some(Session {
            kind: "herdr".to_owned(),
            session: herdr_session.map(str::to_owned),
            pane: herdr_pane.map(str::to_owned),
            evidence: Evidence::Environment,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn tmux_is_read_from_its_own_variables() {
        let session = from_environment(&env(&[
            ("TMUX", "/private/tmp/tmux-501/default,1234,work"),
            ("TMUX_PANE", "%3"),
        ]))
        .expect("tmux says where its children are");
        assert_eq!(session.kind, "tmux");
        assert_eq!(session.pane.as_deref(), Some("%3"));
        assert_eq!(session.session.as_deref(), Some("work"));
        assert_eq!(session.evidence, Evidence::Environment);
        assert_eq!(session.describe(), "tmux:%3");
    }

    #[test]
    fn tmux_without_a_pane_is_still_tmux() {
        let session = from_environment(&env(&[("TMUX", "/tmp/tmux-501/default,1234,0")])).unwrap();
        assert_eq!(session.kind, "tmux");
        assert_eq!(session.pane, None);
        assert_eq!(session.describe(), "tmux:0");
    }

    #[test]
    fn zellij_screen_and_herdr_each_name_their_own() {
        let zellij = from_environment(&env(&[
            ("ZELLIJ", "0"),
            ("ZELLIJ_SESSION_NAME", "review"),
            ("ZELLIJ_PANE_ID", "7"),
        ]))
        .unwrap();
        assert_eq!(zellij.kind, "zellij");
        assert_eq!(zellij.session.as_deref(), Some("review"));
        assert_eq!(zellij.pane.as_deref(), Some("7"));

        let screen =
            from_environment(&env(&[("STY", "4242.pts-3.host"), ("WINDOW", "2")])).unwrap();
        assert_eq!(screen.kind, "screen");
        assert_eq!(screen.session.as_deref(), Some("4242.pts-3.host"));
        assert_eq!(screen.pane.as_deref(), Some("2"));

        let herdr = from_environment(&env(&[("HERDR_SESSION", "backend")])).unwrap();
        assert_eq!(herdr.kind, "herdr");
        assert_eq!(herdr.session.as_deref(), Some("backend"));
    }

    #[test]
    fn herdr_default_session_is_known_by_its_pane() {
        // What herdr 0.9 injects into a pane of the unnamed default
        // session: no HERDR_SESSION, but HERDR_ENV=1 and the public ids.
        let herdr = from_environment(&env(&[
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p1"),
            ("HERDR_TAB_ID", "w1:t1"),
            ("HERDR_WORKSPACE_ID", "w1"),
            ("HERDR_SOCKET_PATH", "/Users/me/.config/herdr/herdr.sock"),
        ]))
        .expect("a herdr pane names itself");
        assert_eq!(herdr.kind, "herdr");
        assert_eq!(herdr.session, None);
        assert_eq!(herdr.pane.as_deref(), Some("w1:p1"));
        assert_eq!(herdr.describe(), "herdr:w1:p1");

        let named = from_environment(&env(&[
            ("HERDR_ENV", "1"),
            ("HERDR_SESSION", "ad-probe"),
            ("HERDR_PANE_ID", "w1:p2"),
        ]))
        .unwrap();
        assert_eq!(named.session.as_deref(), Some("ad-probe"));
        assert_eq!(named.describe(), "herdr:w1:p2");

        // HERDR_ENV alone, or a pane id without the marker, is a leftover
        // export, not evidence of a pane.
        assert!(from_environment(&env(&[("HERDR_ENV", "1")])).is_none());
        assert!(from_environment(&env(&[("HERDR_PANE_ID", "w1:p1")])).is_none());
    }

    #[test]
    fn an_ordinary_shell_is_not_a_multiplexer() {
        assert!(from_environment(&env(&[("SHELL", "/bin/zsh"), ("TERM", "xterm")])).is_none());
        // An empty variable is not evidence: a leftover export says
        // nothing about where this process is.
        assert!(from_environment(&env(&[("TMUX", ""), ("STY", "")])).is_none());
    }
}
