//! `${VAR}` in an Agentfile (version 2 and later), the way a compose file
//! reads it, but strict: a variable that is not set is an error unless the
//! file says what to use instead, so a typo never launches an agent with an
//! empty argument.
//!
//! | text              | means                                                  |
//! |-------------------|--------------------------------------------------------|
//! | `${VAR}`          | the value of `VAR`; an error when it is not set        |
//! | `${VAR:-default}` | `default` when `VAR` is not set or is empty            |
//! | `${VAR-default}`  | `default` when `VAR` is not set                        |
//! | `${VAR:?message}` | an error saying `message` when `VAR` is unset or empty |
//! | `${VAR?message}`  | an error saying `message` when `VAR` is not set        |
//! | `$$`              | one `$`                                                |
//!
//! Any other `$` is an error that says to write `$$`: a shell command such as
//! `echo $HOME` would otherwise reach the agent with the variable silently
//! taken out, or left in, depending on a guess. A default is literal text: it
//! does not nest, and a `$` or `{` in it is refused rather than half-read.

/// Expand `text`, reading variables through `lookup`. `field` names where
/// the text came from, for the error.
pub fn expand(
    text: &str,
    field: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }
        let Some(body) = after.strip_prefix('{') else {
            return Err(format!(
                "{field}: a `$` must start `${{NAME}}`; write `$$` for a dollar sign"
            ));
        };
        let Some(close) = body.find('}') else {
            return Err(format!("{field}: `${{` is never closed with `}}`"));
        };
        out.push_str(&substitute(&body[..close], field, lookup)?);
        rest = &body[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// What is inside one `${…}`.
fn substitute(
    inside: &str,
    field: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let name_end = inside
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(inside.len());
    let (name, operator) = inside.split_at(name_end);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!(
            "{field}: `${{{inside}}}` does not start with a variable name"
        ));
    }
    let value = lookup(name);
    let set_and_not_empty = value.as_deref().is_some_and(|v| !v.is_empty());
    let (op, argument) = if operator.starts_with(":-") || operator.starts_with(":?") {
        operator.split_at(2)
    } else if operator.is_empty() || operator.starts_with('-') || operator.starts_with('?') {
        operator.split_at(operator.len().min(1))
    } else {
        return Err(format!(
            "{field}: `${{{inside}}}` is not `${{NAME}}`, `${{NAME:-default}}` or `${{NAME:?message}}`"
        ));
    };
    match op {
        _ if argument.contains(['$', '{']) => Err(format!(
            "{field}: the default or message in `${{{inside}}}` is literal text; it cannot hold `$` or `{{`"
        )),
        "" => value.ok_or_else(|| {
            format!(
                "{field}: `{name}` is not set; set it, or write `${{{name}:-default}}` to give a value when it is not"
            )
        }),
        ":-" => Ok(if set_and_not_empty {
            value.unwrap_or_default()
        } else {
            argument.to_owned()
        }),
        "-" => Ok(value.unwrap_or_else(|| argument.to_owned())),
        ":?" if set_and_not_empty => Ok(value.unwrap_or_default()),
        "?" if value.is_some() => Ok(value.unwrap_or_default()),
        _ => Err(format!(
            "{field}: `{name}` {}: {}",
            if op == ":?" { "is not set or is empty" } else { "is not set" },
            if argument.is_empty() { "it is required" } else { argument }
        )),
    }
}

/// The text with every `$` doubled: what a version 1 value, which was never
/// expanded, becomes in a version 2 file so that it still means itself.
pub fn escape(text: &str) -> String {
    text.replace('$', "$$")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/me".into()),
            "EMPTY" => Some(String::new()),
            "MODEL" => Some("opus".into()),
            _ => None,
        }
    }

    fn run(text: &str) -> Result<String, String> {
        expand(text, "agents.writer.command[1]", &env)
    }

    #[test]
    fn variables_defaults_and_escapes_expand() {
        assert_eq!(run("plain").unwrap(), "plain");
        assert_eq!(run("${HOME}/src").unwrap(), "/home/me/src");
        assert_eq!(run("--model=${MODEL}").unwrap(), "--model=opus");
        assert_eq!(run("${NOPE:-sonnet}").unwrap(), "sonnet");
        assert_eq!(
            run("${EMPTY:-sonnet}").unwrap(),
            "sonnet",
            ":- covers empty"
        );
        assert_eq!(run("${EMPTY-sonnet}").unwrap(), "", "- covers only unset");
        assert_eq!(run("${NOPE-sonnet}").unwrap(), "sonnet");
        assert_eq!(run("${MODEL:-sonnet}").unwrap(), "opus");
        assert_eq!(run("${EMPTY}").unwrap(), "", "set and empty is a value");
        assert_eq!(run("${EMPTY?needed}").unwrap(), "");
        assert_eq!(run("costs $$5, $${HOME}").unwrap(), "costs $5, ${HOME}");
        assert_eq!(run("${NOPE:-a b: c}").unwrap(), "a b: c");
        assert_eq!(run("${HOME}${MODEL}").unwrap(), "/home/meopus");
    }

    #[test]
    fn what_cannot_be_expanded_is_an_error_naming_the_field() {
        let unset = run("${NOPE}").unwrap_err();
        assert!(unset.contains("agents.writer.command[1]"), "{unset}");
        assert!(unset.contains("`NOPE` is not set"), "{unset}");
        assert!(
            unset.contains(":-default"),
            "says how to give a default: {unset}"
        );
        let required = run("${EMPTY:?the API key}").unwrap_err();
        assert!(
            required.contains("is not set or is empty: the API key"),
            "{required}"
        );
        assert!(run("${NOPE?x}").unwrap_err().contains("is not set: x"));
        let lone = run("echo $HOME").unwrap_err();
        assert!(
            lone.contains("$$"),
            "a lone dollar says how to write one: {lone}"
        );
        assert!(run("trailing $").is_err());
        assert!(run("${HOME").unwrap_err().contains("never closed"));
        assert!(run("${}").is_err());
        assert!(run("${1X}").is_err());
        assert!(run("${HOME:+x}").is_err());
        assert!(run("${HO ME}").is_err());
        let nested = run("${NOPE:-${HOME}}").unwrap_err();
        assert!(
            nested.contains("literal text"),
            "defaults do not nest: {nested}"
        );
    }

    #[test]
    fn escaping_round_trips_through_expansion() {
        for text in ["", "no dollars", "echo $HOME", "$$", "${HOME}", "a$"] {
            assert_eq!(run(&escape(text)).unwrap(), text, "{text:?}");
        }
    }
}
