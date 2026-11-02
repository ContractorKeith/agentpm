use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

// Separate ID types prevent accidentally mixing durable entity identities.
macro_rules! ids {
    ($($name:ident),+) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl TryFrom<String> for $name { type Error = String; fn try_from(value: String) -> Result<Self, String> { Self::new(value) } }
        impl From<$name> for String { fn from(value: $name) -> Self { value.0 } }
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.is_empty() || value.len() > 128 || !value.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_:.".contains(&c)) {
                    return Err("ID must contain 1..128 ASCII identifier characters".into());
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
    )+};
}
ids!(
    ProjectId,
    RunId,
    MilestoneId,
    TaskAttemptId,
    ContextPackageId,
    ArtifactId,
    BackendSessionId,
    WorktreeId,
    PRBindingId,
    EvidenceId,
    AttentionItemId,
    HumanDecisionId,
    ExternalOperationId,
    RequestId
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Usage {
    Unknown {},
    Reported { tokens: u64, source: String },
    Estimated { tokens: u64, source: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionIdentity {
    pub head: String,
    pub base: String,
    pub prerequisite_heads: Vec<String>,
    pub context: ContextPackageId,
    pub check_configuration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub protocol_version: u32,
    pub id: EvidenceId,
    pub revision: RevisionIdentity,
    pub implementer: BackendSessionId,
    pub reviewer: BackendSessionId,
    pub checks_passed: bool,
    pub ci_passed: bool,
    pub review_passed: bool,
}
impl Evidence {
    pub fn qualifies(&self, revision: &RevisionIdentity) -> bool {
        self.protocol_version == PROTOCOL_VERSION
            && self.revision == *revision
            && self.implementer != self.reviewer
            && self.checks_passed
            && self.ci_passed
            && self.review_passed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Waiting,
    Paused,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneState {
    Queued,
    Implementing,
    Checking,
    Reviewing,
    Repairing,
    StaleBase,
    Ready,
    HumanOwned,
    Waiting,
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl RunState {
    pub fn transition(&mut self, next: Self, deliveries_verified: bool) -> Result<(), String> {
        use RunState::*;
        let allowed = matches!(
            (*self, next),
            (Queued, Running | Canceled)
                | (Running, Waiting | Paused | Failed | Canceled)
                | (Waiting | Paused, Running | Canceled)
        );
        if !(allowed || *self == Running && next == Completed && deliveries_verified) {
            return Err("invalid run transition or missing delivery evidence".into());
        }
        *self = next;
        Ok(())
    }
}

impl MilestoneState {
    pub fn transition(
        &mut self,
        next: Self,
        evidence: Option<&Evidence>,
        revision: &RevisionIdentity,
    ) -> Result<(), String> {
        use MilestoneState::*;
        let allowed = matches!(
            (*self, next),
            (Queued, Implementing | Canceled)
                | (
                    Implementing,
                    Checking | Waiting | Paused | Failed | Canceled | StaleBase | HumanOwned
                )
                | (
                    Checking,
                    Reviewing | Repairing | Failed | Canceled | StaleBase | HumanOwned
                )
                | (
                    Reviewing,
                    Repairing | Failed | Canceled | StaleBase | HumanOwned
                )
                | (
                    Repairing,
                    Checking | Failed | Canceled | StaleBase | HumanOwned
                )
                | (StaleBase, Implementing | Repairing | HumanOwned | Canceled)
                | (Waiting | Paused, Implementing | Canceled | HumanOwned)
                | (HumanOwned, StaleBase)
                | (Ready, StaleBase | HumanOwned | Canceled)
        );
        let gated = matches!((*self, next), (Reviewing, Ready) | (Ready, Completed))
            && evidence.is_some_and(|e| e.qualifies(revision));
        if !allowed && !gated {
            return Err("invalid milestone transition or stale/missing evidence".into());
        }
        *self = next;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    ExerciseFake { script: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub command: Command,
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("unsupported protocol version".into());
        }
        RequestId::new(self.request_id.as_str())?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Started {},
    Delayed { milliseconds: u64 },
    Blocked { reason: String },
    Succeeded { usage: Usage },
    Failed { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub sequence: u64,
    pub logical_milliseconds: u64,
    pub event: Event,
}
