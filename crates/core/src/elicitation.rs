//! Bounded, nonsecret MCP form schemas. Parsing and validation do no I/O.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_FORM_TEXT: usize = 4_096;
pub const MAX_FORM_BYTES: usize = 16_000;
const MAX_FIELDS: usize = 16;
const MAX_OPTIONS: usize = 32;
// Do not compare integers beyond the exact range of the provider's doubles.
const MAX_NUMBER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, Debug, PartialEq)]
pub struct McpForm {
    pub fields: Vec<FormField>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    pub key: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub kind: FormKind,
    pub default: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormKind {
    Text {
        min: usize,
        max: usize,
        format: Option<String>,
    },
    Number {
        integer: bool,
        min: f64,
        max: f64,
    },
    Boolean,
    Select {
        options: Vec<(String, String)>,
        multiple: bool,
        min: usize,
        max: usize,
    },
}

fn visible(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(|c| {
        (c.is_control() && !matches!(c, '\n' | '\t'))
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

fn object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, &'static str> {
    let object = value.as_object().ok_or("Expected a flat form object")?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Unsupported form schema field");
    }
    Ok(object)
}

fn string(value: &Value, max: usize) -> Result<String, &'static str> {
    value
        .as_str()
        .filter(|v| visible(v, max))
        .map(str::to_owned)
        .ok_or("Invalid or oversized form text")
}

fn optional_string(value: &Value, max: usize) -> Result<String, &'static str> {
    if value.is_null() {
        Ok(String::new())
    } else {
        string(value, max)
    }
}

fn limit(value: &Value, default: usize, maximum: usize) -> Result<usize, &'static str> {
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= maximum)
        .ok_or("Form bound exceeds the supported limit")
}

fn number(value: &Value, default: f64) -> Result<f64, &'static str> {
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() <= MAX_NUMBER)
        .ok_or("Form number exceeds the exact supported range")
}

fn options(schema: &Value, titled: &str) -> Result<Vec<(String, String)>, &'static str> {
    let values = if schema[titled].is_null() {
        &schema["enum"]
    } else {
        &schema[titled]
    };
    let values = values
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= MAX_OPTIONS)
        .ok_or("A selection needs bounded options")?;
    let names = schema.get("enumNames").filter(|v| !v.is_null());
    if let Some(names) = names
        && !names.as_array().is_some_and(|n| n.len() == values.len())
    {
        return Err("Option names do not match their values");
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let (key, label) = if schema[titled].is_null() {
            let key = string(value, 256)?;
            let label = match names {
                Some(n) => string(&n[index], 256)?,
                None => key.clone(),
            };
            (key, label)
        } else {
            object(value, &["const", "title"])?;
            (string(&value["const"], 256)?, string(&value["title"], 256)?)
        };
        if label.trim().is_empty() || !seen.insert(key.clone()) {
            return Err("Selection options must have unique values and visible labels");
        }
        result.push((key, label));
    }
    Ok(result)
}

