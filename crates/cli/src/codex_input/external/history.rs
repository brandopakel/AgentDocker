//! Read-only item pages, with bounded full-turn pages for legacy Codex stores.
use super::super::{
    recovery,
    transport::{Provider, items_list_unsupported},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

const MAX_PAGES: usize = 100;
const MAX_ITEMS: usize = 5000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct History {
    thread: String,
    turns: bool,
    cursor: Option<String>,
    cursors: HashSet<String>,
    pages: usize,
    items: usize,
    bytes: usize,
    done: bool,
    item_limit: usize,
}

impl History {
    pub fn new(thread: &str, item_limit: usize) -> Self {
        Self {
            thread: thread.into(),
            turns: false,
            cursor: None,
            cursors: HashSet::new(),
            pages: 0,
            items: 0,
            bytes: 0,
            done: false,
            item_limit,
        }
    }

    pub async fn next(&mut self, provider: &mut Provider) -> Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }
        ensure!(
            self.pages < MAX_PAGES,
            "native history exceeds the page limit"
        );
        let value = if self.turns {
            self.turn_page(provider).await?
        } else {
            match provider
                .request(
                    "thread/items/list",
                    json!({"threadId":self.thread,
                "limit":self.item_limit,"sortDirection":"desc","cursor":self.cursor}),
                )
                .await
            {
                Ok(value) => value,
                Err(error) if self.pages == 0 && items_list_unsupported(&error) => {
                    // A method can be unavailable for the active thread store.
                    // Switch before observing any page; never combine cursors
                    // or partially read histories from different APIs.
                    self.turns = true;
                    self.turn_page(provider).await?
                }
                Err(error) => return Err(error),
            }
        };
        self.accept(value).map(Some)
    }

    async fn turn_page(&self, provider: &mut Provider) -> Result<Value> {
        provider
            .request(
                "thread/turns/list",
                json!({"threadId":self.thread,
            "limit":1,"itemsView":"full","sortDirection":"desc","cursor":self.cursor}),
            )
            .await
    }

    fn accept(&mut self, value: Value) -> Result<Vec<Value>> {
        self.bytes = self.bytes.saturating_add(serde_json::to_vec(&value)?.len());
        ensure!(
            self.bytes <= MAX_BYTES,
            "native history exceeds the byte limit"
        );
        let (data, next) = recovery::page(&value, &mut self.cursors)?;
        let entries = if self.turns {
            full_turn_items(data)?
        } else {
            ensure!(
                data.len() <= self.item_limit,
                "native history exceeded the item page limit"
            );
            data.clone()
        };
        self.items = self.items.saturating_add(entries.len());
        ensure!(
            self.items <= MAX_ITEMS,
            "native history exceeds the item limit"
        );
        self.pages += 1;
        self.cursor = next;
        self.done = self.cursor.is_none();
        Ok(entries)
    }
}

