//! Metadata for a completed AgentDocker output write. No output text is stored.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Hook,
    Mcp,
    Channel,
    ManagedInput,
    NativeHook,
    NativeInput,
    BrowserMcp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Emission {
    /// One random identity per successful write; retrying its report reuses it.
    pub id: String,
    pub channel: Channel,
    pub bytes: u64,
}

impl Emission {
    pub fn check(&self) -> Result<(), &'static str> {
        if self.id.len() != 32
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("emission id must be 32 lowercase hexadecimal characters");
        }
        if self.bytes == 0 || self.bytes > 64 * 1024 * 1024 {
            return Err("emitted bytes must be between 1 and 67108864");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_metadata_is_accepted() {
        let valid = Emission {
            id: "a".repeat(32),
            channel: Channel::Mcp,
            bytes: 4,
        };
        assert!(valid.check().is_ok());
        for bytes in [0, 64 * 1024 * 1024 + 1, u64::MAX] {
            assert!(
                Emission {
                    bytes,
                    ..valid.clone()
                }
                .check()
                .is_err()
            );
        }
        for id in [
            String::new(),
            "A".repeat(32),
            "g".repeat(32),
            "1".repeat(33),
        ] {
            assert!(
                Emission {
                    id,
                    ..valid.clone()
                }
                .check()
                .is_err()
            );
        }
        let mut value = serde_json::to_value(&valid).unwrap();
        value["output"] = serde_json::json!("private content");
        assert!(serde_json::from_value::<Emission>(value).is_err());
    }
}
