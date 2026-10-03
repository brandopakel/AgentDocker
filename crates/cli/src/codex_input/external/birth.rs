//! A private owned-launch receipt for a newly witnessed native thread.
//!
//! This is not a history-error fallback. The launcher must observe an empty
//! dedicated server, start its sole TUI, then witness that thread's creation.
//! Admission additionally requires both children still belong to that exact
//! live launcher, pristine receiver state and unchanged empty thread metadata.
use agentdocker_core::{ProcessIdentity, ProviderGeneration};
use agentdocker_host::procinfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Witness {
    pub launcher: ProcessIdentity,
    pub created_at: i64,
}

fn trace(stage: &str) {
    if std::env::var_os("AGENTDOCKER_TRACE_NATIVE_STARTUP").is_some() {
        eprintln!("native-startup: {stage}");
    }
}

impl Witness {
    pub fn valid(&self, provider: &ProviderGeneration, server: &ProcessIdentity) -> bool {
        self.launcher.pid > 0
            && self.launcher.pid != provider.process.pid
            && self.launcher.pid != server.pid
            && self.launcher.started_at <= server.started_at
            && self.launcher.started_at <= provider.process.started_at
            && self.created_at >= server.started_at.timestamp()
            && self.created_at >= provider.process.started_at.timestamp()
    }

    pub fn owns_children(&self, provider: &ProviderGeneration, server: &ProcessIdentity) -> bool {
        if !self.valid(provider, server) {
            trace("birth receipt generations do not match");
            return false;
        }
        if procinfo::start_time(self.launcher.pid) != Some(self.launcher.started_at) {
            trace("launcher generation no longer matches");
            return false;
        }
        for (pid, label) in [(provider.process.pid, "terminal"), (server.pid, "server")] {
            let Some(child) = procinfo::inspect(pid) else {
                trace(if label == "terminal" {
                    "terminal ancestry unavailable"
                } else {
                    "server ancestry unavailable"
                });
                return false;
            };
            if child.ppid != self.launcher.pid {
                trace(if label == "terminal" {
                    "terminal is not a direct launcher child"
                } else {
                    "server is not a direct launcher child"
                });
                return false;
            }
        }
        true
    }

    pub fn matches_empty(&self, thread: &Value, provider: &ProviderGeneration, cwd: &Path) -> bool {
        thread["id"].as_str() == Some(&provider.session)
            && thread["sessionId"].as_str() == Some(&provider.session)
            && thread["cwd"]
                .as_str()
                .and_then(|p| Path::new(p).canonicalize().ok())
                .as_deref()
                == Some(cwd)
            && thread["createdAt"].as_i64() == Some(self.created_at)
            && thread["updatedAt"].as_i64() == Some(self.created_at)
            && thread["preview"].as_str() == Some("")
            && thread["status"]["type"].as_str() == Some("idle")
            && thread["threadSource"].as_str() == Some("user")
            && thread.get("forkedFromId").is_some_and(Value::is_null)
            && thread.get("parentThreadId").is_some_and(Value::is_null)
            && thread["ephemeral"].as_bool() == Some(false)
            && thread["turns"].as_array().is_some_and(Vec::is_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    #[test]
    fn fresh_thread_metadata_rejects_old_forked_busy_and_incomplete_sessions() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let provider = ProviderGeneration {
            process: ProcessIdentity {
                pid: 2,
                started_at: Utc.timestamp_opt(100, 0).unwrap(),
            },
            profile: cwd.to_string_lossy().into_owned(),
            session: "new-thread".into(),
        };
        let witness = Witness {
            launcher: ProcessIdentity {
                pid: 1,
                started_at: Utc.timestamp_opt(99, 0).unwrap(),
            },
            created_at: 100,
        };
        let thread = json!({"id":"new-thread", "sessionId":"new-thread", "cwd":cwd,
            "createdAt":100,"updatedAt":100,"preview":"","status":{"type":"idle"},
            "threadSource":"user","forkedFromId":null,"parentThreadId":null,"ephemeral":false,"turns":[]});
        assert!(witness.matches_empty(&thread, &provider, &cwd));
        for (field, value) in [
            ("id", json!("old-thread")),
            ("sessionId", json!("other")),
            ("cwd", json!(cwd.join("missing"))),
            ("createdAt", json!(99)),
            ("updatedAt", json!(101)),
            ("preview", json!("previous input")),
            ("status", json!({"type":"active"})),
            ("threadSource", json!("subagent")),
            ("forkedFromId", json!("original")),
            ("parentThreadId", json!("parent")),
            ("ephemeral", json!(true)),
            ("turns", json!([{"id":"turn"}])),
        ] {
            let mut changed = thread.clone();
            changed[field] = value;
            assert!(
                !witness.matches_empty(&changed, &provider, &cwd),
                "accepted changed {field}"
            );
            let mut missing = thread.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                !witness.matches_empty(&missing, &provider, &cwd),
                "accepted absent {field}"
            );
        }
        let server = ProcessIdentity {
            pid: 3,
            started_at: Utc.timestamp_opt(100, 0).unwrap(),
        };
        assert!(witness.valid(&provider, &server));
        let mut stale = witness.clone();
        stale.created_at = 99;
        assert!(!stale.valid(&provider, &server));
        stale = witness.clone();
        stale.launcher.pid = 3;
        assert!(!stale.valid(&provider, &server));
        stale = witness;
        stale.launcher.started_at = Utc.timestamp_opt(101, 0).unwrap();
        assert!(!stale.valid(&provider, &server));
    }
}