impl McpForm {
    /// Parse the provider's flat MCP 2025-11-25 schema; unknown constraints fail
    /// closed instead of silently relaxing the server's requested validation.
    pub fn parse(schema: &Value) -> Result<Self, &'static str> {
        if schema.to_string().len() > MAX_FORM_BYTES {
            return Err("Form schema is too large");
        }
        object(schema, &["type", "properties", "required", "$schema"])?;
        optional_string(&schema["$schema"], 256)?;
        if schema["type"] != "object" {
            return Err("A form must be a flat object");
        }
        let properties = schema["properties"]
            .as_object()
            .filter(|p| !p.is_empty() && p.len() <= MAX_FIELDS)
            .ok_or("A form needs one to sixteen fields")?;
        let mut required = BTreeSet::new();
        if !schema["required"].is_null() {
            for key in schema["required"]
                .as_array()
                .ok_or("Invalid required fields")?
            {
                let key = key.as_str().ok_or("Invalid required field name")?;
                if !properties.contains_key(key) || !required.insert(key) {
                    return Err("Invalid required fields");
                }
            }
        }
        let mut fields = Vec::new();
        for (key, value) in properties {
            if key.trim().is_empty()
                || key.len() > 128
                || !visible(key, 128)
                || key.contains(['\n', '\t'])
            {
                return Err("Invalid form field name");
            }
            let mut allowed = vec!["type", "title", "description", "default"];
            let kind = match value["type"].as_str() {
                Some("string") if !value["enum"].is_null() || !value["oneOf"].is_null() => {
                    if !value["enum"].is_null() && !value["oneOf"].is_null() {
                        return Err("Conflicting selection schema");
                    }
                    allowed.extend(if value["oneOf"].is_null() {
                        vec!["enum", "enumNames"]
                    } else {
                        vec!["oneOf"]
                    });
                    FormKind::Select {
                        options: options(value, "oneOf")?,
                        multiple: false,
                        min: 1,
                        max: 1,
                    }
                }
                Some("string") => {
                    allowed.extend(["minLength", "maxLength", "format"]);
                    let min = limit(&value["minLength"], 0, MAX_FORM_TEXT)?;
                    let max = limit(&value["maxLength"], MAX_FORM_TEXT, MAX_FORM_TEXT)?;
                    let format = if value["format"].is_null() {
                        None
                    } else {
                        let f = string(&value["format"], 16)?;
                        if !matches!(f.as_str(), "email" | "uri" | "date" | "date-time") {
                            return Err("Unsupported text format");
                        }
                        Some(f)
                    };
                    if min > max {
                        return Err("Invalid text length bounds");
                    }
                    FormKind::Text { min, max, format }
                }
                Some("number" | "integer") => {
                    allowed.extend(["minimum", "maximum"]);
                    let min = number(&value["minimum"], -MAX_NUMBER)?;
                    let max = number(&value["maximum"], MAX_NUMBER)?;
                    if min > max {
                        return Err("Invalid number bounds");
                    }
                    FormKind::Number {
                        integer: value["type"] == "integer",
                        min,
                        max,
                    }
                }
                Some("boolean") => FormKind::Boolean,
                Some("array") => {
                    allowed.extend(["items", "minItems", "maxItems"]);
                    let items = &value["items"];
                    if items["anyOf"].is_null() {
                        object(items, &["type", "enum"])?;
                        if items["type"] != "string" {
                            return Err("Only string selections are supported");
                        }
                    } else {
                        object(items, &["anyOf"])?;
                    }
                    let options = options(items, "anyOf")?;
                    let min = limit(&value["minItems"], 0, options.len())?;
                    let max =
                        limit(&value["maxItems"], options.len(), MAX_OPTIONS)?.min(options.len());
                    if min > max {
                        return Err("Invalid selection bounds");
                    }
                    FormKind::Select {
                        options,
                        multiple: true,
                        min,
                        max,
                    }
                }
                _ => return Err("Unsupported form field type"),
            };
            object(value, &allowed)?;
            let title = optional_string(&value["title"], 256)?;
            let field = FormField {
                key: key.clone(),
                title: if title.trim().is_empty() {
                    key.clone()
                } else {
                    title
                },
                description: optional_string(&value["description"], MAX_FORM_TEXT)?,
                required: required.contains(key.as_str()),
                kind,
                default: value.get("default").filter(|v| !v.is_null()).cloned(),
            };
            if let Some(default) = &field.default {
                field.validate(default)?;
            }
            fields.push(field);
        }
        Ok(Self { fields })
    }

    pub fn validate(&self, content: &Value) -> Result<(), &'static str> {
        if content.to_string().len() > MAX_FORM_BYTES {
            return Err("Form response is too large");
        }
        let values = content
            .as_object()
            .ok_or("A form response must be an object")?;
        if values
            .keys()
            .any(|k| !self.fields.iter().any(|f| &f.key == k))
        {
            return Err("Unknown form response field");
        }
        for field in &self.fields {
            if let Some(value) = values.get(&field.key) {
                field.validate(value)?;
            } else if field.required {
                return Err("Complete every required field");
            }
        }
        Ok(())
    }

    /// Only an explicit valid object means accept; defaults never submit themselves.
    pub fn response(&self, answer: &str) -> Option<Value> {
        if matches!(answer, "Decline" | "Cancel") {
            return Some(json!({"action":answer.to_ascii_lowercase(),"content":null}));
        }
        if answer.len() > MAX_FORM_BYTES {
            return None;
        }
        let content: Value = serde_json::from_str(answer).ok()?;
        self.validate(&content).ok()?;
        Some(json!({"action":"accept","content":content}))
    }

    pub fn defaults(&self) -> BTreeMap<String, Value> {
        self.fields
            .iter()
            .filter_map(|f| f.default.clone().map(|v| (f.key.clone(), v)))
            .collect()
    }
}

