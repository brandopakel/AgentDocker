//! Roles an agent holds, and the room each role speaks in. The person gives
//! roles, from the project's palette, and only the person; an agent may hold
//! several, and several agents one. A message an agent sends to
//! `role:reviewer` goes to that role's room in its project: one channel every
//! live holder hears, with whoever wrote to it, so one thread carries the
//! role's conversation for its whole life. A room follows who holds the role
//! rather than invitations, and is never closed because its holders left.
//!
//! A role and its room change together: an agent's roles and the members of
//! the rooms they touch are one commit, so a holder that puts a role down is
//! out of its room even when nobody is left to tell. What `agentd` says in a
//! room about it comes after, and a word that cannot be delivered (a full
//! inbox) changes nothing that was decided.
use super::*;
use agentdocker_core::agent::{ROLE_LABEL, check_role, join_roles};
use agentdocker_core::channel::ChannelSubject;

impl State {
    /// The room of `role` in `project`, open or closed, if it was ever made.
    pub(super) fn role_room(&self, project: &ProjectId, role: &str) -> Option<&Channel> {
        self.channels
            .values()
            .filter(|c| &c.project == project && c.role.as_deref() == Some(role))
            .max_by_key(|c| c.opened_at)
    }

    /// Who a room's messages reach: every live agent holding its role in its
    /// project, and the live agents who wrote to it.
    pub(super) fn room_members(&self, room: &Channel) -> Vec<AgentId> {
        let Some(role) = &room.role else {
            return room.members.clone();
        };
        let mut members: Vec<AgentId> = self
            .registry
            .holders(role, Some(&room.project))
            .into_iter()
            .map(|a| a.id.clone())
            .collect();
        members.extend(
            room.members
                .iter()
                .filter(|m| self.registry.get(m).is_some_and(|a| a.status.is_live()))
                .cloned(),
        );
        members.sort();
        members.dedup();
        members
    }

