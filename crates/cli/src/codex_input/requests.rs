//! Human answers remain in the shared queue until their provider request has
//! resolved. Closed/cancelled responses are archived, never sent as new turns.
use super::{
    Client, Ledger, Provider, call, queue,
    review::{self, AtRestart, Closed, Outcome, Pending},
};
use agentdocker_core::{Envelope, Request, Response};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub(super) async fn open(
    client: &Client,
    ledger: &mut Ledger,
    human: &str,
    thread: &str,
    turn: Option<&str>,
    event: Value,
    files: &mut super::file_changes::Reviews,
) -> Result<Option<Value>> {
    if ledger.record().secret_review.is_some() {
        return Ok(Some(super::secret_requests::overlap_response(&event["id"])));
    }
    let planned = if event["method"] == "item/fileChange/requestApproval" {
        files
            .presentation(&event, &ledger.record().binding.cwd)
            .and_then(|files| {
                Pending::plan_with_files(
                    &event,
                    thread,
                    turn,
                    human,
                    chrono::Utc::now(),
                    Some(files),
                )
            })
    } else {
        Pending::plan(&event, thread, turn, human, chrono::Utc::now())
    };
    let pending = match planned {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("Codex request could not be completed: {error:#}");
            return Ok(Some(
                json!({"id":event["id"],"error":{"code":-32000,"message":error.to_string()}}),
            ));
        }
    };
    let id = pending.id.clone();
    // This durable boundary precedes even question publication. If publication
    // succeeds but its reply is lost, restart still refuses normal queue input.
    ledger.update_reviews(|reviews, _| {
        reviews.push(pending);
        Ok(true)
    })?;
    let count = ledger
        .record()
        .reviews
        .last()
        .context("provider request disappeared")?
        .questions
        .len();
    for index in 0..count {
        let pending = ledger
            .record()
            .reviews
            .iter()
            .find(|r| r.id == id)
            .context("provider request disappeared")?;
        let result = call(
            client,
            Request::PostQuestion {
                from: ledger.record().binding.agent.clone(),
                to: pending.human.clone(),
                question: pending.questions[index].text.clone(),
                presentation: pending.questions[index].presentation.clone(),
                timeout_secs: review::REVIEW_WINDOW_SECS,
                form: Default::default(),
            },
        )
        .await?;
        let Response::Sent { message, .. } = result else {
            bail!(
                "daemon did not create the provider question; update the daemon before using this mode"
            );
        };
        ledger.update_reviews(|reviews, _| {
            let pending = reviews
                .iter_mut()
                .find(|r| r.id == id)
                .context("provider request disappeared")?;
            ensure!(
                pending.questions[index].message.is_none(),
                "provider question was already posted"
            );
            pending.questions[index].message = Some(message);
            Ok(true)
        })?;
    }
    Ok(None)
}

fn capture(ledger: &mut Ledger, messages: &[Envelope]) -> Result<()> {
    let agent = ledger.record().binding.agent.clone();
    ledger.update_reviews(|reviews, closed| {
        let mut changed = false;
        for request in reviews {
            changed |= request.capture(messages, &agent, false)?;
        }
        for completed in closed {
            if completed.request.capture(messages, &agent, true)? {
                completed.acknowledged = false;
                changed = true;
                eprintln!(
                    "A later answer to a closed Codex question was retained without applying it."
                );
            }
        }
        Ok(changed)
    })
}

pub(super) fn observe(ledger: &mut Ledger, event: &agentdocker_core::EventKind) -> Result<()> {
    let agent = ledger.record().binding.agent.clone();
    ledger.update_reviews(|reviews, closed| {
        let mut changed = false;
        for request in reviews
            .iter_mut()
            .chain(closed.iter_mut().map(|r| &mut r.request))
        {
            changed |= request.observe(event, &agent)?;
        }
        Ok(changed)
    })
}

