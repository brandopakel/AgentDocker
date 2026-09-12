//! Human answers remain in the shared queue until their provider request has
//! resolved. Closed/cancelled responses are archived, never sent as new turns.
use super::{
    Client, Ledger, Provider, call, queue,
    review::{self, Closed, Outcome, Pending},
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
                timeout_secs: 300,
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

pub(super) async fn recover(client: &Client, ledger: &mut Ledger) -> Result<()> {
    if !ledger.record().reviews.is_empty() {
        cancel_pending(client, ledger).await?;
    }
    ensure!(
        ledger.record().reviews.is_empty(),
        "an earlier provider question has no reconciled resolution; retained answers must be reviewed before queued input resumes"
    );
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
                "Codex ended the turn without confirming its question response; retained answers need recovery"
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
mod tests {
    use super::*;
    use crate::codex_input::ledger::{Binding, Receipt};
    use agentdocker_core::Destination;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn fixture() -> (tempfile::TempDir, Ledger, Envelope) {
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
            matches!(request,Request::ProviderInbox { agent,acknowledge } if agent=="owner" && acknowledge==vec![answer.id])
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
        assert!(error.to_string().contains("no reconciled resolution"));
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
