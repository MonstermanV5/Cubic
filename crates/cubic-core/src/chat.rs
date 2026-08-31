use std::collections::BTreeMap;

/// A bounded, protocol-independent representation of rich server text.
#[derive(Clone, Debug, PartialEq)]
pub enum StructuredText {
    String(String),
    Number(f64),
    Boolean(bool),
    List(Vec<Self>),
    Compound(BTreeMap<String, Self>),
    Unsupported,
}

/// Text retained for Chat Mode, including its readable projection.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub plain_text: String,
    pub structured: StructuredText,
    pub trust: ChatMessageTrust,
}

/// Protocol-independent provenance without exposing signatures to the UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatMessageTrust {
    NotApplicable,
    Unsigned,
    SignedUnverified,
    Modified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatMessageKind {
    Player,
    System,
    ServerNotice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

/// Presentation workload selected for one still-live Minecraft session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionPresentationMode {
    Play,
    #[default]
    Chat,
}

/// Safety conditions surfaced while the three-dimensional world is hidden.
/// Platform code may later map these semantic kinds to native haptics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafetyAlertKind {
    Damage,
    LowHealth,
    Drowning,
    Death,
    LargeDisplacement,
}

/// Events delivered by a persistent Minecraft session without exposing wire packets.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    Connected,
    Message {
        kind: ChatMessageKind,
        sender: Option<String>,
        message: ChatMessage,
    },
    Warning(String),
    SafetyAlert {
        kind: SafetyAlertKind,
        message: String,
    },
    Disconnected {
        reason: String,
    },
}

/// Commands accepted by the network-owned session task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatSessionCommand {
    SendMessage(String),
    Disconnect,
}
