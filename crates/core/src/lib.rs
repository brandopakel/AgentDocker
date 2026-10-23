//! Core types and coordination primitives shared by `agentd` and its clients.
//!
//! Everything in this crate is pure data and pure logic: no I/O, no async, no
//! clocks. Callers pass `now` explicitly so the state machines are trivially
//! testable and deterministic.

pub mod agent;
pub mod board;
pub mod change;
pub mod config;
pub mod conversation;
pub use conversation::{
    ArchivedMessage, ConversationId, ConversationKind, ConversationSummary, ReadCursor,
};
pub mod elicitation;
pub mod event;
pub use elicitation::{FormField, FormKind, McpForm};
pub mod identity;
pub mod input;
pub use input::{
    AdapterContact, AdapterKind, CONTROLLER_BIND_GRACE, CONTROLLER_KILL_AFTER, CONTROLLER_RESTARTS,
    CONTROLLER_STABLE, ControllerLaunch, ControllerRestart, ControllerStep, InputBinding,
    InputDelivery, InputReadiness, InputReceipt, InputReport, ProcessIdentity, ProviderGeneration,
    ReceivedInput, controller_backoff,
};
pub mod journal;
pub mod judgment;
pub mod lease;
pub mod link;
pub mod memory;
pub mod message;
pub mod model_choice;
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
pub mod secret;
pub mod seed;

pub use agent::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, DiscoveredProcess, RestartPolicy, SessionContact,
    SessionKey, VcsState,
};
pub use board::{Board, Lane};
pub mod plan;
pub use plan::{Plan, PlanStatus, ProjectFile};
pub mod rule;
pub use rule::{Rule, RuleAction, Trigger};
pub mod role;
pub mod search;
pub mod tag;
pub use change::{Attribution, Change, ChangeKind, Overlap, OverlapParty, overlaps};
pub use event::{Event, EventCursor, EventKind, WaitOutcome};
pub use journal::{Digest, DigestBudget, JournalEntry, JournalFilter, JournalKind, SummarySource};
pub use lease::{Claimed, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, ResourceKey};
pub use link::{Link, LinkKind};
pub use message::{
    ALLOW_FOR_SESSION, AnswerRoute, Destination, Envelope, HUMAN, HUMAN_RUNTIME, MessageId,
    Question, QuestionFileChange, QuestionFileChangeKind, QuestionOption, QuestionPresentation,
    mcp_url_authority, topic_matches,
};
pub use notification::NotificationTarget;
pub use permissions::{
    QuestionFileSystemPermissions, QuestionNetworkPermissions, QuestionPermissionAccess,
    QuestionPermissionEntry, QuestionPermissionPath, QuestionPermissions,
};
pub use project::{Pause, ProjectId, ProjectRef, ProjectSource};
pub use protocol::DigestRequest;
pub use protocol::{ErrorCode, Request, Response};
pub use registry::{Registry, RegistryError};
pub use role::{RoleColor, RoleDef};
pub use tag::{Tag, TagColor};
pub use task::{
    Column, Comment, CommentId, NewTask, Priority, Size, Task, TaskAction, TaskEntry, TaskId,
};

pub mod policy;
pub use policy::{Policy, Ruling};

pub mod editor;
pub mod multiplexer;
pub mod terminal;

pub mod contest;
pub use contest::{Contest, ContestId, Entry, Measure, Metric, Standing};

pub mod wait;
pub use wait::{
    Activity, ActivityObservation, AgentActivity, Blocked, Judged, ReportedActivity, WaitQueue,
    WaitReason, Waiter,
};

pub mod standing;
pub mod working_set;
pub use working_set::{ReadMark, StalePath};

pub mod delivery;
pub mod gate;
pub mod readiness;
pub mod recovery;
pub mod review;
pub mod send_readiness;
pub mod session;
pub use recovery::{Checkpoint, Recovery, Validation};
pub use send_readiness::{RecipientReadiness, SendIssue, SendReadiness};

pub mod handoff;
pub use handoff::{HandoffBundle, HandoffDiff};

pub mod runtime;
pub mod usage;
pub use runtime::{RuntimeInfo, Wiring};
pub mod webhook;
pub use webhook::{AttemptResult, WebhookAttempt};

pub mod channel;
pub use channel::{Channel, ChannelId, ChannelSubject, Decision, Review, Verdict};
pub mod container;
pub use container::{ContainerEngine, ImageBuild, ImageBuildSpec};
