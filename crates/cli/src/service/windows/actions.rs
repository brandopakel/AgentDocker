//! Literal-only recognition of the task actions this application renders.
//! This is not a PowerShell evaluator: any different script preserves builds.
use super::{encoded, quoted};
use base64::Engine;

const PREFIX: &str = "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand ";

pub(crate) fn arguments(script: &str) -> String {
    format!("{PREFIX}{}", encoded(script))
}

pub(crate) fn daemon(controller: &str, home: &str, agentd: &str, endpoint: &str) -> String {
    format!(
        "& {} daemon supervise --home {} --agentd {} --endpoint {}; exit $LASTEXITCODE",
        quoted(controller),
        quoted(home),
        quoted(agentd),
        quoted(endpoint),
    )
}

pub(crate) fn connector(
    home: &str,
    controller: &str,
    endpoint: &str,
    owner: &str,
    args: &[String],
    log: &str,
) -> String {
    let args = args
        .iter()
        .map(|arg| quoted(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "$ErrorActionPreference='Stop';$env:AGENTDOCKER_HOME={};$env:AGENTDOCKER_NO_AUTOSTART='1';Remove-Item Env:AGENTDOCKER_AGENT_ID,Env:AGENTDOCKER_AGENT_NAME,Env:AGENTDOCKER_SOCKET,Env:AGENTDOCKER_TOKEN_FILE -ErrorAction SilentlyContinue; $ErrorActionPreference='Continue'; $restarts=0; while($true){{$began=[DateTime]::UtcNow;$LASTEXITCODE=$null; & {} --socket {} connector service-run --owner {} {args} *> {}; $code=$LASTEXITCODE; if($null -eq $code){{exit 1}}; if($code -eq 0){{exit 0}}; if(([DateTime]::UtcNow-$began).TotalSeconds -ge 600){{$restarts=0}}; if($restarts -ge 3){{exit $code}}; $restarts++; Start-Sleep -Seconds 2}}",
        quoted(home),
        quoted(controller),
        quoted(endpoint),
        quoted(owner),
        quoted(log),
    )
}

/// Every literal is retained, including service/tunnel arguments and log paths.
/// Callers still need to verify the PowerShell executable and interpret paths.
pub(crate) struct LiteralAction {
    pub(crate) controller: String,
    pub(crate) values: Vec<String>,
}

pub(crate) fn recognize(arguments: &str) -> Option<LiteralAction> {
    if arguments.len() > 64 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(arguments.strip_prefix(PREFIX)?)
        .ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let script = String::from_utf16(&units).ok()?;
    // No extra command-line switches, abbreviation, quoting or base64 aliases.
    if self::arguments(&script) != arguments {
        return None;
    }
    let values = literals(&script)?;
    let controller =
        if values.len() == 4 && daemon(&values[0], &values[1], &values[2], &values[3]) == script {
            values[0].clone()
        } else if values.len() >= 8
            && connector(
                &values[1],
                &values[4],
                &values[5],
                &values[6],
                &values[7..values.len() - 1],
                values.last()?,
            ) == script
        {
            values[4].clone()
        } else {
            return None;
        };
    Some(LiteralAction { controller, values })
}

fn literals(script: &str) -> Option<Vec<String>> {
    let mut chars = script.chars().peekable();
    let mut values = Vec::new();
    while let Some(character) = chars.next() {
        if character != '\'' {
            continue;
        }
        let mut value = String::new();
        loop {
            let character = chars.next()?;
            if matches!(
                character,
                '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
            ) {
                if chars.peek() == Some(&character) {
                    chars.next();
                } else if character == '\'' {
                    break;
                } else {
                    // A smart quote is a PowerShell delimiter too. Only the
                    // doubled spelling emitted by quoted() is a literal here.
                    return None;
                }
            }
            value.push(character);
        }
        values.push(value);
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    const CONTROLLER: &str = r"C:\Users\Test\AgentDocker\desktop\bin\agentdocker.exe";
    const HOME: &str = r"C:\Users\Test\.agentdocker";
    const AGENTD: &str = r"C:\Users\Test\AgentDocker\desktop\bin\agentd.exe";
    const ENDPOINT: &str = r"\\.\pipe\agentdocker-test";
    const OWNER: &str = "AgentDocker per-user connector; ownership fixture";
    const LOG: &str = r"C:\Users\Test\.agentdocker\connector\service.log";

    #[test]
    fn task_rendering_preserves_the_preexisting_definition_bytes() {
        // Hashes independently captured from the ee37be35 renderers before
        // extracting them. Task ownership compares these exact arguments.
        let daemon = daemon(CONTROLLER, HOME, AGENTD, ENDPOINT);
        let connector = connector(
            HOME,
            CONTROLLER,
            ENDPOINT,
            OWNER,
            &["--listen".into(), "127.0.0.1:8123".into()],
            LOG,
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(daemon.as_bytes())),
            "30e91f5f24ac9d2a518aa373babba39c1bb6f6091d41243eda2af21e58198cfd"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(connector.as_bytes())),
            "808ebf0adf496edad3e9d4e1cfa30d705e9d6cd888aac68671a3d98907c33131"
        );
    }

    #[test]
    fn recognized_literals_preserve_quotes_unicode_and_shell_metacharacters() {
        let path = "C:\\Users\\O''Brien ‘quoted’ ‚value‛ 雪\\$env:TEMP; $(exit)\\agentdocker.exe";
        let action = recognize(&arguments(&daemon(path, HOME, AGENTD, ENDPOINT))).unwrap();
        assert_eq!(action.controller, path);
        assert_eq!(action.values, [path, HOME, AGENTD, ENDPOINT]);
        let args = vec!["--cloudflared".into(), path.into(), "".into()];
        let action = recognize(&arguments(&connector(
            HOME, path, ENDPOINT, OWNER, &args, LOG,
        )))
        .unwrap();
        assert_eq!(action.controller, path);
        assert_eq!(&action.values[7..10], &args);
        assert_eq!(action.values.last().unwrap(), LOG);
    }

    #[test]
    fn unknown_wrappers_extra_statements_and_changed_fixed_literals_are_opaque() {
        let script = daemon(CONTROLLER, HOME, AGENTD, ENDPOINT);
        let wrapper = connector(HOME, CONTROLLER, ENDPOINT, OWNER, &[], LOG);
        for changed in [
            format!("{script}; & $env:OTHER_SERVICE"),
            script.replace("exit $LASTEXITCODE", "exit 0"),
            script.replace(
                &quoted(CONTROLLER),
                "(Join-Path $env:APP_ROOT 'agentdocker.exe')",
            ),
            wrapper.replace("'Stop'", "'SilentlyContinue'"),
            wrapper.replace("'1'", "'0'"),
            wrapper.replace("Start-Sleep -Seconds 2", "Start-Sleep -Seconds 1"),
        ] {
            assert!(recognize(&arguments(&changed)).is_none());
        }
        for changed in [
            script,
            "-File service.ps1".into(),
            format!("{} --extra", arguments(&wrapper)),
            arguments(&wrapper).replace("-NoProfile ", ""),
        ] {
            assert!(recognize(&changed).is_none());
        }
    }

    #[test]
    fn malformed_encoding_and_unescaped_quote_delimiters_are_opaque() {
        for value in [
            "not base64".into(),
            base64::engine::general_purpose::STANDARD.encode([0_u8]),
            base64::engine::general_purpose::STANDARD.encode([0_u8, 0xD8]),
            "A".repeat(64 * 1024),
        ] {
            assert!(recognize(&format!("{PREFIX}{value}")).is_none());
        }
        let script = daemon(
            "C:\\Users\\O‘Brien\\agentdocker.exe",
            HOME,
            AGENTD,
            ENDPOINT,
        );
        assert!(recognize(&arguments(&script.replace("‘‘", "‘"))).is_none());
        assert!(recognize(&arguments(&script[..script.len() - 1])).is_none());
    }
}