    /// The room of `role` in `project`, made or opened again, its members
    /// brought up to who holds the role now — `joining` among them, before
    /// its record says so — with `guest` (a sender, to hear the answers): the
    /// channel to commit, and its events. With `holders_needed`, a role nobody
    /// holds is `not_found`.
    pub(super) fn stage_role_room(
        &mut self,
        project: &ProjectId,
        role: &str,
        guest: Option<&AgentId>,
        joining: Option<&AgentId>,
        holders_needed: bool,
        now: DateTime<Utc>,
    ) -> Result<(Channel, Vec<EventKind>), Box<Response>> {
        let mut holders: Vec<AgentId> = self
            .registry
            .holders(role, Some(project))
            .into_iter()
            .map(|a| a.id.clone())
            .collect();
        holders.extend(joining.cloned());
        if holders_needed && holders.is_empty() {
            return Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!("nobody holds the role {role} in this project"),
            )));
        }
        let title = self
            .project_roles(project)
            .ok()
            .and_then(|roles| roles.into_iter().find(|r| r.role == role))
            .map_or_else(|| role.to_owned(), |r| r.name);
        let mut events = Vec::new();
        let (mut room, made) = match self.role_room(project, role).cloned() {
            Some(room) => (room, false),
            None => (
                Channel {
                    id: ChannelId::generate(),
                    project: project.clone(),
                    name: None,
                    subject: ChannelSubject::Task { task: title },
                    members: Vec::new(),
                    opened_by: None,
                    opened_at: now,
                    reviews: Vec::new(),
                    closed_at: None,
                    resolution: None,
                    role: Some(role.to_owned()),
                },
                true,
            ),
        };
        let reopened = !made && !room.is_open();
        room.closed_at = None;
        room.resolution = None;
        // Called by its role's address when no other open channel here is.
        if room.name.as_deref() != Some(role)
            && !self.channels.values().any(|c| {
                c.id != room.id
                    && c.is_open()
                    && &c.project == project
                    && c.name.as_deref() == Some(role)
            })
        {
            room.name = Some(role.to_owned());
        }
        // Who wrote to it and still runs. A holder that put the role down was
        // taken out of the members when it did, so it is never one of these.
        let guests = room.members.iter().filter(|m| {
            !holders.contains(m) && self.registry.get(m).is_some_and(|a| a.status.is_live())
        });
        let mut members: Vec<AgentId> = holders.iter().chain(guests).cloned().collect();
        if let Some(guest) = guest
            && self
                .registry
                .get(guest)
                .is_some_and(|a| a.status.is_live() && !humans::is_human(a))
        {
            members.push(guest.clone());
        }
        members.sort();
        members.dedup();
        if made || reopened {
            events.push(EventKind::ChannelOpened {
                channel: room.id.clone(),
                project: project.clone(),
                title: room.title(),
                members: members.clone(),
            });
        } else {
            for joined in members.iter().filter(|m| !room.members.contains(m)) {
                events.push(EventKind::ChannelJoined {
                    channel: room.id.clone(),
                    agent: joined.clone(),
                });
            }
        }
        room.members = members;
        Ok((room, events))
    }

    /// Commit an agent's record (when it changed), the rooms that follow it
    /// and their events, as one.
    fn commit_roles(
        &mut self,
        record: Option<AgentRecord>,
        rooms: Vec<Channel>,
        kinds: Vec<EventKind>,
        now: DateTime<Utc>,
    ) -> Result<(), Box<Response>> {
        let events: Vec<Event> = kinds
            .into_iter()
            .map(|kind| Event::new(kind, now))
            .collect();
        let committed = self.commit(
            "role",
            events,
            |store, events| store.roles_change(record.as_ref(), &rooms, events),
            |state, _, ()| {
                if let Some(record) = &record {
                    *state.registry.get_mut(&record.id).expect("resolved agent") = record.clone();
                }
                for room in &rooms {
                    state.channels.insert(room.id.clone(), room.clone());
                }
            },
        );
        if committed == Persisted::Committed {
            return Ok(());
        }
        Err(Box::new(self.write_failure().unwrap_or_else(|| {
            Response::error(ErrorCode::Internal, "the roles were not recorded")
        })))
    }

    /// Say something in a role's room as the daemon: who took the role up or
    /// put it down. A word to a room with nobody in it, or one a full inbox
    /// refuses, is left unsaid: the role and its room are already right.
    fn role_notice(&mut self, project: &ProjectId, role: &str, text: String) {
        let now = Utc::now();
        let Ok((room, events)) = self.stage_role_room(project, role, None, None, false, now) else {
            return;
        };
        if room.members.is_empty() {
            return;
        }
        let envelope = Envelope::new(
            "agentd",
            Destination::Channel(room.id.clone()),
            "channel",
            json!({"channel": room.id.as_str(), "title": room.title(), "text": text}),
            None,
            now,
        );
        let response = self.publish_with_channel(envelope, None, Some((room, events, None)), None);
        if !matches!(response, Response::Sent { .. }) {
            info!(%role, ?response, "a role's notice was not said");
        }
    }

    /// Write `roles` as an agent's set, with `role_set`, and bring the rooms
    /// of the roles it took up or put down along in the same commit: the
    /// same way for the one-role `role` request, `role_assign`, and a role
    /// taken out of the palette.
    pub(super) fn write_roles(
        &mut self,
        id: &AgentId,
        roles: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<AgentRecord, Box<Response>> {
        let mut record = self.registry.get(id).expect("resolved agent").clone();
        if record.spec.labels.get(ROLE_LABEL) == roles.as_ref() {
            return Ok(record);
        }
        let before: Vec<String> = record.roles().into_iter().map(str::to_owned).collect();
        match &roles {
            Some(roles) => {
                record
                    .spec
                    .labels
                    .insert(ROLE_LABEL.to_owned(), roles.clone());
            }
            None => {
                record.spec.labels.remove(ROLE_LABEL);
            }
        }
        let after: Vec<String> = record.roles().into_iter().map(str::to_owned).collect();
        let project = record.project.as_ref().map(ProjectRef::id);
        let mut rooms = Vec::new();
        let mut kinds = vec![EventKind::RoleSet {
            agent: id.clone(),
            project: project.clone(),
            role: roles,
        }];
        if let Some(project) = &project {
            for gained in after.iter().filter(|r| !before.contains(r)) {
                let (room, events) =
                    self.stage_role_room(project, gained, None, Some(id), false, now)?;
                rooms.push(room);
                kinds.extend(events);
            }
            for lost in before.iter().filter(|r| !after.contains(r)) {
                if let Some(mut room) = self.role_room(project, lost).cloned()
                    && room.members.contains(id)
                {
                    room.members.retain(|m| m != id);
                    rooms.push(room);
                }
            }
        }
        self.commit_roles(Some(record.clone()), rooms, kinds, now)?;
        Ok(record)
    }

    /// Give an agent roles from its project's palette, or take some away:
    /// the person's to do. Each role it takes up or puts down is said in
    /// that role's room, so the agent hears what it now holds.
    pub(super) fn role_assign(
        &mut self,
        from: &str,
        reference: &str,
        add: &[String],
        remove: &[String],
    ) -> Response {
        let actor = match self.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return Response::error(ErrorCode::Forbidden, "giving roles is the person's to do");
        }
        for role in add.iter().chain(remove) {
            if let Err(reason) = check_role(role) {
                return Response::error(ErrorCode::Invalid, reason);
            }
        }
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let record = self.registry.get(&id).expect("resolved agent").clone();
        if !record.status.is_live() {
            return Response::error(ErrorCode::Invalid, "a finished agent has no roles");
        }
        let Some(project) = record.project.as_ref().map(ProjectRef::id) else {
            return Response::error(
                ErrorCode::Invalid,
                "an agent outside any project has no roles",
            );
        };
        let catalog = match self.project_roles(&project) {
            Ok(catalog) => catalog,
            Err(response) => return *response,
        };
        let unknown: Vec<&str> = add
            .iter()
            .filter(|r| !catalog.iter().any(|c| &c.role == *r))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let names: Vec<&str> = catalog.iter().map(|r| r.role.as_str()).collect();
            return Response::Error {
                code: ErrorCode::Invalid,
                message: format!(
                    "the project has no role called {}; its roles are {}",
                    unknown.join(", "),
                    names.join(", ")
                ),
                details: Some(json!({ "roles": names })),
            };
        }
        let held: Vec<&str> = record.roles();
        let gained: Vec<String> = add
            .iter()
            .filter(|r| !held.contains(&r.as_str()))
            .cloned()
            .collect();
        let lost: Vec<String> = remove
            .iter()
            .filter(|r| held.contains(&r.as_str()) && !add.contains(r))
            .cloned()
            .collect();
        let roles = join_roles(
            held.iter()
                .copied()
                .filter(|r| !lost.iter().any(|l| l == r))
                .chain(gained.iter().map(String::as_str)),
        );
        let record = match self.write_roles(&id, roles, Utc::now()) {
            Ok(record) => record,
            Err(response) => return *response,
        };
        for role in &gained {
            let words = catalog
                .iter()
                .find(|r| &r.role == role)
                .map(|r| {
                    if r.description.is_empty() {
                        r.name.clone()
                    } else {
                        format!("{}: {}", r.name, r.description)
                    }
                })
                .unwrap_or_else(|| role.clone());
            self.role_notice(
                &project,
                role,
                format!("{} now holds {words}", record.spec.name),
            );
        }
        for role in &lost {
            self.role_notice(
                &project,
                role,
                format!("{} no longer holds {role}", record.spec.name),
            );
        }
        Response::Agent {
            agent: self.registry.get(&id).cloned().unwrap_or(record),
        }
    }

    /// The role a channel speaks for, when it is a role's room: a word to
    /// it by its channel id is a word to that role, for policy as for routing.
    pub(super) fn room_role(&self, channel: &ChannelId) -> Option<String> {
        self.channels.get(channel).and_then(|c| c.role.clone())
    }

    /// An agent's message to one of its project's roles, said in that role's
    /// room, the sender joining it to hear the answers. `None` for a sender
    /// with no project of its own — the person, or a sender with no record —
    /// whose `role:` names one holder as before.
    pub(super) fn send_to_role(
        &mut self,
        from: &str,
        role: &str,
        kind: String,
        payload: Value,
        reply_to: Option<MessageId>,
        links: Vec<agentdocker_core::Link>,
    ) -> Option<Response> {
        let sender = self.registry.resolve(from).ok()?;
        let record = self.registry.get(&sender)?;
        if humans::is_human(record) {
            return None;
        }
        let project = record.project.as_ref()?.id();
        if let Err(reason) = check_role(role) {
            return Some(Response::error(ErrorCode::Invalid, reason));
        }
        let action = format!("send:role:{role}");
        let ruling = self.permits(&sender, &action);
        if !ruling.is_allowed() {
            return Some(self.refuse(&sender, &action, ruling));
        }
        let now = Utc::now();
        let (room, events) =
            match self.stage_role_room(&project, role, Some(&sender), None, true, now) {
                Ok(staged) => staged,
                Err(response) => return Some(*response),
            };
        let mut envelope = Envelope::new(
            sender.to_string(),
            Destination::Channel(room.id.clone()),
            kind,
            payload,
            reply_to,
            now,
        );
        envelope.links = links;
        Some(self.publish_with_channel(envelope, None, Some((room, events, None)), None))
    }

    /// Take a role out of every agent of a project that holds it, and close
    /// its room: what taking the role out of the palette needs first, so a
    /// retry after a failure still finds the role to take out.
    pub(super) fn retire_role(
        &mut self,
        project: &ProjectId,
        role: &str,
    ) -> Result<(), Box<Response>> {
        let holders: Vec<AgentRecord> = self
            .registry
            .all()
            .filter(|a| a.holds_role(role))
            .filter(|a| a.project.as_ref().is_some_and(|p| &p.id() == project))
            .cloned()
            .collect();
        let now = Utc::now();
        for holder in holders {
            let roles = join_roles(holder.roles().into_iter().filter(|r| *r != role));
            self.write_roles(&holder.id, roles, now)?;
        }
        if self.role_room(project, role).is_none_or(|c| !c.is_open()) {
            return Ok(());
        }
        self.role_notice(
            project,
            role,
            format!("The role {role} was taken out of the project"),
        );
        let Some(mut room) = self.role_room(project, role).cloned() else {
            return Ok(());
        };
        let members = self.room_members(&room);
        room.members = members;
        room.closed_at = Some(now);
        room.resolution = Some("the role was taken out".to_owned());
        let event = EventKind::ChannelClosed {
            channel: room.id.clone(),
            resolution: room.resolution.clone(),
        };
        self.commit_roles(None, vec![room], vec![event], now)
    }
}
