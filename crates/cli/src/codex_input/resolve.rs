//! Closing, on the person's confirmed word, a Codex question a stopped
//! managed controller left with a response that may have reached Codex.
//! Nothing is ever sent to Codex again: the question is closed as
//! cancelled with its response kept, its retained replies archived, and the
//! person's intent written to the journal first. Only the stopped
//! controller's own record is opened, never a new one, and its owner lock is
//! held throughout, so a running controller refuses this.
use super::{
    Client, Ledger, call,
    ledger::Binding,
    requests,
    review::{AtRestart, Outcome},
};
use agentdocker_core::{AgentRecord, Request, Response};
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// The managed Codex agent whose retained questions to review.
    #[arg(long)]
    pub agent: String,
    /// The request to close, by its ID as the preview prints it (JSON: a
    /// number, or a quoted string). Leave out for a read-only preview.
    #[arg(long, requires_all = ["confirm", "note"])]
    pub request: Option<String>,
    /// The confirmation the preview printed for that request.
    #[arg(long, requires = "request")]
    pub confirm: Option<String>,
    /// Why it is closed, written to the journal.
    #[arg(long, requires = "request")]
    pub note: Option<String>,
}

pub async fn run(client: Client, socket: Option<PathBuf>, args: Args) -> Result<()> {
    let client = client.with_start_timeout(None);
    let home = dirs::home();
    let socket = socket.unwrap_or_else(|| dirs::socket_path(&home));
    let agent = match call(
        &client,
        Request::Inspect {
            agent: args.agent.clone(),
        },
    )
    .await?
    {
        Response::Agent { agent } => agent,
        other => bail!("cannot find that agent: {other:?}"),
    };
    let binding = binding(&agent, socket, super::provider_home()?)?;
    let mut ledger = Ledger::open_existing(&home, binding)?;
    match (args.request, args.confirm, args.note) {
        (Some(request), Some(confirm), Some(note)) => {
            println!(
                "{}",
                close(&client, &mut ledger, &request, &confirm, &note).await?
            );
        }
        _ => print!("{}", preview(&ledger)),
    }
    Ok(())
}

/// The binding a managed controller of `agent` holds: its checkout, the
/// daemon's socket and the Codex profile.
fn binding(agent: &AgentRecord, socket: PathBuf, provider_home: PathBuf) -> Result<Binding> {
    let cwd = agent
        .spec
        .workdir
        .as_deref()
        .context("the agent names no checkout")?
        .canonicalize()
        .context("the agent's checkout cannot be resolved")?;
    Ok(Binding {
        agent: agent.id.to_string(),
        socket,
        cwd,
        provider_home,
    })
}

/// What the record holds for the person to review, read-only.
fn preview(ledger: &Ledger) -> String {
    let record = ledger.record();
    let binding = &record.binding;
    let mut out = String::new();
    if record.reviews.is_empty() {
        out.push_str("No retained Codex question.\n");
    }
    for request in &record.reviews {
        let (state, what) = match request.at_restart() {
            AtRestart::ResponseUncertain => (
                "response may have reached Codex",
                format!(
                    "close it with --request '{}' --confirm {} --note \"…\" (nothing is resent)",
                    request.id,
                    request.confirmation(binding)
                ),
            ),
            AtRestart::PublicationUncertain => (
                "publication uncertain",
                "this command can show it but cannot resolve it".to_owned(),
            ),
            AtRestart::NeverAnswered => (
                "never answered",
                "the controller closes it as cancelled when it starts".to_owned(),
            ),
        };
        out.push_str(&format!("\n{state}:\n"));
        for line in request.evidence() {
            out.push_str(&format!("  {line}\n"));
        }
        out.push_str(&format!("  {what}\n"));
    }
    for closed in record.closed_reviews.iter().filter(|c| !c.acknowledged) {
        out.push_str(&format!(
            "\nclosed, its replies not yet acknowledged: request {} (the next start, or this command with its confirmation, finishes it)\n",
            closed.request.id
        ));
    }
    if record.secret_review.is_some() {
        out.push_str(
            "\nA temporary response fence is retained; this command does not handle it.\n",
        );
    }
    out
}