async fn acknowledge_closed(client: &Client, ledger: &mut Ledger) -> Result<()> {
    for index in 0..ledger.record().closed_reviews.len() {
        let closed = &ledger.record().closed_reviews[index];
        if closed.acknowledged {
            continue;
        }
        let ids = closed.request.answers().map(|a| a.id.clone()).collect();
        // The durable closed record is proof of provider resolution or asker
        // cancellation, not a claim that every retained answer was applied.
        queue(client, ledger, ids).await?;
        ledger.update_reviews(|_, closed| {
            closed[index].acknowledged = true;
            Ok(true)
        })?;
    }
    Ok(())
}

/// At a controller's start, settle what the last one left asked. A question
/// never answered, with every route known, is closed as cancelled: its
/// Codex request ended with the old provider process, and nothing was ever
/// written for it. Every other one stays, with its known routes cancelled,
/// and the start refuses, saying why: a written response may have reached
/// Codex and is never sent again, and a question with no route may have
/// been published with its reply lost.
pub(super) async fn recover(client: &Client, ledger: &mut Ledger) -> Result<()> {
    acknowledge_closed(client, ledger).await?;
    let never: Vec<Value> = ledger
        .record()
        .reviews
        .iter()
        .filter(|r| r.at_restart() == AtRestart::NeverAnswered)
        .map(|r| r.id.clone())
        .collect();
    for id in never {
        close_archived(client, ledger, &id, Outcome::Cancelled).await?;
    }
    if !ledger.record().reviews.is_empty() {
        cancel_pending(client, ledger).await?;
        let agent = &ledger.record().binding.agent;
        let refusals: Vec<String> = ledger
            .record()
            .reviews
            .iter()
            .map(|r| refusal(r, agent))
            .collect();
        bail!("{}", refusals.join(" "));
    }
    Ok(())
}

/// Why a retained question keeps its controller from starting.
fn refusal(request: &Pending, agent: &str) -> String {
    match request.at_restart() {
        AtRestart::ResponseUncertain => format!(
            "Codex request {} has a response that was written before its send and may have reached Codex; it is never sent again. Look at the Codex conversation, then run `agentdocker codex-input-resolve --agent {agent}` to review and close it.",
            request.id
        ),
        AtRestart::PublicationUncertain => format!(
            "Codex request {}'s question publication is uncertain: the daemon may hold a question this controller never learned the ID of, so a reply to it could not be told from new input. The session stays stopped; `agentdocker codex-input-resolve --agent {agent}` shows it but cannot resolve it.",
            request.id
        ),
        AtRestart::NeverAnswered => format!("Codex request {} is still open.", request.id),
    }
}

/// Close `id` as `outcome` for a controller that is not the one that asked
/// it: its routes are cancelled, the queue is read once, and in one save it
/// moves to the closed questions with every reply it owns kept as archived,
/// whatever closure the old controller last saw; only then are those
/// replies acknowledged, by their exact IDs.
pub(super) async fn close_archived(
    client: &Client,
    ledger: &mut Ledger,
    id: &Value,
    outcome: Outcome,
) -> Result<()> {
    let request = ledger
        .record()
        .reviews
        .iter()
        .find(|r| &r.id == id)
        .context("provider request disappeared")?
        .clone();
    cancel_routes(client, ledger, &request).await?;
    let messages = queue(client, ledger, Vec::new()).await?;
    acknowledge_closed(client, ledger).await?;
    let agent = ledger.record().binding.agent.clone();
    ledger.update_reviews(|reviews, closed| {
        for other in reviews.iter_mut().filter(|r| &r.id != id) {
            other.capture(&messages, &agent, false)?;
        }
        for completed in closed.iter_mut() {
            if completed.request.capture(&messages, &agent, true)? {
                completed.acknowledged = false;
            }
        }
        if closed.len() == review::RETAINED {
            ensure!(
                closed.front().is_some_and(|r| r.acknowledged),
                "earlier provider question receipts are still unacknowledged"
            );
            closed.pop_front();
        }
        let position = reviews
            .iter()
            .position(|r| &r.id == id)
            .context("provider request disappeared")?;
        let mut request = reviews.remove(position);
        request.capture(&messages, &agent, true)?;
        closed.push_back(Closed {
            request,
            outcome,
            acknowledged: false,
        });
        Ok(true)
    })?;
    acknowledge_closed(client, ledger).await
}

