//! Core types and coordination primitives shared by `agentd` and its clients.
//!
//! Everything in this crate is pure data and pure logic: no I/O, no async, no
//! clocks. Callers pass `now` explicitly so the state machines are trivially
//! testable and deterministic.

pub mod agent;
pub mod change;
pub mod event;
pub mod journal;
pub mod lease;
pub mod message;
pub mod paths;
pub mod project;
pub mod protocol;
pub mod registry;

pub use agent::{AgentId, AgentRecord, AgentSpec, AgentStatus, DiscoveredProcess, VcsState};
pub use change::{Attribution, Change, ChangeKind, Overlap, OverlapParty, overlaps};
pub use event::{Event, EventKind};
pub use journal::{Digest, DigestBudget, JournalEntry, JournalFilter, JournalKind, SummarySource};
pub use lease::{Claimed, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, ResourceKey};
pub use message::{Destination, Envelope, MessageId, topic_matches};
pub use project::{ProjectId, ProjectRef, ProjectSource};
pub use protocol::DigestRequest;
pub use protocol::{ErrorCode, Request, Response};
pub use registry::{Registry, RegistryError};

pub mod working_set;
pub use working_set::{ReadMark, StalePath};

pub mod recovery;
pub use recovery::{Checkpoint, Recovery, Validation};

pub mod container;
pub use container::{ContainerEngine, ImageBuild, ImageBuildSpec};
pub mod handoff;
pub use handoff::{HandoffBundle, HandoffDiff};
