//! Literal systemd values; shell quoting alone does not prevent expansion.

/// Encode one unit-file word, including empty values and line boundaries.
/// Percent specifiers expand even inside quotes; double each literal percent.
pub(crate) fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/-._=:@".contains(c))
    {
        return value.to_owned();
    }
    let mut result = String::from("\"");
    for c in value.chars() {
        match c {
            '%' => result.push_str("%%"),
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_ascii_control() => result.push_str(&format!("\\x{:02x}", c as u8)),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

/// The ':' command prefix disables environment substitution in every argument.
/// Keep it inside the first quoted word when the executable contains spaces.
pub(crate) fn command(argv: &[String]) -> String {
    argv.iter()
        .enumerate()
        .map(|(i, value)| {
            if i == 0 {
                quote(&format!(":{value}"))
            } else {
                quote(value)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_words_preserve_specifiers_variables_empty_values_and_line_boundaries() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("/safe/path"), "/safe/path");
        assert_eq!(quote("%h/${HOME}/$HOME"), "\"%%h/${HOME}/$HOME\"");
        assert_eq!(
            quote("line\nRestart=no\r\t\\\""),
            "\"line\\nRestart=no\\r\\t\\\\\\\"\""
        );
        assert_eq!(
            command(&["/path %h ${HOME}/exe".into(), "".into(), "$HOME".into()]),
            "\":/path %%h ${HOME}/exe\" \"\" \"$HOME\""
        );
    }
}
