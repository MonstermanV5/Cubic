//! Protocol-independent presentation and state for Cubic Chat Mode.

use std::collections::VecDeque;

use cubic_core::{ChatConnectionState, ChatEvent, ChatMessageKind};

pub const MAX_HISTORY_MESSAGES: usize = 500;
pub const MAX_HISTORY_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_UTF16_UNITS: usize = 256;

pub trait ChatSessionPort: Send {
    fn try_next_event(&mut self) -> Option<ChatEvent>;
    fn take_critical_event(&mut self) -> Option<ChatEvent>;
    fn dropped_event_count(&mut self) -> usize;
    fn send_message(&mut self, message: String) -> Result<(), String>;
    fn disconnect(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayedMessage {
    pub kind: ChatMessageKind,
    pub sender: Option<String>,
    pub text: String,
}

pub struct ChatModel {
    state: ChatConnectionState,
    history: VecDeque<DisplayedMessage>,
    history_bytes: usize,
    input: String,
}

impl Default for ChatModel {
    fn default() -> Self {
        Self {
            state: ChatConnectionState::Connecting,
            history: VecDeque::new(),
            history_bytes: 0,
            input: String::new(),
        }
    }
}

impl ChatModel {
    #[must_use]
    pub const fn state(&self) -> ChatConnectionState {
        self.state
    }

    #[must_use]
    pub fn history(&self) -> &VecDeque<DisplayedMessage> {
        &self.history
    }

    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn set_input(&mut self, value: String) {
        self.input = value
            .chars()
            .filter(|character| !character.is_control())
            .scan(0_usize, |units, character| {
                let next = units.saturating_add(character.len_utf16());
                (next <= MAX_INPUT_UTF16_UNITS).then(|| {
                    *units = next;
                    character
                })
            })
            .collect();
    }

    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Connected => self.state = ChatConnectionState::Connected,
            ChatEvent::Message {
                kind,
                sender,
                message,
            } => self.push(DisplayedMessage {
                kind,
                sender,
                text: message.plain_text,
            }),
            ChatEvent::Warning(text) => self.push(DisplayedMessage {
                kind: ChatMessageKind::ServerNotice,
                sender: None,
                text,
            }),
            ChatEvent::Disconnected { reason } => {
                self.state = ChatConnectionState::Disconnected;
                self.push(DisplayedMessage {
                    kind: ChatMessageKind::ServerNotice,
                    sender: None,
                    text: format!("Disconnected: {reason}"),
                });
            }
        }
    }

    pub fn take_message_to_send(&mut self) -> Option<String> {
        let trimmed = self.input.trim();
        if trimmed.is_empty() {
            return None;
        }
        let message = trimmed.to_owned();
        self.input.clear();
        Some(message)
    }

    fn push(&mut self, message: DisplayedMessage) {
        let bytes = retained_bytes(&message);
        if bytes > MAX_HISTORY_TEXT_BYTES {
            return;
        }
        self.history_bytes = self.history_bytes.saturating_add(bytes);
        self.history.push_back(message);
        while self.history.len() > MAX_HISTORY_MESSAGES
            || self.history_bytes > MAX_HISTORY_TEXT_BYTES
        {
            let Some(removed) = self.history.pop_front() else {
                break;
            };
            self.history_bytes = self.history_bytes.saturating_sub(retained_bytes(&removed));
        }
    }
}

pub struct ChatMode {
    model: ChatModel,
    port: Box<dyn ChatSessionPort>,
}