pub(super) async fn acknowledge_archived(client: &Client, ledger: &mut Ledger) -> Result<()> {
    acknowledge_closed(client, ledger).await
}

// Closing a route and sending an answer serialize under the daemon's state
// lock. The queue read after all cancellations includes every answer accepted
// before cancellation, including a race with the Questions UI.
async fn cancel_routes(client: &Client, ledger: &Ledger, request: &Pending) -> Result<()> {
    for question in &request.questions {
        if let Some(message) = &question.message {
            ensure!(
                matches!(
                    call(
                        client,
                        Request::CancelQuestion {
                            agent: ledger.record().binding.agent.clone(),
                            message: message.clone(),
                        }
                    )
                    .await?,
                    Response::Ok
                ),
                "daemon did not close the provider question"
            );
        }
    }
    Ok(())
}

async fn close(client: &Client, ledger: &mut Ledger, id: &Value, outcome: Outcome) -> Result<()> {
    let request = ledger
        .record()
        .reviews
        .iter()
        .find(|r| &r.id == id)
        .context("provider request disappeared")?
        .clone();
    cancel_routes(client, ledger, &request).await?;
    let messages = queue(client, ledger, Vec::new()).await?;
    capture(ledger, &messages)?;
    acknowledge_closed(client, ledger).await?;
    ledger.update_reviews(|reviews, closed| {
        if closed.len() == review::RETAINED {
            ensure!(
                closed.front().is_some_and(|r| r.acknowledged),
                "earlier provider question receipts are still unacknowledged"
            );
            closed.pop_front();
        }
        let position = reviews
            .iter()
            .position(|r| &r.id == id)
            .context("provider request disappeared")?;
        let request = reviews.remove(position);
        closed.push_back(Closed {
            request,
            outcome,
            acknowledged: false,
        });
        Ok(true)
    })?;
    acknowledge_closed(client, ledger).await
}

pub(super) async fn resolved(client: &Client, ledger: &mut Ledger, params: &Value) -> Result<()> {
    let Some(request) =
        ledger.record().reviews.iter().find(|r| {
            r.id == params["requestId"] && params["threadId"].as_str() == Some(&r.thread)
        })
    else {
        return Ok(());
    };
    let id = request.id.clone();
    let outcome = if request.response.is_some() {
        Outcome::Resolved
    } else {
        Outcome::Cancelled
    };
    close(client, ledger, &id, outcome).await?;
    eprintln!(
        "Codex question {}.",
        if outcome == Outcome::Resolved {
            "resolved"
        } else {
            "cancelled before a response was sent"
        }
    );
    Ok(())
}

pub(super) async fn turn_ended(client: &Client, ledger: &mut Ledger) -> Result<()> {
    for request in ledger.record().reviews.clone() {
        if request.response.is_some() {
            cancel_routes(client, ledger, &request).await?;
            bail!(
                "Codex ended the turn without confirming its question response; it is never sent again. Look at the Codex conversation, then run `agentdocker codex-input-resolve --agent {}` to review and close it.",
                ledger.record().binding.agent
            );
        }
        close(client, ledger, &request.id, Outcome::Cancelled).await?;
    }
    Ok(())
}

pub(super) async fn cancel_pending(client: &Client, ledger: &Ledger) -> Result<()> {
    for request in &ledger.record().reviews {
        cancel_routes(client, ledger, request).await?;
    }
    Ok(())
}

