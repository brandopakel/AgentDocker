//! Core types and coordination primitives shared by `agentd` and its clients.
//!
//! Everything in this crate is pure data and pure logic: no I/O, no async, no
//! clocks. Callers pass `now` explicitly so the state machines are trivially
//! testable and deterministic.

pub mod agent;
pub mod change;
pub mod config;
pub mod conversation;
pub use conversation::{
    ArchivedMessage, ConversationId, ConversationKind, ConversationSummary, ReadCursor,
};
pub mod event;
pub mod identity;
pub mod input;
pub use input::{
    AdapterContact, AdapterKind, CONTROLLER_BIND_GRACE, CONTROLLER_KILL_AFTER, CONTROLLER_RESTARTS,
    CONTROLLER_STABLE, ControllerLaunch, ControllerRestart, ControllerStep, InputBinding,
    InputDelivery, InputReadiness, InputReceipt, InputReport, ProcessIdentity, ProviderGeneration,
    ReceivedInput, controller_backoff,
};
pub mod journal;
pub mod lease;
pub mod message;
pub mod notification;
pub mod paths;
pub mod permissions;
pub mod project;
pub mod protocol;
pub mod provider;
pub mod task;
pub use provider::{
    ProviderAvailability, ProviderIssue, ProviderIssueKind, ProviderReport, provider_block,
};
pub mod registry;

pub use agent::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, DiscoveredProcess, RestartPolicy, VcsState,
};
pub use change::{Attribution, Change, ChangeKind, Overlap, OverlapParty, overlaps};
pub use event::{Event, EventCursor, EventKind, WaitOutcome};
pub use journal::{Digest, DigestBudget, JournalEntry, JournalFilter, JournalKind, SummarySource};
pub use lease::{Claimed, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, ResourceKey};
pub use message::{
    AnswerRoute, Destination, Envelope, HUMAN, HUMAN_RUNTIME, MessageId, Question,
    QuestionFileChange, QuestionFileChangeKind, QuestionOption, QuestionPresentation,
    topic_matches,
};
pub use notification::NotificationTarget;
pub use permissions::{
    QuestionFileSystemPermissions, QuestionNetworkPermissions, QuestionPermissionAccess,
    QuestionPermissionEntry, QuestionPermissionPath, QuestionPermissions,
};
pub use project::{ProjectId, ProjectRef, ProjectSource};
pub use protocol::DigestRequest;
pub use protocol::{ErrorCode, Request, Response};
pub use registry::{Registry, RegistryError};
pub use task::{Column, Task, TaskId};

pub mod policy;
pub use policy::{Policy, Ruling};

pub mod multiplexer;

pub mod contest;
pub use contest::{Contest, ContestId, Entry, Measure, Metric, Standing};

pub mod wait;
pub use wait::{
    Activity, ActivityObservation, AgentActivity, Blocked, ReportedActivity, WaitQueue, Waiter,
};

pub mod working_set;
pub use working_set::{ReadMark, StalePath};

pub mod recovery;
pub mod session;
pub use recovery::{Checkpoint, Recovery, Validation};

pub mod handoff;
pub use handoff::{HandoffBundle, HandoffDiff};

pub mod runtime;
pub mod usage;
pub use runtime::{RuntimeInfo, Wiring};

pub mod channel;
pub use channel::{Channel, ChannelId, ChannelSubject, Decision, Review, Verdict};
pub mod container;
pub use container::{ContainerEngine, ImageBuild, ImageBuildSpec};
