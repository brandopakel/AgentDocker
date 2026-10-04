//! In-memory MCP form edits, bound to one original reviewed schema.
use super::*;
use agentdocker_core::{FormField, FormKind, McpForm, QuestionPresentation};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default)]
pub(super) struct FieldValue {
    pub included: bool,
    pub text: String,
    pub selected: BTreeSet<String>,
}

impl FieldValue {
    pub fn initial(field: &FormField) -> Self {
        let mut value = Self {
            included: field.required || field.default.is_some(),
            ..Self::default()
        };
        if let Some(default) = &field.default {
            match &field.kind {
                FormKind::Select { multiple: true, .. } => {
                    value.selected = default
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                }
                FormKind::Text { .. } | FormKind::Select { .. } => {
                    value.text = default.as_str().unwrap_or_default().to_owned()
                }
                _ => value.text = default.to_string(),
            }
        }
        value
    }

    fn value(&self, field: &FormField) -> Result<Value, String> {
        let value = match &field.kind {
            FormKind::Text { .. }
            | FormKind::Select {
                multiple: false, ..
            } => Value::String(self.text.clone()),
            FormKind::Number { .. } | FormKind::Boolean => serde_json::from_str(&self.text)
                .map_err(|_| format!("{}: {}", field.title, field.hint()))?,
            FormKind::Select { multiple: true, .. } => json!(self.selected),
        };
        field
            .validate(&value)
            .map_err(|reason| format!("{}: {reason}", field.title))?;
        Ok(value)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Draft {
    pub schema: Value,
    pub fields: BTreeMap<String, FieldValue>,
}

impl Draft {
    pub fn new(schema: &Value, form: &McpForm) -> Self {
        Self {
            schema: schema.clone(),
            fields: form
                .fields
                .iter()
                .map(|f| (f.key.clone(), FieldValue::initial(f)))
                .collect(),
        }
    }

    pub fn answer(&self, form: &McpForm) -> Result<String, String> {
        let mut content = serde_json::Map::new();
        for field in &form.fields {
            let input = self
                .fields
                .get(&field.key)
                .ok_or("The form changed. Review it again.")?;
            if field.required || input.included {
                content.insert(field.key.clone(), input.value(field)?);
            }
        }
        let value = Value::Object(content);
        form.validate(&value).map_err(str::to_owned)?;
        Ok(value.to_string())
    }
}

impl App {
    /// Refuse stale, completed, disconnected or sending forms before editing or submitting.
    pub(super) fn active_form(&self, id: &MessageId) -> Option<(Value, McpForm)> {
        if self.connected.is_err() || self.sending.contains(id) {
            return None;
        }
        let question = self
            .questions
            .iter()
            .find(|q| q.id == *id && !q.expired(Utc::now()))?;
        let presentation = question
            .presentation
            .as_ref()
            .filter(|p| p.valid_for(&question.text))?;
        let QuestionPresentation::McpForm { schema, .. } = presentation else {
            return None;
        };
        Some((schema.clone(), McpForm::parse(schema).ok()?))
    }

    pub(super) fn edit_form(&mut self, id: MessageId, key: String, edit: Edit) {
        let Some((schema, form)) = self.active_form(&id) else {
            return;
        };
        let Some(field) = form.fields.iter().find(|f| f.key == key) else {
            return;
        };
        if !self.shell.forms.contains_key(&id) && self.shell.forms.len() >= 64 {
            self.shell
                .answer_errors
                .insert(id, "Finish another form before editing this one.".into());
            return;
        }
        let draft = self
            .shell
            .forms
            .entry(id.clone())
            .or_insert_with(|| Draft::new(&schema, &form));
        if draft.schema != schema {
            *draft = Draft::new(&schema, &form);
        }
        let Some(value) = draft.fields.get_mut(&key) else {
            return;
        };
        match edit {
            Edit::Text(text)
                if text.len() <= agentdocker_core::elicitation::MAX_FORM_TEXT
                    && matches!(field.kind, FormKind::Text { .. } | FormKind::Number { .. }) =>
            {
                value.text = text;
                value.included = true;
            }
            Edit::Include(included) if !field.required => value.included = included,
            Edit::Select(selected) => match &field.kind {
                FormKind::Boolean if matches!(selected.as_str(), "true" | "false") => {
                    value.text = selected;
                    value.included = true;
                }
                FormKind::Select {
                    options, multiple, ..
                } if options.iter().any(|(v, _)| v == &selected) => {
                    if *multiple {
                        if !value.selected.remove(&selected) {
                            value.selected.insert(selected);
                        }
                    } else {
                        value.text = selected;
                    }
                    value.included = true;
                }
                _ => return,
            },
            _ => return,
        }
        self.shell.answer_errors.remove(&id);
    }

    pub(super) fn submit_form(&mut self, id: MessageId) {
        let Some((schema, form)) = self.active_form(&id) else {
            return;
        };
        let initial = Draft::new(&schema, &form);
        if self
            .shell
            .forms
            .get(&id)
            .is_some_and(|draft| draft.schema != schema)
        {
            self.shell.forms.insert(id.clone(), initial);
            self.shell.answer_errors.insert(
                id,
                "The form changed. Review the new fields before submitting.".into(),
            );
            return;
        }
        let draft = self.shell.forms.get(&id).unwrap_or(&initial);
        let answer = match draft.answer(&form) {
            Ok(answer) => answer,
            Err(error) => {
                self.shell.answer_errors.insert(id, error);
                return;
            }
        };
        self.shell.answer_errors.remove(&id);
        self.sending.insert(id.clone());
        self.shell.pending_answer_reveal = Some(id.clone());
        self.shell
            .chosen_answers
            .insert(id.clone(), "Submit".into());
        self.send(Cmd::Answer(id, answer));
    }
}

pub(super) enum Edit {
    Text(String),
    Include(bool),
    Select(String),
}
