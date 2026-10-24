//! A herdr server as far as the daemon talks to one, for the status
//! bridge's tests and the screen judge's: `pane.list` answers with the
//! panes it is given, `events.subscribe` acknowledges and then pushes what
//! the test sends, and `pane.read` answers with the screen it is given for
//! a pane, in the shape a live herdr 0.9.3 answered, or as the test says.
//! It records every method it is asked, each subscription's request, each
//! read's parameters, and each subscription the client closed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// How `pane.read` is answered.
#[derive(Clone, Debug)]
pub(crate) enum Reads {
    /// The pane's screen, or herdr's `pane_not_found` for a pane it does
    /// not have.
    Answer,
    /// Never: the connection is held open unanswered.
    Hang,
    /// herdr's refusal.
    Refuse,
    /// This reply, whatever was asked.
    Reply(Value),
    /// The pane's screen, with the panes changed to these before it is
    /// sent: the session moved while the pane was read.
    AnswerThenMove(Value),
}

pub(crate) struct Fake {
    panes: Arc<Mutex<Value>>,
    screens: Arc<Mutex<BTreeMap<String, String>>>,
    reads: Arc<Mutex<Reads>>,
    methods: Arc<Mutex<Vec<String>>>,
    read_params: Arc<Mutex<Vec<Value>>>,
    subscriptions: Arc<Mutex<Vec<Value>>>,
    pushers: Arc<Mutex<Vec<mpsc::UnboundedSender<String>>>>,
    pub(crate) closed: Arc<AtomicUsize>,
    _server: tokio::task::JoinHandle<()>,
}

impl Fake {
    pub(crate) fn start(path: &Path, panes: Value) -> Self {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        let fake = Self {
            panes: Arc::new(Mutex::new(panes)),
            screens: Arc::default(),
            reads: Arc::new(Mutex::new(Reads::Answer)),
            methods: Arc::default(),
            read_params: Arc::default(),
            subscriptions: Arc::default(),
            pushers: Arc::default(),
            closed: Arc::default(),
            _server: tokio::spawn(async {}),
        };
        let shared = fake.shared();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(shared.clone().serve(stream));
            }
        });
        Self {
            _server: server,
            ..fake
        }
    }

    fn shared(&self) -> Shared {
        Shared {
            panes: self.panes.clone(),
            screens: self.screens.clone(),
            reads: self.reads.clone(),
            methods: self.methods.clone(),
            read_params: self.read_params.clone(),
            subscriptions: self.subscriptions.clone(),
            pushers: self.pushers.clone(),
            closed: self.closed.clone(),
        }
    }

    pub(crate) fn set_panes(&self, panes: Value) {
        *self.panes.lock().unwrap() = panes;
    }

    /// What `pane` shows.
    pub(crate) fn show(&self, pane: &str, text: &str) {
        self.screens
            .lock()
            .unwrap()
            .insert(pane.to_owned(), text.to_owned());
    }

    pub(crate) fn reads(&self, reads: Reads) {
        *self.reads.lock().unwrap() = reads;
    }

    /// One line to every open subscription.
    pub(crate) fn push(&self, line: Value) {
        self.pushers
            .lock()
            .unwrap()
            .retain(|push| push.send(line.to_string()).is_ok());
    }

    /// herdr ends every subscription.
    pub(crate) fn end_subscriptions(&self) {
        self.pushers.lock().unwrap().clear();
    }

    pub(crate) fn subscribed(&self) -> Vec<Value> {
        self.subscriptions.lock().unwrap().clone()
    }

    pub(crate) fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }

    /// The parameters of each `pane.read`.
    pub(crate) fn read_params(&self) -> Vec<Value> {
        self.read_params.lock().unwrap().clone()
    }

    /// The panes `pane.read` was asked for.
    pub(crate) fn panes_read(&self) -> Vec<String> {
        self.read_params()
            .iter()
            .map(|params| params["pane_id"].as_str().unwrap_or("").to_owned())
            .collect()
    }
}

/// What every connection shares with the test.
#[derive(Clone)]
struct Shared {
    panes: Arc<Mutex<Value>>,
    screens: Arc<Mutex<BTreeMap<String, String>>>,
    reads: Arc<Mutex<Reads>>,
    methods: Arc<Mutex<Vec<String>>>,
    read_params: Arc<Mutex<Vec<Value>>>,
    subscriptions: Arc<Mutex<Vec<Value>>>,
    pushers: Arc<Mutex<Vec<mpsc::UnboundedSender<String>>>>,
    closed: Arc<AtomicUsize>,
}

impl Shared {
    async fn serve(self, stream: tokio::net::UnixStream) {
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let mut line = String::new();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or("").to_owned();
        self.methods.lock().unwrap().push(method.clone());
        let reply = match method.as_str() {
            "pane.list" => {
                let panes = self.panes.lock().unwrap().clone();
                json!({"id": id, "result": {"type": "pane_list", "panes": panes}})
            }
            "pane.read" => {
                let params = request["params"].clone();
                self.read_params.lock().unwrap().push(params.clone());
                let reads = self.reads.lock().unwrap().clone();
                match reads {
                    Reads::Hang => {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        return;
                    }
                    Reads::Refuse => {
                        json!({"id": id, "error": {"code": "internal", "message": "busy"}})
                    }
                    Reads::Reply(reply) => reply,
                    Reads::Answer | Reads::AnswerThenMove(_) => {
                        let pane = params["pane_id"].as_str().unwrap_or("").to_owned();
                        let screen = self.screens.lock().unwrap().get(&pane).cloned();
                        if let Reads::AnswerThenMove(panes) = reads {
                            *self.panes.lock().unwrap() = panes;
                        }
                        match screen {
                            Some(text) => {
                                json!({"id": id, "result": {"type": "pane_read", "read": {
                                    "pane_id": pane, "workspace_id": "w1", "tab_id": "w1:t1",
                                    "source": "visible", "format": "text", "text": text,
                                    "revision": 0, "truncated": false,
                                }}})
                            }
                            None => json!({"id": id, "error": {
                                "code": "pane_not_found", "message": format!("pane {pane} not found"),
                            }}),
                        }
                    }
                }
            }
            "events.subscribe" => {
                self.subscriptions
                    .lock()
                    .unwrap()
                    .push(request["params"]["subscriptions"].clone());
                let ack = json!({"id": id, "result": {"type": "subscription_started"}});
                let _ = write.write_all(format!("{ack}\n").as_bytes()).await;
                let (push, mut pushed) = mpsc::unbounded_channel::<String>();
                self.pushers.lock().unwrap().push(push);
                let mut rest = String::new();
                loop {
                    tokio::select! {
                        next = pushed.recv() => match next {
                            Some(line) => {
                                let _ = write.write_all(format!("{line}\n").as_bytes()).await;
                            }
                            None => return,
                        },
                        read = read.read_line(&mut rest) => {
                            if read.unwrap_or(0) == 0 {
                                self.closed.fetch_add(1, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                }
            }
            _ => json!({"id": id, "error": {"code": "unknown_method", "message": "no"}}),
        };
        let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
    }
}

/// One pane as `pane.list` describes it, holding `session` if any.
pub(crate) fn pane(id: &str, status: &str, session: Option<&str>) -> Value {
    json!({
        "pane_id": id,
        "terminal_id": "term_1",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "focused": false,
        "revision": 0,
        "agent": "gemini",
        "agent_status": status,
        "agent_session": session.map(|value| json!({
            "source": "herdr:gemini", "agent": "gemini", "kind": "id", "value": value
        })),
    })
}