impl FormField {
    pub fn validate(&self, value: &Value) -> Result<(), &'static str> {
        let valid = match &self.kind {
            FormKind::Text { min, max, format } => value.as_str().is_some_and(|s| {
                visible(s, MAX_FORM_TEXT)
                    && (*min..=*max).contains(&s.chars().count())
                    && match format.as_deref() {
                        None => true,
                        Some("email") => email_address::EmailAddress::parse_with_options(
                            s,
                            email_address::Options::default().without_display_text(),
                        )
                        .is_ok(),
                        Some("uri") => fluent_uri::Uri::parse(s).is_ok(),
                        Some("date") => {
                            s.len() == 10
                                && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
                        }
                        Some("date-time") => chrono::DateTime::parse_from_rfc3339(s).is_ok(),
                        _ => false,
                    }
            }),
            FormKind::Number { integer, min, max } => value.as_f64().is_some_and(|n| {
                n.is_finite()
                    && n.abs() <= MAX_NUMBER
                    && n >= *min
                    && n <= *max
                    && (!integer || n.fract() == 0.0)
            }),
            FormKind::Boolean => value.is_boolean(),
            FormKind::Select {
                options,
                multiple,
                min,
                max,
            } => {
                let known = |v: &Value| {
                    v.as_str()
                        .is_some_and(|v| options.iter().any(|(key, _)| key == v))
                };
                if *multiple {
                    value.as_array().is_some_and(|values| {
                        (*min..=*max).contains(&values.len())
                            && values.iter().all(known)
                            && values
                                .iter()
                                .map(|v| v.as_str().unwrap_or_default())
                                .collect::<BTreeSet<_>>()
                                .len()
                                == values.len()
                    })
                } else {
                    known(value)
                }
            }
        };
        if valid {
            Ok(())
        } else {
            Err("The value does not meet this field's type, format or bounds")
        }
    }

    pub fn hint(&self) -> String {
        match &self.kind {
            FormKind::Text { min, max, format } => format!(
                "{} · {min}–{max} characters",
                format.as_deref().unwrap_or("Text")
            ),
            FormKind::Number { integer, min, max } => {
                let kind = if *integer { "Whole number" } else { "Number" };
                match (*min == -MAX_NUMBER, *max == MAX_NUMBER) {
                    (true, true) => kind.into(),
                    (false, true) => format!("{kind} · at least {min}"),
                    (true, false) => format!("{kind} · at most {max}"),
                    (false, false) => format!("{kind} · {min} to {max}"),
                }
            }
            FormKind::Boolean => "Choose Yes or No".into(),
            FormKind::Select {
                multiple, min, max, ..
            } => {
                if *multiple {
                    format!("Choose {min}–{max}")
                } else {
                    "Choose one".into()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_form_validates_types_bounds_defaults_and_exact_selection_values() {
        let schema = json!({"type":"object","required":["name","count","agree","color","tags"],"properties":{
            "name":{"type":"string","minLength":2,"maxLength":4,"default":"Zoë"},
            "count":{"type":"integer","minimum":1,"maximum":3},
            "agree":{"type":"boolean"},
            "color":{"type":"string","oneOf":[{"const":"#123","title":"Blue"},{"const":"#456","title":"Blue"}]},
            "tags":{"type":"array","minItems":1,"maxItems":2,"items":{"anyOf":[{"const":"a","title":"A"},{"const":"b","title":"B"}]}},
            "optional":{"type":"string","enum":["x","y"],"enumNames":["X","Y"],"default":"y"}
        }});
        let form = McpForm::parse(&schema).unwrap();
        let content = json!({"name":"Joé","count":2,"agree":false,"color":"#456","tags":["a"]});
        assert_eq!(
            form.response(&content.to_string()).unwrap(),
            json!({"action":"accept","content":content})
        );
        assert_eq!(form.defaults()["optional"], "y");
        assert!(
            form.response("Accept").is_none(),
            "a choice must not silently submit defaults"
        );
        for (field, bad) in [
            ("name", json!("é")),
            ("name", json!("12345")),
            ("count", json!(1.5)),
            ("count", json!(4)),
            ("agree", json!("false")),
            ("color", json!("Blue")),
            ("tags", json!([])),
            ("tags", json!(["a", "a"])),
            ("tags", json!(["unknown"])),
        ] {
            let mut input = content.clone();
            input[field] = bad;
            assert!(form.validate(&input).is_err(), "{input}");
        }
        let mut absent = content.clone();
        absent.as_object_mut().unwrap().remove("agree");
        assert!(form.validate(&absent).is_err());
        let mut extra = content;
        extra["unexpected"] = json!(true);
        assert!(form.validate(&extra).is_err());
        assert_eq!(
            form.response("Decline").unwrap(),
            json!({"action":"decline","content":null})
        );
        assert_eq!(
            form.response("Cancel").unwrap(),
            json!({"action":"cancel","content":null})
        );
    }

    #[test]
    fn mcp_form_refuses_unknown_constraints_bad_defaults_and_precision_loss() {
        for field in [
            json!({"type":"string","pattern":"[a-z]"}),
            json!({"type":"string","format":"password"}),
            json!({"type":"object","properties":{}}),
            json!({"type":"string","minLength":3,"maxLength":1}),
            json!({"type":"number","minimum":9007199254740992_u64}),
            json!({"type":"integer","default":1.5}),
            json!({"type":"boolean","default":"true"}),
            json!({"type":"string","enum":["one","one"]}),
            json!({"type":"string","enum":["one"],"enumNames":["One","Two"]}),
        ] {
            assert!(
                McpForm::parse(&json!({"type":"object","properties":{"field":field}})).is_err()
            );
        }
        let schema = json!({"type":"object","properties":{"field":{"type":"integer"}}});
        assert!(
            McpForm::parse(&schema)
                .unwrap()
                .validate(&json!({"field":9007199254740993_u64}))
                .is_err()
        );
        let mut schema = schema;
        schema["required"] = json!(["missing"]);
        assert!(McpForm::parse(&schema).is_err());
    }

    #[test]
    fn mcp_form_formats_are_validated_without_fetching_or_normalizing() {
        for (format, good, bad) in [
            (
                "email",
                "person+tag@example.com",
                "Name <person@example.com>",
            ),
            ("uri", "urn:example:fixture", "relative/path"),
            ("date", "2024-02-29", "2025-02-29"),
            (
                "date-time",
                "2026-10-04T12:30:00-07:00",
                "2026-10-04T12:30:00",
            ),
        ] {
            let form = McpForm::parse(
                &json!({"type":"object","properties":{"field":{"type":"string","format":format}}}),
            )
            .unwrap();
            let input = json!({"field":good});
            assert!(form.validate(&input).is_ok(), "{format}");
            assert_eq!(form.response(&input.to_string()).unwrap()["content"], input);
            assert!(form.validate(&json!({"field":bad})).is_err(), "{format}");
        }
    }
}
