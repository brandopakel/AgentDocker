//! Temporary review fields deliberately bypass ordinary message/draft controls.
use super::*;
use agentdocker_core::secret::{PROVIDER_NOTICE, SecretText};
use iced::widget::column;

impl App {
    pub(super) fn secret_review_panel(&self, c: Colors) -> Element<'_, Message> {
        let now = Utc::now();
        let mut cards = column![].spacing(12);
        for review in self.secrets.reviews.iter().filter(|r| r.expires_at > now) {
            let mut card = column![
                heading(format!("Temporary input · {}", self.name_of(review.agent.as_str())), 16),
                note(PROVIDER_NOTICE, c),
                small(format!("Expires in {} seconds. Edits are discarded if this window disconnects or closes.", (review.expires_at - now).num_seconds().max(0)), c),
            ].spacing(8);
            if let Some(agent) = self.agents.iter().find(|a| a.id == review.agent)
                && let Some(project) = &agent.project
            {
                card = card.push(small(format!("Project: {}", project.dir().display()), c));
            }
            if self.secrets.attempted(&review.id) {
                card = card.push(note("Submission or cancellation was attempted. A missing confirmation does not authorize another submission.", c));
            } else {
                let draft = self.secrets.drafts.get(&review.id);
                for field in &review.request.fields {
                    let value = draft
                        .and_then(|d| d.values.get(&field.id))
                        .map_or("", SecretText::expose);
                    let route = review.id.clone();
                    let key = field.id.clone();
                    card = card.push(text(field.question.clone()).size(14)).push(
                        crate::controls::secret_input(
                            format!("temporary-{}-{}", review.id, field.id),
                            &field.question,
                            value,
                            move |text| {
                                Message::SecretEdit(
                                    route.clone(),
                                    key.clone(),
                                    SecretText::new(text).ok(),
                                )
                            },
                            self.connected.is_ok(),
                        ),
                    );
                }
                let acknowledged = draft.is_some_and(|d| d.retention_acknowledged);
                card = card.push(action(
                    format!("temporary-notice-{}", review.id),
                    if acknowledged {
                        "Provider retention notice acknowledged"
                    } else {
                        "Acknowledge provider retention notice"
                    },
                    self.connected
                        .is_ok()
                        .then_some(Message::SecretAcknowledge(review.id.clone(), !acknowledged)),
                    acknowledged,
                ));
            }
            card = card.push(
                row![
                    ghost(
                        format!("temporary-cancel-{}", review.id),
                        "Cancel",
                        self.connected
                            .is_ok()
                            .then_some(Message::SecretCancel(review.id.clone()))
                    ),
                    primary(
                        format!("temporary-submit-{}", review.id),
                        "Submit once",
                        (self.connected.is_ok() && self.secrets.ready(&review.id, now))
                            .then_some(Message::SecretSubmit(review.id.clone()))
                    ),
                ]
                .spacing(8),
            );
            cards = cards.push(
                container(card)
                    .padding(16)
                    .width(Fill)
                    .style(move |_| c.card_style()),
            );
        }
        cards.into()
    }
}