pub(super) async fn poll(
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<Vec<Envelope>> {
    let messages = queue(client, ledger, Vec::new()).await?;
    capture(ledger, &messages)?;
    acknowledge_closed(client, ledger).await?;
    for index in 0..ledger.record().reviews.len() {
        if let Some(response) = ledger.record().reviews[index].reply(chrono::Utc::now())? {
            ledger.update_reviews(|reviews, _| {
                reviews[index].response = Some(response.clone());
                Ok(true)
            })?;
            provider.send(&response).await?;
        }
    }
    let agent = &ledger.record().binding.agent;
    Ok(messages
        .into_iter()
        .filter(|message| {
            !ledger
                .record()
                .reviews
                .iter()
                .chain(ledger.record().closed_reviews.iter().map(|r| &r.request))
                .any(|r| r.owns(message, agent))
        })
        .collect())
}

#[cfg(all(test, unix))]
pub(super) mod tests {
    use super::*;
    use crate::codex_input::ledger::{Binding, Receipt};
    use agentdocker_core::{Destination, MessageId};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    pub(in crate::codex_input) fn fixture() -> (tempfile::TempDir, Ledger, Envelope) {
        let home = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(
            home.path(),
            Binding {
                agent: "owner".into(),
                socket: home.path().join("sock"),
                cwd: home.path().into(),
                provider_home: home.path().into(),
            },
        )
        .unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let message = Envelope::new(
            "peer",
            Destination::Agent("owner".into()),
            "chat",
            json!({"text":"do work"}),
            None,
            chrono::Utc::now(),
        );
        let input = ledger.prepare(&message).unwrap();
        ledger
            .accept(
                &input,
                Receipt {
                    thread: "thread".into(),
                    turn: "turn".into(),
                    item: "item".into(),
                },
            )
            .unwrap();
        ledger.acknowledge(message.id.as_str()).unwrap();
        let event = json!({"id":17,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned"}});
        let mut pending =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        pending.questions[0].message = Some("question".to_owned().into());
        let answer = Envelope::new(
            "human",
            Destination::Agent("owner".into()),
            "answer",
            json!({"text":"Allow"}),
            Some("question".to_owned().into()),
            chrono::Utc::now(),
        );
        pending
            .observe(
                &agentdocker_core::EventKind::QuestionClosed {
                    question: "question".to_owned().into(),
                    answer: Some(answer.id.clone()),
                },
                "owner",
            )
            .unwrap();
        pending
            .capture(std::slice::from_ref(&answer), "owner", false)
            .unwrap();
        pending.response = pending.reply(chrono::Utc::now()).unwrap();
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        (home, ledger, answer)
    }

    async fn one_request(listener: tokio::net::UnixListener, response: Response) -> Request {
        answer_one(&listener, response).await
    }

    async fn answer_one(listener: &tokio::net::UnixListener, response: Response) -> Request {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request = serde_json::from_str(&line).unwrap();
        reader
            .get_mut()
            .write_all((serde_json::to_string(&response).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
        request
    }

    /// A daemon that answers exactly `responses`, in order, and then is
    /// gone, so any request beyond them fails.
    pub(in crate::codex_input) fn script(
        socket: &std::path::Path,
        responses: Vec<Response>,
    ) -> tokio::task::JoinHandle<Vec<Request>> {
        let _ = std::fs::remove_file(socket);
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            let mut asked = Vec::new();
            for response in responses {
                asked.push(answer_one(&listener, response).await);
            }
            asked
        })
    }

    /// What a scripted daemon was asked, once it has answered everything.
    pub(in crate::codex_input) async fn asked(
        serving: tokio::task::JoinHandle<Vec<Request>>,
    ) -> Vec<Request> {
        tokio::time::timeout(std::time::Duration::from_secs(5), serving)
            .await
            .expect("every scripted daemon answer was asked for")
            .unwrap()
    }

    pub(in crate::codex_input) fn refused(message: &str) -> Response {
        Response::Error {
            code: agentdocker_core::ErrorCode::Internal,
            message: message.into(),
            details: None,
        }
    }

    pub(in crate::codex_input) fn cancelled(request: &Request, route: &str) -> bool {
        matches!(request, Request::CancelQuestion { agent, message } if agent == "owner" && message.as_str() == route)
    }

    pub(in crate::codex_input) fn read(request: &Request, ids: &[&MessageId]) -> bool {
        matches!(request, Request::ProviderInbox { agent, acknowledge, .. }
            if agent == "owner" && acknowledge.iter().collect::<Vec<_>>() == ids)
    }

    /// A command approval routed to `route` that was never answered.
    pub(in crate::codex_input) fn never_answered(ledger: &mut Ledger, id: u64, route: &str) {
        let event = json!({"id":id,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo other","cwd":"/owned"}});
        let mut pending =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        pending.questions[0].message = Some(route.to_owned().into());
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
    }

    pub(in crate::codex_input) fn reply(from: &str, route: &str, text: &str) -> Envelope {
        Envelope::new(
            from,
            Destination::Agent("owner".into()),
            "answer",
            json!({ "text": text }),
            Some(route.to_owned().into()),
            chrono::Utc::now(),
        )
    }

    /// The fixture's ledger with its retained question taken away.
    fn bound() -> (tempfile::TempDir, Ledger) {
        let (home, mut ledger, _) = fixture();
        ledger
            .update_reviews(|reviews, _| {
                reviews.clear();
                Ok(true)
            })
            .unwrap();
        (home, ledger)
    }

    pub(in crate::codex_input) fn client(ledger: &Ledger) -> Client {
        Client::new(Some(ledger.record().binding.socket.clone())).with_start_timeout(None)
    }

    #[tokio::test]
    async fn a_never_answered_question_closes_at_restart_with_its_late_answer_archived() {
        let (home, mut ledger) = bound();
        never_answered(&mut ledger, 18, "open-question");
        // The old controller never saw the question close: its late answer
        // waits in the queue beside unrelated input.
        let late = reply("human", "open-question", "Allow");
        let unrelated = reply("peer", "elsewhere", "next work");
        let serving = script(
            &ledger.record().binding.socket,
            vec![
                Response::Ok,
                Response::Messages {
                    messages: vec![late.clone(), unrelated.clone()],
                },
                Response::Messages {
                    messages: vec![unrelated.clone()],
                },
            ],
        );
        recover(&client(&ledger), &mut ledger).await.unwrap();
        let asked = asked(serving).await;
        assert!(cancelled(&asked[0], "open-question"));
        assert!(read(&asked[1], &[]));
        assert!(read(&asked[2], &[&late.id]));
        let record = ledger.record();
        assert!(record.reviews.is_empty());
        let closed = &record.closed_reviews[0];
        assert_eq!(closed.outcome, Outcome::Cancelled);
        assert!(closed.acknowledged && closed.request.response.is_none());
        assert_eq!(
            closed.request.answers().map(|a| &a.id).collect::<Vec<_>>(),
            [&late.id]
        );
        // What `poll` holds back as a closed question's reply, never input.
        assert!(closed.request.owns(&late, "owner"));
        assert!(!closed.request.owns(&unrelated, "owner"));
        let binding = record.binding.clone();
        drop(ledger);
        // Nothing is left to settle: the daemon is gone and a start passes.
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        recover(&client(&ledger), &mut ledger).await.unwrap();
    }

    #[tokio::test]
    async fn a_mixed_restart_archives_the_never_answered_question_then_refuses_for_the_response() {
        let (_home, mut ledger, answer) = fixture();
        never_answered(&mut ledger, 18, "open-question");
        let late = reply("human", "open-question", "Deny");
        let serving = script(
            &ledger.record().binding.socket,
            vec![
                Response::Ok,
                Response::Messages {
                    messages: vec![answer.clone(), late.clone()],
                },
                Response::Messages {
                    messages: vec![answer.clone()],
                },
                Response::Ok,
            ],
        );
        let error = recover(&client(&ledger), &mut ledger).await.unwrap_err();
        let asked = asked(serving).await;
        assert!(cancelled(&asked[0], "open-question"));
        assert!(read(&asked[1], &[]));
        assert!(read(&asked[2], &[&late.id]));
        assert!(cancelled(&asked[3], "question"));
        assert!(
            error
                .to_string()
                .contains("Codex request 17 has a response")
                && error.to_string().contains("never sent again")
                && !error.to_string().contains("18"),
            "{error}"
        );
        let record = ledger.record();
        assert_eq!(record.reviews.len(), 1);
        assert_eq!(record.reviews[0].id, json!(17));
        assert_eq!(
            record.reviews[0]
                .answers()
                .map(|a| &a.id)
                .collect::<Vec<_>>(),
            [&answer.id]
        );
        assert_eq!(record.closed_reviews.len(), 1);
        assert!(record.closed_reviews[0].acknowledged);
        assert_eq!(record.closed_reviews[0].request.id, json!(18));
    }

    #[tokio::test]
    async fn an_uncertain_publication_keeps_the_session_stopped_and_settles_nothing() {
        let (_home, mut ledger) = bound();
        // The first question was posted and routed; the second's PostQuestion
        // may have been accepted with its reply lost.
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
        let before = std::fs::read(ledger.file_path()).unwrap();
        // Only the known route is cancelled: the queue is not read, nothing
        // is acknowledged, and no question the daemon holds is looked for or
        // bound to the missing route.
        let serving = script(&ledger.record().binding.socket, vec![Response::Ok]);
        let error = recover(&client(&ledger), &mut ledger).await.unwrap_err();
        let asked = asked(serving).await;
        assert_eq!(asked.len(), 1);
        assert!(cancelled(&asked[0], "first-route"));
        assert!(
            error
                .to_string()
                .contains("\"ask\"'s question publication is uncertain")
                && error.to_string().contains(
                    "`agentdocker codex-input-resolve --agent owner` shows it but cannot resolve it"
                ),
            "{error}"
        );
        assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
        assert!(ledger.record().reviews[0].questions[1].message.is_none());
        assert!(ledger.record().closed_reviews.is_empty());
    }

    #[tokio::test]
    async fn a_failed_restart_close_leaves_the_question_retained_and_acknowledges_nothing() {
        // The cancellation, then the queue read, refused.
        for script_responses in [vec![refused("cancel")], vec![Response::Ok, refused("read")]] {
            let (_home, mut ledger) = bound();
            never_answered(&mut ledger, 18, "open-question");
            let before = std::fs::read(ledger.file_path()).unwrap();
            let count = script_responses.len();
            let serving = script(&ledger.record().binding.socket, script_responses);
            assert!(recover(&client(&ledger), &mut ledger).await.is_err());
            assert_eq!(asked(serving).await.len(), count);
            assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
            assert_eq!(ledger.record().reviews.len(), 1);
        }
        // The one save that would close it is refused (more later replies
        // than a question keeps): nothing moves, and nothing is acknowledged.
        let (_home, mut ledger) = bound();
        never_answered(&mut ledger, 18, "open-question");
        let before = std::fs::read(ledger.file_path()).unwrap();
        let flood = (0..=review::MAX_QUESTIONS)
            .map(|n| reply("human", "open-question", &format!("reply {n}")))
            .collect();
        let serving = script(
            &ledger.record().binding.socket,
            vec![Response::Ok, Response::Messages { messages: flood }],
        );
        let error = recover(&client(&ledger), &mut ledger).await.unwrap_err();
        assert!(
            error.to_string().contains("too many later responses"),
            "{error}"
        );
        assert_eq!(asked(serving).await.len(), 2);
        assert_eq!(std::fs::read(ledger.file_path()).unwrap(), before);
        assert_eq!(ledger.record().reviews.len(), 1);
    }

    #[tokio::test]
    async fn a_failed_restart_acknowledgement_is_finished_by_the_next_start() {
        let (home, mut ledger) = bound();
        never_answered(&mut ledger, 18, "open-question");
        let late = reply("human", "open-question", "Allow");
        let serving = script(
            &ledger.record().binding.socket,
            vec![
                Response::Ok,
                Response::Messages {
                    messages: vec![late.clone()],
                },
                refused("acknowledge"),
            ],
        );
        assert!(recover(&client(&ledger), &mut ledger).await.is_err());
        assert_eq!(asked(serving).await.len(), 3);
        let binding = ledger.record().binding.clone();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        assert!(ledger.record().reviews.is_empty());
        assert!(!ledger.record().closed_reviews[0].acknowledged);
        let serving = script(
            &ledger.record().binding.socket,
            vec![Response::Messages {
                messages: Vec::new(),
            }],
        );
        recover(&client(&ledger), &mut ledger).await.unwrap();
        let asked = asked(serving).await;
        assert!(read(&asked[0], &[&late.id]));
        assert!(ledger.record().closed_reviews[0].acknowledged);
    }

    #[tokio::test]
    async fn closed_answer_recovery_acknowledges_only_its_message_and_never_repeats_the_response() {
        let (home, mut ledger, answer) = fixture();
        ledger
            .update_reviews(|reviews, closed| {
                closed.push_back(Closed {
                    request: reviews.remove(0),
                    outcome: Outcome::Resolved,
                    acknowledged: false,
                });
                Ok(true)
            })
            .unwrap();
        let binding = ledger.record().binding.clone();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        let listener = tokio::net::UnixListener::bind(&binding.socket).unwrap();
        let unrelated = Envelope::new(
            "peer",
            Destination::Agent("owner".into()),
            "chat",
            json!({"text":"next work"}),
            None,
            chrono::Utc::now(),
        );
        let serving = tokio::spawn(one_request(
            listener,
            Response::Messages {
                messages: vec![unrelated],
            },
        ));
        let client = Client::new(Some(binding.socket.clone())).with_start_timeout(None);
        recover(&client, &mut ledger).await.unwrap();
        let request = serving.await.unwrap();
        assert!(
            matches!(request,Request::ProviderInbox { agent,acknowledge, .. } if agent=="owner" && acknowledge==vec![answer.id])
        );
        assert!(ledger.record().closed_reviews[0].acknowledged);
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        // The listener is gone: an unnecessary read/ack/repost would fail.
        recover(&client, &mut ledger).await.unwrap();
        assert_eq!(ledger.record().closed_reviews.len(), 1);
        assert_eq!(
            ledger.record().closed_reviews[0]
                .request
                .response
                .as_ref()
                .unwrap()["result"]["decision"],
            "accept"
        );
    }

    #[tokio::test]
    async fn uncertain_response_restart_closes_its_question_but_preserves_the_queued_answer() {
        let (home, ledger, answer) = fixture();
        let binding = ledger.record().binding.clone();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        let listener = tokio::net::UnixListener::bind(&binding.socket).unwrap();
        let serving = tokio::spawn(one_request(listener, Response::Ok));
        let client = Client::new(Some(binding.socket)).with_start_timeout(None);
        let error = recover(&client, &mut ledger).await.unwrap_err();
        assert!(
            error.to_string().contains("never sent again")
                && error
                    .to_string()
                    .contains("agentdocker codex-input-resolve --agent owner"),
            "{error}"
        );
        assert!(
            matches!(serving.await.unwrap(),Request::CancelQuestion { agent,message } if agent=="owner" && message.as_str()=="question")
        );
        assert_eq!(ledger.record().reviews.len(), 1);
        assert!(ledger.record().closed_reviews.is_empty());
        assert_eq!(
            ledger.record().reviews[0].answers().next().unwrap().id,
            answer.id
        );
        assert!(
            ledger.record().reviews[0]
                .reply(chrono::Utc::now())
                .unwrap()
                .is_none()
        );
    }
}
