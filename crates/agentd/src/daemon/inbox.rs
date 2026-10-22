use super::*;

impl State {
    pub(super) fn input_consumer(
        &mut self,
        reference: &str,
        provider: bool,
    ) -> Result<(), Box<Response>> {
        let id = self.resolve(reference)?;
        let agent = self.registry.get(&id).expect("resolved input identity");
        let owned = agentdocker_host::provider_input::is_codex_input(agent);
        if !provider {
            // Somebody else consumes this queue: not an error, there is
            // simply nothing here for a legacy reader to deliver.
            if let Some(owner) = self.input_owner(&id) {
                return Err(Box::new(owner));
            }
            return Ok(());
        }
        if !owned {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "Provider input requires a managed Codex bridge session, or a token for a bound controller.",
            )));
        }
        Ok(())
    }

    /// A non-draining legacy read: refused for a bound queue, since the
    /// daemon cannot tell a person from the session's own reader and the
    /// controller cannot be told about an exposure that lands between its
    /// snapshot and its enqueue; open for a managed bridge session, whose
    /// app view reads this way.
    pub(super) fn readable_inbox(&mut self, reference: &str) -> Result<(), Box<Response>> {
        let id = self.resolve(reference)?;
        if self
            .registry
            .get(&id)
            .is_some_and(|r| r.input_binding.is_some())
            && let Some(owner) = self.input_owner(&id)
        {
            return Err(Box::new(owner));
        }
        Ok(())
    }

    /// A legacy acknowledgement is refused for an owned queue, except for
    /// the messages a legacy reader had been offered before the binding:
    /// that reader finishing its in-flight delivery is exactly what the
    /// binding wants to hear about.
    pub(super) fn legacy_ack_allowed(
        &mut self,
        reference: &str,
        messages: &[MessageId],
    ) -> Result<(), Box<Response>> {
        let id = self.resolve(reference)?;
        let record = self.registry.get(&id).expect("resolved input identity");
        if let Some(binding) = &record.input_binding
            && !messages.is_empty()
            && messages.iter().all(|m| binding.uncertain.contains(m))
        {
            return Ok(());
        }
        self.input_consumer(reference, false)
    }

    /// The legacy read just answered with these messages: remember the
    /// offer for a controller that may bind later.
    pub(super) fn offered_to_legacy(&mut self, reference: &str, response: &Response) {
        if let Response::Messages { messages } = response
            && let Ok(id) = self.resolve(reference)
        {
            let ids: Vec<MessageId> = messages.iter().map(|m| m.id.clone()).collect();
            self.note_legacy_offers(&id, &ids, Utc::now());
        }
    }

    pub(super) fn ack_inbox(&mut self, reference: &str, messages: &[MessageId]) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch_reader(&id);
        // Repeated or fabricated IDs do not establish a new receipt. Retain
        // inbox order and record only the accepted work this call removes.
        let wanted: HashSet<&MessageId> = messages.iter().collect();
        let mut acknowledged = HashSet::new();
        let messages: Vec<MessageId> = self
            .inboxes
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|message| {
                wanted.contains(&message.id) && acknowledged.insert(message.id.clone())
            })
            .map(|message| message.id.clone())
            .collect();
        if messages.is_empty() {
            return Response::Ok;
        }
        let event = Event::new(
            EventKind::InboxAcknowledged {
                agent: id.clone(),
                messages: messages.to_vec(),
            },
            Utc::now(),
        );
        let _ = self.transition(
            "inbox acknowledgement",
            event,
            |store, event| store.ack_inbox(&id, &messages, event),
            |state| {
                if let Some(queue) = state.inboxes.get_mut(&id) {
                    let bytes = state.inbox_bytes.entry(id.clone()).or_default();
                    queue.retain(|message| {
                        if messages.contains(&message.id) {
                            *bytes = bytes.saturating_sub(message_bytes(message));
                            false
                        } else {
                            true
                        }
                    });
                }
            },
        );
        self.write_failure().unwrap_or(Response::Ok)
    }

    pub(super) fn inbox(&mut self, reference: &str, drain: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if drain
            && agentdocker_core::provider_block(
                self.registry.get(&id).expect("resolved"),
                self.registry.all(),
            )
            .is_some()
        {
            // Keep the original Inbox response shape for older clients. Only
            // the explicit DeliveryQueue request opts into InputWaiting.
            return Response::error(
                ErrorCode::Conflict,
                "provider unavailable; queued messages are retained until delivery resumes",
            );
        }
        // A failed liveness write must stop the operation before queue removal.
        self.touch_reader(&id);
        let messages = match self.read_inbox(&id, drain) {
            Ok(messages) => messages,
            Err(error) => return *error,
        };
        Response::Messages { messages }
    }

    /// Snapshot before removal; an explicit destructive read commits its exact
    /// IDs and replay event together. Opening a stream never acknowledges work.
    pub(super) fn read_inbox(
        &mut self,
        id: &AgentId,
        drain: bool,
    ) -> Result<Vec<Envelope>, Box<Response>> {
        if let Some(error) = self.storage_failure() {
            return Err(Box::new(error));
        }
        // An answer held for a waiting ask is not in the queue for anyone,
        // and neither is anything behind it: the queue is delivered in
        // order, and an answer that comes back to it when the ask ends
        // must not find later messages already delivered ahead of it.
        let messages: Vec<Envelope> = self
            .inboxes
            .get(id)
            .map(|queue| {
                queue
                    .iter()
                    .take_while(|m| !self.held_answers.contains_key(&m.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if drain && !messages.is_empty() {
            let ids = messages
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let response = self.ack_inbox(id.as_str(), &ids);
            if !matches!(response, Response::Ok) {
                return Err(Box::new(response));
            }
        }
        Ok(messages)
    }
}