impl ChatMode {
    #[must_use]
    pub fn new(port: Box<dyn ChatSessionPort>) -> Self {
        Self {
            model: ChatModel::default(),
            port,
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.port.try_next_event() {
            self.model.apply(event);
            changed = true;
        }
        if let Some(event) = self.port.take_critical_event() {
            self.model.apply(event);
            changed = true;
        }
        let dropped = self.port.dropped_event_count();
        if dropped > 0 {
            self.model.apply(ChatEvent::Warning(format!(
                "{dropped} incoming messages were dropped because the UI queue was full"
            )));
            changed = true;
        }
        changed
    }

    pub fn show(&mut self, root: &mut egui::Ui) {
        let connected = self.model.state() == ChatConnectionState::Connected;
        egui::Panel::top("cubic-chat-header").show(root, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("Cubic");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, color) = match self.model.state() {
                        ChatConnectionState::Connecting => ("Connecting ●", egui::Color32::YELLOW),
                        ChatConnectionState::Connected => {
                            ("Connected ●", egui::Color32::LIGHT_GREEN)
                        }
                        ChatConnectionState::Disconnected => {
                            ("Disconnected ●", egui::Color32::LIGHT_RED)
                        }
                    };
                    ui.colored_label(color, label);
                });
            });
            ui.add_space(8.0);
        });

        egui::Panel::bottom("cubic-chat-input").show(root, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let available = (ui.available_width() - 84.0).max(80.0);
                let response = ui.add_sized(
                    [available, 44.0],
                    egui::TextEdit::singleline(&mut self.model.input)
                        .hint_text("Message…")
                        .desired_width(f32::INFINITY),
                );
                let enter =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let send = ui
                    .add_enabled(
                        connected,
                        egui::Button::new("Send").min_size([72.0, 44.0].into()),
                    )
                    .clicked();
                if connected && (send || enter) {
                    self.send_current();
                    response.request_focus();
                }
            });
            self.model.set_input(self.model.input.clone());
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for message in self.model.history() {
                        ui.horizontal_wrapped(|ui| {
                            match message.kind {
                                ChatMessageKind::Player => {
                                    if let Some(sender) = &message.sender {
                                        ui.strong(format!("<{sender}>"));
                                    }
                                }
                                ChatMessageKind::System => {
                                    ui.colored_label(egui::Color32::LIGHT_BLUE, "Server:");
                                }
                                ChatMessageKind::ServerNotice => {
                                    ui.colored_label(egui::Color32::YELLOW, "Notice:");
                                }
                            }
                            ui.label(&message.text);
                        });
                        ui.add_space(4.0);
                    }
                });
        });
    }

    pub fn disconnect(&mut self) {
        self.port.disconnect();
    }

    fn send_current(&mut self) {
        let Some(message) = self.model.take_message_to_send() else {
            return;
        };
        if let Err(error) = self.port.send_message(message) {
            self.model.apply(ChatEvent::Warning(error));
        }
    }
}

fn retained_bytes(message: &DisplayedMessage) -> usize {
    message
        .sender
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(message.text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_core::{ChatMessage, StructuredText};

    fn message(text: &str) -> ChatEvent {
        ChatEvent::Message {
            kind: ChatMessageKind::System,
            sender: None,
            message: ChatMessage {
                plain_text: text.to_owned(),
                structured: StructuredText::String(text.to_owned()),
            },
        }
    }

    #[test]
    fn history_evicts_oldest_messages_deterministically() {
        let mut model = ChatModel::default();
        for index in 0..=MAX_HISTORY_MESSAGES {
            model.apply(message(&index.to_string()));
        }
        assert_eq!(model.history().len(), MAX_HISTORY_MESSAGES);
        assert_eq!(
            model.history().front().map(|entry| entry.text.as_str()),
            Some("1")
        );
    }

    #[test]
    fn input_is_bounded_and_control_characters_are_removed() {
        let mut model = ChatModel::default();
        model.set_input(format!("a\n{}", "😀".repeat(200)));
        assert!(!model.input().contains('\n'));
        assert!(model.input().encode_utf16().count() <= MAX_INPUT_UTF16_UNITS);
    }

    #[test]
    fn send_action_rejects_empty_and_clears_nonempty_input() {
        let mut model = ChatModel::default();
        model.set_input("   ".to_owned());
        assert_eq!(model.take_message_to_send(), None);
        model.set_input(" hello ".to_owned());
        assert_eq!(model.take_message_to_send().as_deref(), Some("hello"));
        assert!(model.input().is_empty());
    }

    #[test]
    fn connection_transitions_and_disconnect_reason_are_visible() {
        let mut model = ChatModel::default();
        model.apply(ChatEvent::Connected);
        assert_eq!(model.state(), ChatConnectionState::Connected);
        model.apply(ChatEvent::Disconnected {
            reason: "bye".to_owned(),
        });
        assert_eq!(model.state(), ChatConnectionState::Disconnected);
        assert_eq!(
            model.history().back().map(|entry| entry.text.as_str()),
            Some("Disconnected: bye")
        );
    }

    #[test]
    fn common_unicode_is_preserved_in_input_and_history() {
        const SAMPLE: &str = "£ € café Привет 😄 漢字";
        let mut model = ChatModel::default();
        model.set_input(SAMPLE.to_owned());
        assert_eq!(model.input(), SAMPLE);
        model.apply(message(SAMPLE));
        assert_eq!(
            model.history().back().map(|entry| entry.text.as_str()),
            Some(SAMPLE)
        );
    }
}