fn full_turn_items(turns: &[Value]) -> Result<Vec<Value>> {
    ensure!(
        turns.len() <= 1,
        "native history exceeded the turn page limit"
    );
    let Some(turn) = turns.first() else {
        return Ok(Vec::new());
    };
    let id = turn["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("native history turn has no ID")?;
    let items = turn["items"]
        .as_array()
        .context("native history turn lacks full items")?;
    ensure!(
        items.len() <= MAX_ITEMS,
        "native history turn exceeds the item limit"
    );
    // Full turns expose items in conversation order; the caller expects newest
    // first, exactly like thread/items/list sortDirection=desc.
    Ok(items
        .iter()
        .rev()
        .map(|item| json!({"turnId":id,"item":item}))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn only_an_explicit_store_refusal_switches_to_read_only_turn_pages() {
        use std::os::unix::fs::PermissionsExt;
        for code in [-32601, -32603] {
            let root = tempfile::tempdir().unwrap();
            let program = root.path().join("provider");
            let requests = root.path().join("requests.jsonl");
            let script = format!(
                r#"#!/usr/bin/env python3
import json,sys
for line in sys.stdin:
    request=json.loads(line)
    with open({requests},'a') as log: log.write(json.dumps(request)+'\n')
    if request['method']=='thread/items/list':
        reply={{'error':{{'code':{code},'message':'thread/items/list is not supported yet'}}}}
    elif request['method']=='thread/turns/list':
        assert request['params']['limit']==1 and request['params']['itemsView']=='full'
        assert request['params']['sortDirection']=='desc'
        cursor=request['params']['cursor']
        if cursor is None:
            reply={{'result':{{'data':[{{'id':'recent','items':[{{'id':'new','type':'userMessage','content':[{{'type':'text','text':'exact'}}]}}]}}],'nextCursor':'older'}}}}
        else:
            assert cursor=='older'
            reply={{'result':{{'data':[{{'id':'old','items':[{{'id':'anchor'}}]}}],'nextCursor':None}}}}
    else: raise AssertionError('unexpected or mutating request')
    print(json.dumps(dict(reply,id=request['id'])),flush=True)
"#,
                requests = serde_json::to_string(&requests.to_str().unwrap()).unwrap()
            );
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut provider = Provider::start(&program, &[], root.path()).unwrap();
            let mut history = History::new("thread", 50);
            let first = history.next(&mut provider).await;
            if code == -32601 {
                let first = first.unwrap().unwrap();
                let receipt = recovery::receipt(
                    "thread",
                    first[0]["turnId"].as_str().unwrap(),
                    &first[0]["item"],
                    "exact",
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    (
                        receipt.thread.as_str(),
                        receipt.turn.as_str(),
                        receipt.item.as_str()
                    ),
                    ("thread", "recent", "new")
                );
                assert_eq!(
                    history.next(&mut provider).await.unwrap().unwrap()[0]["item"]["id"],
                    "anchor"
                );
                assert!(history.next(&mut provider).await.unwrap().is_none());
            } else {
                assert!(first.is_err());
            }
            provider.shutdown().await.unwrap();
            let seen = std::fs::read_to_string(requests).unwrap();
            let methods: Vec<_> = seen
                .lines()
                .map(|line| {
                    serde_json::from_str::<Value>(line).unwrap()["method"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect();
            assert_eq!(
                methods,
                if code == -32601 {
                    vec![
                        "thread/items/list",
                        "thread/turns/list",
                        "thread/turns/list",
                    ]
                } else {
                    vec!["thread/items/list"]
                }
            );
        }
    }

    #[test]
    fn legacy_turns_preserve_exact_ids_content_and_reverse_item_order() {
        let input = json!({"id":"user","type":"userMessage","content":[{"type":"text","text":"exact input"}]});
        let answer = json!({"id":"answer","type":"agentMessage","text":"done"});
        let mut history = History::new("thread", 50);
        history.turns = true;
        let page = history
            .accept(json!({"data":[{"id":"turn","items":[input,answer]}],"nextCursor":"older"}))
            .unwrap();
        assert_eq!(
            page,
            vec![
                json!({"turnId":"turn","item":answer}),
                json!({"turnId":"turn","item":input})
            ]
        );
        assert_eq!(history.cursor.as_deref(), Some("older"));
        assert!(!history.done);
        assert!(
            history
                .accept(json!({"data":[],"nextCursor":null}))
                .unwrap()
                .is_empty()
        );
        assert!(history.done);
    }

    #[test]
    fn legacy_history_refuses_partial_turns_cycles_and_unbounded_content() {
        for turns in [
            json!([{"id":"turn"}]),
            json!([{"items":[]}]),
            json!([{"id":"a","items":[]},{"id":"b","items":[]}]),
        ] {
            assert!(full_turn_items(turns.as_array().unwrap()).is_err());
        }
        let mut history = History::new("thread", 50);
        history.turns = true;
        history
            .accept(json!({"data":[],"nextCursor":"same"}))
            .unwrap();
        assert!(
            history
                .accept(json!({"data":[],"nextCursor":"same"}))
                .is_err()
        );
        let mut history = History::new("thread", 50);
        history.bytes = MAX_BYTES;
        assert!(history.accept(json!({"data":[]})).is_err());
        let mut history = History::new("thread", 50);
        history.turns = true;
        history.items = MAX_ITEMS;
        assert!(
            history
                .accept(json!({"data":[{"id":"turn","items":[{"id":"one-more"}]}]}))
                .is_err()
        );
        let mut history = History::new("thread", 1);
        assert!(history.accept(json!({"data":[{},{}]})).is_err());
    }
}
