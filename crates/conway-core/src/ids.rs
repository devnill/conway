//! Identifier newtypes shared across the workspace.
//!
//! Serde representation for every newtype is the transparent inner value, so a
//! `SessionId` serializes as a bare JSON string and a `LogSeq` as a bare
//! number. This is load-bearing for the JSONL session log format (§5.1).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::ConwayError;

/// ULID-backed identifiers: sortable, timestamped, human-pasteable.
macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub ulid::Ulid);

        impl $name {
            /// Generate a fresh identifier.
            pub fn new() -> Self {
                Self(ulid::Ulid::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ConwayError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                ulid::Ulid::from_string(s)
                    .map(Self)
                    .map_err(|e| ConwayError::Parse {
                        detail: format!(concat!(stringify!($name), ": invalid ULID {:?}: {}"), s, e),
                    })
            }
        }
    };
}

/// String-backed identifiers.
macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ConwayError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_string()))
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

ulid_id!(
    /// Identifies one session (one agent's append-only log).
    SessionId
);
ulid_id!(
    /// Identifies one agent in the tree.
    AgentId
);
ulid_id!(
    /// Identifies one prompt segment within an assembled context.
    SegmentId
);

string_id!(
    /// A model name as the backend knows it (e.g. `claude-sonnet-4-6`, `qwen3-coder:30b`).
    ModelId
);
string_id!(
    /// A configured backend instance (e.g. `anthropic`, `local`).
    BackendId
);
string_id!(
    /// A network endpoint identity used by the health registry.
    EndpointId
);
string_id!(
    /// A routing role alias (e.g. `planner`, `fast`).
    RoleAlias
);
string_id!(
    /// A tool name as registered by a plugin.
    ToolName
);

/// Stable identity of a shared context prefix: the cache/slot lookup key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrefixKey(pub String);

impl PrefixKey {
    /// Build a key from a blake3 hash, rendered as lowercase hex.
    pub fn from_blake3(hash: blake3::Hash) -> Self {
        Self(hash.to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrefixKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PrefixKey {
    type Err = ConwayError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

/// A fully-qualified model reference: `backend/model`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelRef {
    pub backend: BackendId,
    pub model: ModelId,
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.backend, self.model)
    }
}

impl FromStr for ModelRef {
    type Err = ConwayError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((backend, model)) if !backend.is_empty() && !model.is_empty() => Ok(Self {
                backend: BackendId::new(backend),
                model: ModelId::new(model),
            }),
            _ => Err(ConwayError::Parse {
                detail: format!("ModelRef: expected `backend/model`, got {s:?}"),
            }),
        }
    }
}

/// Monotonic per-session log sequence number.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct LogSeq(pub u64);

impl LogSeq {
    pub const ZERO: LogSeq = LogSeq(0);

    /// The next sequence number.
    pub fn succ(self) -> LogSeq {
        LogSeq(self.0 + 1)
    }
}

impl fmt::Display for LogSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for LogSeq {
    type Err = ConwayError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>().map(Self).map_err(|e| ConwayError::Parse {
            detail: format!("LogSeq: invalid integer {s:?}: {e}"),
        })
    }
}

/// A half-open range of log sequence numbers. `end` is exclusive; `None` means
/// open-ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SeqRange {
    pub start: LogSeq,
    pub end: Option<LogSeq>,
}

impl SeqRange {
    pub fn new(start: LogSeq, end: Option<LogSeq>) -> Self {
        Self { start, end }
    }

    /// The full open-ended range from zero.
    pub fn full() -> Self {
        Self {
            start: LogSeq::ZERO,
            end: None,
        }
    }

    pub fn contains(&self, seq: &LogSeq) -> bool {
        *seq >= self.start
            && match self.end {
                Some(end) => *seq < end,
                None => true,
            }
    }
}

impl fmt::Display for SeqRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.end {
            Some(end) => write!(f, "{}..{}", self.start, end),
            None => write!(f, "{}..", self.start),
        }
    }
}

impl FromStr for SeqRange {
    type Err = ConwayError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (start, end) = s.split_once("..").ok_or_else(|| ConwayError::Parse {
            detail: format!("SeqRange: expected `start..end` or `start..`, got {s:?}"),
        })?;
        let start = start.parse::<LogSeq>()?;
        let end = if end.is_empty() {
            None
        } else {
            Some(end.parse::<LogSeq>()?)
        };
        Ok(Self { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_ids_roundtrip_display_fromstr() {
        let id = SessionId::new();
        let parsed: SessionId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn ids_serialize_transparently() {
        let id = AgentId::new();
        let json = serde_json::to_string(&id).unwrap();
        // A bare JSON string, not an object.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let seq = LogSeq(42);
        assert_eq!(serde_json::to_string(&seq).unwrap(), "42");
    }

    #[test]
    fn model_ref_display_and_parse() {
        let mr: ModelRef = "anthropic/claude-sonnet-4-6".parse().unwrap();
        assert_eq!(mr.backend.as_str(), "anthropic");
        assert_eq!(mr.model.as_str(), "claude-sonnet-4-6");
        assert_eq!(mr.to_string(), "anthropic/claude-sonnet-4-6");
        // Splits on the FIRST slash only.
        let mr2: ModelRef = "local/qwen3/coder".parse().unwrap();
        assert_eq!(mr2.model.as_str(), "qwen3/coder");
        assert!("no-slash".parse::<ModelRef>().is_err());
        assert!("/model".parse::<ModelRef>().is_err());
        assert!("backend/".parse::<ModelRef>().is_err());
    }

    #[test]
    fn log_seq_succ_and_zero() {
        assert_eq!(LogSeq::ZERO.0, 0);
        assert_eq!(LogSeq(41).succ(), LogSeq(42));
    }

    #[test]
    fn seq_range_contains_and_display() {
        let r = SeqRange::new(LogSeq(2), Some(LogSeq(5)));
        assert!(!r.contains(&LogSeq(1)));
        assert!(r.contains(&LogSeq(2)));
        assert!(r.contains(&LogSeq(4)));
        assert!(!r.contains(&LogSeq(5)));
        assert_eq!(r.to_string(), "2..5");
        let open = SeqRange::new(LogSeq(3), None);
        assert!(open.contains(&LogSeq(1_000_000)));
        assert_eq!(open.to_string(), "3..");
        let parsed: SeqRange = "2..5".parse().unwrap();
        assert_eq!(parsed, r);
        let parsed_open: SeqRange = "3..".parse().unwrap();
        assert_eq!(parsed_open, open);
    }

    #[test]
    fn prefix_key_from_blake3_is_lowercase_hex() {
        let key = PrefixKey::from_blake3(blake3::hash(b"conway"));
        assert_eq!(key.as_str().len(), 64);
        assert!(key
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