/// Close one request whose response may have reached Codex: its
/// confirmation checked before anything else, the person's intent written
/// to the journal, then the archival close as cancelled, then its replies
/// acknowledged. A request already closed with the same confirmation only
/// has its acknowledgement finished.
async fn close(
    client: &Client,
    ledger: &mut Ledger,
    request: &str,
    confirm: &str,
    note: &str,
) -> Result<String> {
    let id: Value = serde_json::from_str(request).context(
        "the request ID is JSON, as the preview prints it: a number, or a quoted string",
    )?;
    let binding = ledger.record().binding.clone();
    if let Some(closed) = ledger
        .record()
        .closed_reviews
        .iter()
        .find(|c| c.request.id == id && c.request.confirmation(&binding) == confirm)
    {
        if !closed.acknowledged {
            requests::acknowledge_archived(client, ledger).await?;
        }
        return Ok(format!(
            "Request {id} is already closed; nothing was resent, and its retained replies are acknowledged."
        ));
    }
    let pending = ledger
        .record()
        .reviews
        .iter()
        .find(|r| r.id == id)
        .context("no such retained question; review it again")?;
    ensure!(
        pending.confirmation(&binding) == confirm,
        "the retained question is not the one that confirmation names; review it again"
    );
    match pending.at_restart() {
        AtRestart::ResponseUncertain => {}
        AtRestart::PublicationUncertain => bail!(
            "this question's publication is uncertain; this command can show it but cannot resolve it"
        ),
        AtRestart::NeverAnswered => bail!(
            "this question was never answered; the controller closes it as cancelled when it starts"
        ),
    }
    let pid = std::process::id();
    let born = procinfo::start_time(pid).map_or_else(|| "unknown".to_owned(), |at| at.to_rfc3339());
    let summary = format!(
        "Manual close {confirm} of Codex request {id} on thread {} / turn {}: its response was written and may have reached Codex; nothing is resent. Operator pid {pid} born {born}. {note}",
        pending.thread,
        pending.turn.as_deref().unwrap_or("(none)"),
    );
    ensure!(
        matches!(
            call(
                client,
                Request::JournalAdd {
                    agent: binding.agent.clone(),
                    summary,
                },
            )
            .await?,
            Response::JournalEntry { .. }
        ),
        "the journal did not confirm the intent; nothing was closed"
    );
    requests::close_archived(client, ledger, &id, Outcome::Cancelled).await?;
    Ok(format!(
        "Manually closed request {id}: its response may have reached Codex; nothing was resent, and its retained replies are archived."
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::codex_input::requests::tests::{
        asked, cancelled, client, fixture, never_answered, read, refused, reply, script,
    };
    use crate::codex_input::review::Pending;
    use agentdocker_core::{AgentSpec, MessageId};
    use serde_json::json;

    fn journaled() -> Response {
        Response::JournalEntry {
            entry: serde_json::from_value(json!({
                "project":"project","at":"2026-01-01T00:00:00Z","agent_name":"owner",
                "kind":"note","summary":"noted","summary_source":"explicit"
            }))
            .unwrap(),
        }
    }

    fn confirmation(ledger: &Ledger, id: &Value) -> String {
        let record = ledger.record();
        record
            .reviews
            .iter()
            .find(|r| &r.id == id)
            .unwrap()
            .confirmation(&record.binding)
    }

    /// The fixture's uncertain response beside a question never answered
    /// and one whose second publication is uncertain.
    fn every_state() -> (tempfile::TempDir, Ledger) {
        let (home, mut ledger, _) = fixture();
        never_answered(&mut ledger, 18, "open-question");
        let event = json!({"id":"ask","method":"item/tool/requestUserInput","params":{"threadId":"thread","turnId":"turn","questions":[{"id":"first","question":"Choose?"},{"id":"second","question":"Provide a value?"}]}});
        let mut pending =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        pending.questions[0].message = Some("first-route".to_owned().into());
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        (home, ledger)
    }

    #[test]
    fn the_binding_is_the_agents_resolved_checkout() {
        let home = tempfile::tempdir().unwrap();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "codex-1".into(),
                runtime: "codex".into(),
                workdir: Some(home.path().join(".")),
                ..Default::default()
            },
            true,
            chrono::Utc::now(),
        );
        let bound = binding(&agent, "/sock".into(), "/profile".into()).unwrap();
        assert_eq!(bound.agent, agent.id.to_string());
        assert_eq!(bound.cwd, home.path().canonicalize().unwrap());
        agent.spec.workdir = None;
        assert!(binding(&agent, "/sock".into(), "/profile".into()).is_err());
    }

    #[test]
    fn the_preview_offers_an_action_only_for_a_response_that_may_have_reached_codex() {
        let (_home, ledger) = every_state();
        let before = std::fs::read(ledger.file_path()).unwrap();
        let shown = preview(&ledger);
        assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
        let digest = confirmation(&ledger, &json!(17));
        assert!(
            shown.contains(&format!(
                "close it with --request '17' --confirm {digest} --note \"…\" (nothing is resent)"
            )),
            "{shown}"
        );
        assert!(
            shown.contains("\npublication uncertain:\n  request \"ask\""),
            "{shown}"
        );
        assert!(shown.contains("question second (no route): "), "{shown}");
        assert!(
            shown.contains("this command can show it but cannot resolve it"),
            "{shown}"
        );
        assert!(shown.contains("\nnever answered:\n  request 18"), "{shown}");
        assert_eq!(shown.matches("--confirm").count(), 1, "{shown}");
    }

    #[tokio::test]
    async fn a_close_that_names_the_wrong_question_is_refused_before_any_request() {
        // Nothing listens: any request would fail with another message.
        let (_home, mut ledger) = every_state();
        let before = std::fs::read(ledger.file_path()).unwrap();
        let client = client(&ledger);
        let digest = confirmation(&ledger, &json!(17));
        for (request, confirm, refusal) in [
            ("17", "0".repeat(64), "not the one that confirmation names"),
            ("\"17\"", digest.clone(), "no such retained question"),
            ("seventeen", digest.clone(), "the request ID is JSON"),
            (
                "\"ask\"",
                confirmation(&ledger, &json!("ask")),
                "can show it but cannot resolve it",
            ),
            (
                "18",
                confirmation(&ledger, &json!(18)),
                "closes it as cancelled when it starts",
            ),
        ] {
            let error = close(&client, &mut ledger, request, &confirm, "looked")
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(refusal),
                "{request}: {error:#}"
            );
            assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn a_journal_that_does_not_confirm_the_intent_closes_nothing() {
        for answer in [refused("journal"), Response::Ok] {
            let (_home, mut ledger, _) = fixture();
            let before = std::fs::read(ledger.file_path()).unwrap();
            let digest = confirmation(&ledger, &json!(17));
            let serving = script(&ledger.record().binding.socket, vec![answer]);
            assert!(
                close(&client(&ledger), &mut ledger, "17", &digest, "looked")
                    .await
                    .is_err()
            );
            let asked = asked(serving).await;
            assert!(matches!(&asked[0], Request::JournalAdd { agent, .. } if agent == "owner"));
            assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
            assert_eq!(ledger.record().reviews.len(), 1);
        }
    }

    #[tokio::test]
    async fn a_confirmed_close_says_what_it_did_and_lifts_the_restart_refusal() {
        let (_home, mut ledger, answer) = fixture();
        let digest = confirmation(&ledger, &json!(17));
        let serving = script(
            &ledger.record().binding.socket,
            vec![
                journaled(),
                Response::Ok,
                Response::Messages {
                    messages: vec![answer.clone()],
                },
                Response::Messages {
                    messages: Vec::new(),
                },
            ],
        );
        let said = close(&client(&ledger), &mut ledger, "17", &digest, "looked")
            .await
            .unwrap();
        assert_eq!(
            said,
            "Manually closed request 17: its response may have reached Codex; nothing was resent, and its retained replies are archived."
        );
        assert_eq!(asked(serving).await.len(), 4);
        assert!(ledger.record().closed_reviews[0].acknowledged);
        requests::recover(&client(&ledger), &mut ledger)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_confirmed_close_journals_archives_and_acknowledges_and_a_retry_finishes_it() {
        let (home, mut ledger, answer) = fixture();
        let digest = confirmation(&ledger, &json!(17));
        // A reply that arrives after the preview is archived with the rest.
        let late = reply("human", "question", "also this");
        let serving = script(
            &ledger.record().binding.socket,
            vec![
                journaled(),
                Response::Ok,
                Response::Messages {
                    messages: vec![answer.clone(), late.clone()],
                },
                refused("acknowledge"),
            ],
        );
        let error = close(
            &client(&ledger),
            &mut ledger,
            "17",
            &digest,
            "looked at the conversation",
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("acknowledge"), "{error:#}");
        let asked_first = asked(serving).await;
        assert!(
            matches!(&asked_first[0], Request::JournalAdd { agent, summary }
            if agent == "owner"
                && summary.starts_with(&format!("Manual close {digest} of Codex request 17 on thread thread / turn turn: "))
                && summary.contains("may have reached Codex; nothing is resent")
                && summary.ends_with("looked at the conversation"))
        );
        assert!(cancelled(&asked_first[1], "question"));
        assert!(read(&asked_first[2], &[]));
        assert!(read(&asked_first[3], &[&answer.id, &late.id]));
        let binding = ledger.record().binding.clone();
        drop(ledger);

        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        let record = ledger.record();
        assert!(record.reviews.is_empty());
        let closed = &record.closed_reviews[0];
        assert_eq!(closed.outcome, Outcome::Cancelled);
        assert!(!closed.acknowledged);
        assert_eq!(
            closed.request.response.as_ref().unwrap()["result"]["decision"],
            "accept"
        );
        assert_eq!(closed.request.confirmation(&record.binding), digest);
        let serving = script(
            &ledger.record().binding.socket,
            vec![Response::Messages {
                messages: Vec::new(),
            }],
        );
        let said = close(&client(&ledger), &mut ledger, "17", &digest, "again")
            .await
            .unwrap();
        assert!(said.starts_with("Request 17 is already closed"), "{said}");
        let ids: [&MessageId; 2] = [&answer.id, &late.id];
        assert!(read(&asked(serving).await[0], &ids));
        assert!(ledger.record().closed_reviews[0].acknowledged);

        // Settled: a retry asks nothing, and the controller's start passes.
        let said = close(&client(&ledger), &mut ledger, "17", &digest, "again")
            .await
            .unwrap();
        assert!(said.starts_with("Request 17 is already closed"), "{said}");
        requests::recover(&client(&ledger), &mut ledger)
            .await
            .unwrap();
    }
}
