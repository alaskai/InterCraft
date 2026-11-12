// Built-in chat panel. The conversation runs on a background thread with its own
// tool host, which edits the project files. The viewer picks those edits up from
// disk, the same way it does for an external MCP client.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui;
use intercraft_llm::{
    claude_code_version, AnthropicChat, AnthropicSettings, Cancelled, ChatEvent, ChatSession,
    ClaudeCodeChat, ClaudeCodeSettings, OpenAiChat, OpenAiSettings, ToolDef, ToolExecutor, Usage,
    ANTHROPIC_MODELS, CLAUDE_CODE_MODELS, OPENAI_PRESETS,
};
use intercraft_tools::ToolHost;
use serde_json::Value;

use crate::settings::{self, KeySource, Provider, Settings, EFFORTS};

enum Command {
    Send { text: String, images: Vec<String> },
    Reset,
    Configure(Config),
}

/// Everything the worker needs to talk to either provider.
#[derive(Clone)]
struct Config {
    provider: Provider,
    anthropic: AnthropicSettings,
    claude_code: ClaudeCodeSettings,
    openai: OpenAiSettings,
}

/// Said to the model in Claude Code sessions, on top of the MCP server's own guide.
const CLAUDE_CODE_PROMPT: &str = "You are running inside the InterCraft desktop app's chat panel. \
The user watches the 3D viewer update after each tool call. Only the InterCraft tools are available. \
Keep replies short and give measured numbers from the build reports.";

enum Reply {
    Event(ChatEvent),
    Failed(String),
    Stopped,
}

enum Message {
    /// The text, and whether a sketch went with it.
    User(String, bool),
    Assistant { model: String, items: Vec<Item> },
    Notice(String),
    Error(String),
}

enum Item {
    Text(String),
    Note(String),
    Tool(ToolCall),
}

struct ToolCall {
    id: String,
    name: String,
    input: Value,
    /// None while running.
    result: Option<(bool, Value)>,
    /// For ask_user: what the user picked or typed.
    answer: Option<String>,
    /// For ask_user: the "Other" text box and the multi-select ticks.
    other: String,
    picks: Vec<bool>,
}

pub struct ChatPanel {
    project_dir: PathBuf,
    settings: Settings,
    api_key: String,
    key_source: KeySource,
    key_input: String,
    /// Key for the OpenAI-compatible server currently set, if one is saved.
    server_key: String,
    /// Set when the user asks for the Settings window from the chat panel.
    pub wants_settings: bool,
    settings_error: Option<String>,
    /// Result of `claude --version`, checked when Claude Code is chosen.
    claude_code: Option<Option<String>>,

    worker: Option<(Sender<Command>, Receiver<Reply>)>,
    cancel: Arc<AtomicBool>,
    busy: bool,
    messages: Vec<Message>,
    markdown: egui_commonmark::CommonMarkCache,
    input: String,
    /// A sketch waiting to go with the next message: base64 PNG and its written summary.
    sketch: Option<(String, String)>,
    usage: Usage,
    ctx: egui::Context,
}

impl ChatPanel {
    pub fn new(ctx: egui::Context, project_dir: PathBuf) -> Self {
        let (api_key, key_source) = settings::load_api_key();
        let settings = Settings::load();
        let server_key = match settings.provider {
            Provider::OpenAi => settings::load_server_key(&settings.openai_base_url),
            _ => String::new(),
        };
        let claude_code = (settings.provider == Provider::ClaudeCode).then(claude_code_version);
        let panel = Self {
            project_dir,
            settings,
            wants_settings: false,
            api_key,
            key_source,
            key_input: String::new(),
            server_key,
            settings_error: None,
            claude_code,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: false,
            messages: Vec::new(),
            markdown: Default::default(),
            input: String::new(),
            sketch: None,
            usage: Usage::default(),
            ctx,
        };
        panel
    }

    /// Whether the chosen provider can take a message right now.
    fn ready(&self) -> bool {
        match self.settings.provider {
            Provider::Anthropic => !self.api_key.is_empty(),
            Provider::ClaudeCode => matches!(self.claude_code, Some(Some(_))),
            // A local server needs no key, so the address and model are enough.
            Provider::OpenAi => {
                intercraft_llm::completions_url(&self.settings.openai_base_url).is_ok()
                    && !self.settings.openai_model.trim().is_empty()
            }
        }
    }

    fn config(&self) -> Config {
        // The Claude Code session starts its own InterCraft MCP server, pinned to this project.
        let launcher = std::path::absolute(concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/mcp_launcher.py"))
            .unwrap_or_default();
        Config {
            provider: self.settings.provider,
            anthropic: AnthropicSettings {
                api_key: self.api_key.clone(),
                model: self.settings.model.clone(),
                effort: self.settings.effort.clone(),
                system: intercraft_tools::GUIDE.to_owned(),
            },
            claude_code: ClaudeCodeSettings {
                project_dir: self.project_dir.clone(),
                mcp_command: "python".to_owned(),
                mcp_args: vec![
                    launcher.to_string_lossy().into_owned(),
                    self.project_dir.to_string_lossy().into_owned(),
                    // Adds ask_user, which only this chat panel can show.
                    "--chat-panel".to_owned(),
                ],
                model: self.settings.claude_code_model.clone(),
                extra_prompt: CLAUDE_CODE_PROMPT.to_owned(),
            },
            openai: OpenAiSettings {
                base_url: self.settings.openai_base_url.clone(),
                // Looked up for this exact address every time, so a key can never go
                // to a server other than the one it was saved for.
                api_key: match self.settings.provider {
                    Provider::OpenAi => settings::load_server_key(&self.settings.openai_base_url),
                    _ => String::new(),
                },
                model: self.settings.openai_model.trim().to_owned(),
                system: intercraft_tools::GUIDE.to_owned(),
                vision: self.settings.openai_vision,
            },
        }
    }

    fn worker(&mut self) -> &Sender<Command> {
        if self.worker.is_none() {
            let (cmd_tx, cmd_rx) = channel();
            let (reply_tx, reply_rx) = channel();
            let ctx = self.ctx.clone();
            let cancel = self.cancel.clone();
            let config = self.config();
            let dir = self.project_dir.clone();
            std::thread::spawn(move || run_worker(dir, config, cmd_rx, reply_tx, cancel, ctx));
            self.worker = Some((cmd_tx, reply_rx));
        }
        &self.worker.as_ref().expect("worker was just started").0
    }

    fn push_settings(&mut self) {
        let mine = self.settings.clone();
        let saved = Settings::update(|s| {
            s.provider = mine.provider;
            s.model = mine.model.clone();
            s.effort = mine.effort.clone();
            s.claude_code_model = mine.claude_code_model.clone();
            s.openai_base_url = mine.openai_base_url.clone();
            s.openai_model = mine.openai_model.clone();
            s.openai_vision = mine.openai_vision;
        });
        if let Err(e) = saved {
            self.settings_error = Some(format!("Couldn't save settings: {e:#}"));
        }
        if self.worker.is_some() {
            let config = self.config();
            let _ = self.worker().send(Command::Configure(config));
        }
    }

    /// Start fresh on another project: stop any reply, forget the conversation, and
    /// point the tools at the new folder.
    pub fn set_project(&mut self, dir: PathBuf, name: &str) {
        self.cancel.store(true, Ordering::Relaxed);
        // The old worker thread ends once its reply stops and its channel closes.
        self.worker = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = false;
        self.project_dir = dir;
        self.messages.clear();
        self.usage = Usage::default();
        self.messages.push(Message::Notice(format!("Opened {name}. This is a new conversation.")));
    }

    #[cfg(test)]
    pub fn use_server(&mut self, base_url: &str, model: &str) {
        self.settings.provider = Provider::OpenAi;
        self.settings.openai_base_url = base_url.to_owned();
        self.settings.openai_model = model.to_owned();
    }

    /// Hold a sketch to go with the next message. It replaces one already waiting.
    pub fn attach_sketch(&mut self, png_base64: String, summary: String) {
        self.sketch = Some((png_base64, summary));
    }

    /// The sketch waiting to be sent, as (base64 PNG, summary).
    #[cfg(test)]
    pub fn attached_sketch(&self) -> Option<&(String, String)> {
        self.sketch.as_ref()
    }

    /// A reply is being written, so nothing new can be sent or attached yet.
    /// Whether a question can be sent right now: a provider is set up and no reply is running.
    pub fn can_ask(&self) -> bool {
        self.ready() && !self.busy
    }

    /// Send a question on the user's behalf, as if they had typed it. Returns false
    /// when it can't be sent yet.
    pub fn ask(&mut self, text: &str) -> bool {
        if !self.can_ask() {
            return false;
        }
        self.send_text(text.to_owned(), None);
        true
    }

    /// The newest message the user sent, for tests.
    #[cfg(test)]
    pub fn last_question(&self) -> Option<&str> {
        self.messages.iter().rev().find_map(|m| if let Message::User(text, _) = m { Some(text.as_str()) } else { None })
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// One line describing the chat setup, for the panel header.
    pub fn summary(&self) -> String {
        match self.settings.provider {
            Provider::Anthropic => {
                let model = ANTHROPIC_MODELS
                    .iter()
                    .find(|(id, _)| *id == self.settings.model)
                    .map_or(self.settings.model.as_str(), |(_, name)| name);
                format!("{model} · effort {}", self.settings.effort)
            }
            Provider::ClaudeCode => match self.settings.claude_code_model.as_str() {
                "" => "Claude Code · default model".to_owned(),
                alias => format!("Claude Code · {}", claude_code_model_name(alias)),
            },
            Provider::OpenAi => {
                let url = &self.settings.openai_base_url;
                let server = OPENAI_PRESETS.iter().find(|(_, preset)| preset == url).map_or(url.as_str(), |(name, _)| name);
                match self.settings.openai_model.trim() {
                    "" => format!("{server} · no model chosen"),
                    model => format!("{model} · {server}"),
                }
            }
        }
    }

    /// `context` is a short note about what the user is looking at, sent with their message.
    fn send(&mut self, context: Option<String>) {
        let text = self.input.trim().to_owned();
        if (text.is_empty() && self.sketch.is_none()) || self.busy || !self.ready() {
            return;
        }
        self.input.clear();
        self.send_text(text, context);
    }

    fn send_text(&mut self, text: String, context: Option<String>) {
        if self.busy || !self.ready() {
            return;
        }
        let sketch = self.sketch.take();
        self.messages.push(Message::User(text.clone(), sketch.is_some()));
        let mut full = text;
        let mut images = Vec::new();
        if let Some((png, summary)) = sketch {
            // The summary goes along in words, so the labels and numbers survive even
            // if the model reads the picture badly or can't read pictures at all.
            let said = if full.is_empty() { "See my sketch.".to_owned() } else { full };
            full = format!("{said}\n\n[Attached sketch]\n{summary}");
            images.push(png);
        }
        if let Some(context) = context {
            full = format!("{full}\n\n[{context}]");
        }
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        let _ = self.worker().send(Command::Send { text: full, images });
    }

    /// Drain replies from the worker. Returns the part the model last touched, so the
    /// viewer can follow it.
    pub fn poll(&mut self) -> Option<String> {
        let mut focus = None;
        let Some((_, rx)) = &self.worker else { return None };
        let replies: Vec<Reply> = rx.try_iter().collect();
        for reply in replies {
            match reply {
                Reply::Event(event) => {
                    if let Some(part) = self.apply_event(event) {
                        focus = Some(part);
                    }
                }
                Reply::Failed(error) => {
                    self.messages.push(Message::Error(error));
                    self.busy = false;
                }
                Reply::Stopped => {
                    self.messages.push(Message::Notice("Stopped.".into()));
                    self.busy = false;
                }
            }
        }
        focus
    }

    fn apply_event(&mut self, event: ChatEvent) -> Option<String> {
        if let ChatEvent::ReplyStarted { model } = &event {
            match self.messages.last_mut() {
                Some(Message::Assistant { model: current, .. }) => {
                    if current.is_empty() {
                        *current = model.clone();
                    }
                }
                _ => self.messages.push(Message::Assistant { model: model.clone(), items: Vec::new() }),
            }
            return None;
        }
        if let ChatEvent::Done = event {
            self.busy = false;
            return None;
        }
        if let ChatEvent::Usage(usage) = event {
            self.usage = usage;
            return None;
        }
        if let ChatEvent::Notice(text) = event {
            self.messages.push(Message::Notice(text));
            return None;
        }

        // Everything else belongs to the current assistant reply.
        if !matches!(self.messages.last(), Some(Message::Assistant { .. })) {
            self.messages.push(Message::Assistant { model: String::new(), items: Vec::new() });
        }
        let Some(Message::Assistant { items, .. }) = self.messages.last_mut() else { return None };
        match event {
            ChatEvent::TextDelta(text) => match items.last_mut() {
                Some(Item::Text(current)) => current.push_str(&text),
                _ => items.push(Item::Text(text)),
            },
            ChatEvent::NoteDelta(text) => match items.last_mut() {
                Some(Item::Note(current)) => current.push_str(&text),
                _ => items.push(Item::Note(text)),
            },
            ChatEvent::ToolStarted { id, name, input } => {
                items.push(Item::Tool(ToolCall {
                    id,
                    name,
                    input,
                    result: None,
                    answer: None,
                    other: String::new(),
                    picks: Vec::new(),
                }));
            }
            ChatEvent::ToolFinished { id, ok, output } => {
                for item in items.iter_mut().rev() {
                    if let Item::Tool(call) = item {
                        if call.id == id {
                            let part = call.input["part"].as_str().map(str::to_owned);
                            call.result = Some((ok, output));
                            return part;
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, context: Option<String>) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(self.summary()).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Settings").clicked() {
                    self.wants_settings = true;
                }
            });
        });
        ui.separator();

        // Input sits at the bottom, the transcript fills the rest.
        egui::Panel::bottom("chat_input").show(ui, |ui| {
            ui.add_space(4.0);
            // Take Enter before the text box sees it, but only while the box has focus.
            let input_id = ui.make_persistent_id("chat_input_box");
            let enter = ui.memory(|m| m.has_focus(input_id))
                && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    .id(input_id)
                    .hint_text("Describe a part or a change. Enter sends, Shift+Enter for a new line.")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter)),
            );
            ui.horizontal(|ui| {
                if self.busy {
                    ui.spinner();
                    if ui.button("Stop").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                } else {
                    let can_send = (!self.input.trim().is_empty() || self.sketch.is_some()) && self.ready();
                    if ui.add_enabled(can_send, egui::Button::new("Send")).clicked() || (enter && can_send) {
                        self.send(context.clone());
                    }
                }
                if ui.add_enabled(!self.busy, egui::Button::new("New chat")).clicked() {
                    self.messages.clear();
                    self.usage = Usage::default();
                    if self.worker.is_some() {
                        let _ = self.worker().send(Command::Reset);
                    }
                }
                // On the same line as the buttons, so attaching never pushes them out of view.
                if self.sketch.is_some() {
                    ui.separator();
                    ui.label("Sketch attached").on_hover_text("It goes with your next message, as a picture and a written summary");
                    if ui.small_button("Remove").clicked() {
                        self.sketch = None;
                    }
                }
            });
            if self.usage.input_tokens + self.usage.cache_read_tokens > 0 {
                ui.weak(format!(
                    "Last request: {}k tokens, {}k from cache",
                    (self.usage.input_tokens + self.usage.cache_read_tokens + self.usage.cache_write_tokens) / 1000,
                    self.usage.cache_read_tokens / 1000
                ))
                .on_hover_text("Size of the last request and how much of it came from the prompt cache.");
            }
            ui.add_space(4.0);
        });

        let mut answered = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    if self.settings.provider == Provider::Anthropic && !self.ready() {
                        subscription_help(ui);
                    } else if self.settings.provider == Provider::OpenAi && !self.ready() {
                        ui.weak("Open Settings and set the server address and a model. Local servers (Ollama, LM Studio) need no API key.");
                    } else {
                        ui.weak("Ask for a part, for example: \"Make a 60 x 40 x 5 mm plate with four M4 holes 6 mm in from the corners.\"");
                    }
                }
                // Questions can only be answered on the newest reply, once it has finished.
                let newest = self.messages.iter().rposition(|m| matches!(m, Message::Assistant { .. }));
                let open = newest.filter(|&i| {
                    !self.busy && !self.messages[i + 1..].iter().any(|m| matches!(m, Message::User(..)))
                });
                for (i, message) in self.messages.iter_mut().enumerate() {
                    answered |= message_ui(ui, message, &mut self.markdown, open == Some(i));
                    ui.add_space(6.0);
                }
            });

        // Once every question in the reply has an answer, send them together.
        if answered {
            if let Some(reply) = self.answers_ready() {
                self.send_text(reply, context);
            }
        }
    }

    /// The answers to the newest reply's questions, if all of them are answered.
    fn answers_ready(&self) -> Option<String> {
        let Some(Message::Assistant { items, .. }) =
            self.messages.iter().rev().find(|m| matches!(m, Message::Assistant { .. }))
        else {
            return None;
        };
        let questions: Vec<&ToolCall> = items
            .iter()
            .filter_map(|item| match item {
                Item::Tool(call) if is_question(call) => Some(call),
                _ => None,
            })
            .collect();
        if questions.is_empty() || questions.iter().any(|q| q.answer.is_none()) {
            return None;
        }
        let lines: Vec<String> = questions
            .iter()
            .map(|q| {
                let question = q.input["question"].as_str().unwrap_or_default();
                format!("{question} {}", q.answer.as_deref().unwrap_or_default())
            })
            .collect();
        Some(lines.join("\n"))
    }

    /// The chat part of the Settings window.
    pub fn settings_contents(&mut self, ui: &mut egui::Ui) {
        let model_name = ANTHROPIC_MODELS
            .iter()
            .find(|(id, _)| *id == self.settings.model)
            .map_or(self.settings.model.clone(), |(_, name)| (*name).to_owned());
        {
            {
                let mut changed = false;
                egui::Grid::new("chat_settings_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Use");
                    let before = self.settings.provider;
                    egui::ComboBox::from_id_salt("provider")
                        .selected_text(provider_name(self.settings.provider))
                        .show_ui(ui, |ui| {
                            for provider in [Provider::Anthropic, Provider::ClaudeCode, Provider::OpenAi] {
                                ui.selectable_value(&mut self.settings.provider, provider, provider_name(provider));
                            }
                        });
                    ui.end_row();
                    if self.settings.provider != before {
                        changed = true;
                        if self.settings.provider == Provider::ClaudeCode {
                            self.claude_code = Some(claude_code_version());
                        }
                        if self.settings.provider == Provider::OpenAi {
                            self.server_key = settings::load_server_key(&self.settings.openai_base_url);
                        }
                        self.messages.push(Message::Notice(format!(
                            "Now using {}. It starts its own conversation and doesn't see the messages above.",
                            provider_name(self.settings.provider)
                        )));
                    }

                    if self.settings.provider == Provider::ClaudeCode {
                        ui.label("Model");
                        egui::ComboBox::from_id_salt("cc_model")
                            .selected_text(claude_code_model_name(&self.settings.claude_code_model))
                            .show_ui(ui, |ui| {
                                for (alias, name) in CLAUDE_CODE_MODELS {
                                    changed |= ui
                                        .selectable_value(&mut self.settings.claude_code_model, (*alias).to_owned(), *name)
                                        .changed();
                                }
                            });
                        ui.end_row();

                        ui.label("Status");
                        ui.vertical(|ui| {
                            match &self.claude_code {
                                Some(Some(version)) => {
                                    ui.weak(format!("Found {version}. Uses your own Claude sign-in."));
                                }
                                _ => {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        "Claude Code wasn't found. Install it, run claude once in a terminal to sign in, then check again.",
                                    );
                                    if ui.button("Check again").clicked() {
                                        self.claude_code = Some(claude_code_version());
                                    }
                                }
                            }
                        });
                        ui.end_row();
                        return;
                    }

                    if self.settings.provider == Provider::OpenAi {
                        changed |= self.server_settings(ui);
                        return;
                    }

                    ui.label("Model");
                    egui::ComboBox::from_id_salt("model").selected_text(model_name.as_str()).show_ui(ui, |ui| {
                        for (id, name) in ANTHROPIC_MODELS {
                            changed |= ui.selectable_value(&mut self.settings.model, (*id).to_owned(), *name).changed();
                        }
                    });
                    ui.end_row();

                    ui.label("Effort");
                    egui::ComboBox::from_id_salt("effort").selected_text(&self.settings.effort).show_ui(ui, |ui| {
                        for effort in EFFORTS {
                            changed |= ui.selectable_value(&mut self.settings.effort, (*effort).to_owned(), *effort).changed();
                        }
                    })
                    .response
                    .on_hover_text("Higher effort thinks longer and costs more. High suits most modelling.");
                    ui.end_row();

                    ui.label("API key");
                    ui.vertical(|ui| {
                        ui.weak(match self.key_source {
                            KeySource::Keyring => "Saved in the system credential store.",
                            KeySource::Environment => "Read from ANTHROPIC_API_KEY.",
                            KeySource::Missing => "Not set. Get one at console.anthropic.com.",
                        });
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.key_input)
                                    .password(true)
                                    .hint_text("sk-ant-...")
                                    .desired_width((ui.available_width() - 70.0).max(60.0)),
                            );
                            if ui.add_enabled(!self.key_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
                                match settings::save_api_key(&self.key_input) {
                                    Ok(()) => {
                                        self.key_input.clear();
                                        self.settings_error = None;
                                        changed = true;
                                    }
                                    Err(e) => self.settings_error = Some(format!("Couldn't save the key: {e:#}")),
                                }
                            }
                            if self.key_source == KeySource::Keyring && ui.button("Remove").clicked() {
                                if let Err(e) = settings::clear_api_key() {
                                    self.settings_error = Some(format!("Couldn't remove the key: {e:#}"));
                                }
                                changed = true;
                            }
                        });
                    });
                    ui.end_row();
                });
                if let Some(error) = &self.settings_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if changed {
                    (self.api_key, self.key_source) = settings::load_api_key();
                    self.push_settings();
                }
            }
        }
    }
}

impl ChatPanel {
    /// Settings rows for an OpenAI-compatible server. Returns true when something changed.
    fn server_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let mut new_server = false;

        ui.label("Server");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let field = egui::TextEdit::singleline(&mut self.settings.openai_base_url)
                    .hint_text("https://api.openai.com/v1")
                    .desired_width((ui.available_width() - 70.0).max(60.0));
                // Saved when the box is left, not on every key press.
                new_server |= ui.add(field).lost_focus();
                egui::ComboBox::from_id_salt("server_presets").selected_text("Pick").width(50.0).show_ui(ui, |ui| {
                    for (name, url) in OPENAI_PRESETS {
                        if ui.selectable_label(self.settings.openai_base_url == *url, *name).clicked() {
                            self.settings.openai_base_url = (*url).to_owned();
                            new_server = true;
                        }
                    }
                });
            });
            if let Err(e) = intercraft_llm::completions_url(&self.settings.openai_base_url) {
                ui.colored_label(ui.visuals().error_fg_color, capitalise(&format!("{e:#}")));
            }
        });
        ui.end_row();

        ui.label("Model");
        let model = egui::TextEdit::singleline(&mut self.settings.openai_model).hint_text("The model's id on that server, e.g. gpt-5 or qwen3:14b");
        changed |= ui.add(model).lost_focus();
        ui.end_row();

        ui.label("Images");
        changed |= ui
            .checkbox(&mut self.settings.openai_vision, "This model can read images")
            .on_hover_text("Lets the model look at the part with view_model. Leave it off for text-only models.")
            .changed();
        ui.end_row();

        ui.label("API key");
        ui.vertical(|ui| {
            ui.weak(if self.server_key.is_empty() {
                "None saved for this server. Local servers don't need one."
            } else {
                "Saved for this server in the system credential store."
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.key_input)
                        .password(true)
                        .hint_text("sk-...")
                        .desired_width((ui.available_width() - 70.0).max(60.0)),
                );
                let url = self.settings.openai_base_url.clone();
                if ui.add_enabled(!self.key_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
                    match settings::save_server_key(&url, &self.key_input) {
                        Ok(()) => {
                            self.key_input.clear();
                            self.settings_error = None;
                            new_server = true;
                        }
                        Err(e) => self.settings_error = Some(format!("Couldn't save the key: {e:#}")),
                    }
                }
                if !self.server_key.is_empty() && ui.button("Remove").clicked() {
                    if let Err(e) = settings::clear_server_key(&url) {
                        self.settings_error = Some(format!("Couldn't remove the key: {e:#}"));
                    }
                    new_server = true;
                }
            });
        });
        ui.end_row();

        if new_server {
            // Each server has its own key, so a key is never sent to a server it wasn't saved for.
            self.server_key = settings::load_server_key(&self.settings.openai_base_url);
        }
        changed | new_server
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Anthropic API key",
        Provider::ClaudeCode => "Claude Code (your subscription)",
        Provider::OpenAi => "OpenAI-compatible server",
    }
}

fn claude_code_model_name(alias: &str) -> &'static str {
    CLAUDE_CODE_MODELS
        .iter()
        .find(|(a, _)| *a == alias)
        .map_or("Your Claude Code default", |(_, name)| name)
}

/// Shown when there's no API key: subscription users can drive InterCraft from Claude Code.
fn subscription_help(ui: &mut egui::Ui) {
    // Registers the server for every folder, using the launcher that ships with this checkout.
    let launcher = std::path::absolute(concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/mcp_launcher.py"))
        .unwrap_or_default();
    let command = format!(
        "claude mcp add --scope user intercraft -- python \"{}\"",
        launcher.display()
    );

    egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(6.0)
        .inner_margin(10)
        .show(ui, |ui| {
            // Stay inside the panel and wrap, however narrow it is.
            ui.set_max_width(ui.available_width());
            ui.strong("Using a Claude Pro or Max subscription?");
            ui.label(concat!(
                "Open Settings and set \"Use\" to Claude Code. It runs your own Claude Code ",
                "in the background with your sign-in, and you chat here as normal."
            ));
            ui.add_space(4.0);
            ui.label("Or work from a Claude Code terminal instead:");
            ui.label("1. Copy this command and run it once in a terminal. It registers InterCraft with Claude Code.");
            if ui.button("Copy command").on_hover_text(&command).clicked() {
                ui.ctx().copy_text(command.clone());
            }
            ui.label("2. Run claude in any folder and check /mcp shows intercraft.");
            ui.label("3. Ask for a part, for example \"make a 60 x 40 x 5 mm plate with four M4 holes\".");
            ui.add_space(4.0);
            ui.weak(concat!(
                "Other agent apps that support MCP (Codex CLI, Gemini CLI, Cursor) work the same way. ",
                "Or add an API key in Settings to chat here."
            ));
        });
}

/// Returns true when the user answered a question this frame.
fn message_ui(
    ui: &mut egui::Ui,
    message: &mut Message,
    markdown: &mut egui_commonmark::CommonMarkCache,
    open: bool,
) -> bool {
    let mut answered = false;
    match message {
        Message::User(text, sketch) => {
            egui::Frame::new()
                .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.3))
                .corner_radius(6.0)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !text.is_empty() {
                        ui.label(text.as_str());
                    }
                    if *sketch {
                        ui.weak("Sent with a sketch");
                    }
                });
        }
        Message::Assistant { model, items } => {
            if !model.is_empty() {
                ui.weak(model.as_str());
            }
            for item in items.iter_mut() {
                match item {
                    Item::Text(text) => {
                        egui_commonmark::CommonMarkViewer::new().show(ui, markdown, text.trim());
                    }
                    Item::Note(text) => {
                        ui.label(egui::RichText::new(text.trim()).italics().weak());
                    }
                    Item::Tool(call) if is_question(call) => answered |= question_ui(ui, call, open),
                    Item::Tool(call) => tool_ui(ui, call),
                }
            }
        }
        Message::Notice(text) => {
            ui.weak(text);
        }
        Message::Error(text) => {
            ui.colored_label(ui.visuals().error_fg_color, text.as_str());
        }
    }
    answered
}

/// An ask_user call that passed validation (a rejected one shows as a normal tool row).
fn is_question(call: &ToolCall) -> bool {
    call.name == "ask_user" && !matches!(call.result, Some((false, _)))
}

/// A question card. Returns true when the user answered it this frame.
fn question_ui(ui: &mut egui::Ui, call: &mut ToolCall, open: bool) -> bool {
    let question = call.input["question"].as_str().unwrap_or_default().to_owned();
    let options: Vec<(String, String)> = call.input["options"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            (
                o["label"].as_str().unwrap_or_default().to_owned(),
                o["description"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let multi = call.input["multi_select"] == true;
    if call.picks.len() != options.len() {
        call.picks = vec![false; options.len()];
    }

    let mut answer = None;
    let accent = ui.visuals().selection.stroke.color;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, if open { accent } else { ui.visuals().widgets.noninteractive.bg_stroke.color }))
        .corner_radius(6.0)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong(&question);
            if let Some(given) = &call.answer {
                ui.weak(format!("You answered: {given}"));
                return;
            }
            ui.add_space(4.0);
            ui.add_enabled_ui(open, |ui| {
                for (i, (label, description)) in options.iter().enumerate() {
                    if multi {
                        ui.checkbox(&mut call.picks[i], label.as_str());
                    } else {
                        let button = egui::Button::new(label.as_str())
                            .min_size(egui::vec2(ui.available_width(), 26.0));
                        if ui.add(button).clicked() {
                            answer = Some(label.clone());
                        }
                    }
                    if !description.is_empty() {
                        ui.label(egui::RichText::new(description).small().weak());
                    }
                    ui.add_space(2.0);
                }
                if multi {
                    let any = call.picks.iter().any(|p| *p);
                    if ui.add_enabled(any, egui::Button::new("Send choices")).clicked() {
                        let chosen: Vec<&str> = options
                            .iter()
                            .zip(&call.picks)
                            .filter(|(_, picked)| **picked)
                            .map(|((label, _), _)| label.as_str())
                            .collect();
                        answer = Some(chosen.join(", "));
                    }
                }
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut call.other)
                            .hint_text("Or type your own answer")
                            .desired_width((ui.available_width() - 60.0).max(80.0)),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let typed = !call.other.trim().is_empty();
                    if (ui.add_enabled(typed, egui::Button::new("Send")).clicked() || enter) && typed {
                        answer = Some(call.other.trim().to_owned());
                    }
                });
            });
            if !open {
                ui.weak("Answer once the reply has finished.");
            }
        });
    match answer {
        Some(given) => {
            call.answer = Some(given);
            true
        }
        None => false,
    }
}

fn tool_ui(ui: &mut egui::Ui, call: &ToolCall) {
    let green = egui::Color32::from_rgb(70, 170, 90);
    let (color, outcome) = match &call.result {
        None => (ui.visuals().weak_text_color(), "running".to_owned()),
        Some((false, output)) => (ui.visuals().error_fg_color, first_line(output)),
        Some((true, output)) if output["ok"] == false => {
            (ui.visuals().error_fg_color, "saved, but the part doesn't build".to_owned())
        }
        Some((true, output)) => (green, success_summary(&call.input, output)),
    };
    let response = egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if call.result.is_none() {
                    ui.spinner();
                } else {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                }
                ui.monospace(&call.name);
                let summary = call_summary(&call.input);
                if !summary.is_empty() {
                    ui.weak(summary);
                }
                ui.label(egui::RichText::new(outcome).color(color).small());
            });
        })
        .response;
    response.on_hover_ui(|ui| {
        ui.set_max_width(420.0);
        ui.monospace(serde_json::to_string_pretty(&call.input).unwrap_or_default());
    });
}

/// "RodBracket · Rod bore", "f020 · bore_d = 14"
fn call_summary(input: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["part", "feature_id", "label", "name"] {
        if let Some(v) = input[key].as_str() {
            parts.push(v.to_owned());
        }
    }
    if let Some(value) = input.get("value") {
        if let Some(last) = parts.last_mut() {
            *last = format!("{last} = {value}");
        }
    }
    parts.join(" · ")
}

fn success_summary(input: &Value, output: &Value) -> String {
    // Show the volume change of the feature this call touched, when there is one.
    let target = output["added_feature"].as_str().or(input["feature_id"].as_str());
    let features = output["features"].as_array();
    let feature = features.and_then(|fs| match target {
        Some(id) => fs.iter().find(|f| f["id"] == id),
        None => fs.last(),
    });
    match feature.and_then(|f| f["volume_change_mm3"].as_f64().or(f["volume_mm3"].as_f64())) {
        Some(v) if feature.is_some_and(|f| f.get("volume_change_mm3").is_some()) => format!("{v:+.1} mm³"),
        Some(v) => format!("{v:.1} mm³"),
        None => "done".to_owned(),
    }
}

fn first_line(value: &Value) -> String {
    let text = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
    text.lines().next().unwrap_or_default().chars().take(120).collect()
}

/// Exposes the shared InterCraft tools to the chat model.
struct HostTools(ToolHost);

impl ToolExecutor for HostTools {
    fn definitions(&self) -> Vec<ToolDef> {
        intercraft_tools::chat_panel_definitions()
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| ToolDef {
                name: t["name"].as_str().unwrap_or_default().to_owned(),
                description: t["description"].as_str().unwrap_or_default().to_owned(),
                input_schema: t["inputSchema"].clone(),
            })
            .collect()
    }

    fn call(&mut self, name: &str, input: &Value) -> anyhow::Result<Value> {
        self.0.call(name, input)
    }
}

fn run_worker(
    dir: PathBuf,
    config: Config,
    commands: Receiver<Command>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    // Claude Code runs the tools itself, so only the API route needs a warm tool host.
    let mut host = ToolHost::new(dir).with_ask_user();
    if config.provider != Provider::ClaudeCode {
        host.warm_up();
    }
    let mut tools = HostTools(host);
    let mut provider = config.provider;
    let mut anthropic = AnthropicChat::new(config.anthropic);
    let mut claude_code = ClaudeCodeChat::new(config.claude_code);
    let mut openai = OpenAiChat::new(config.openai);

    for command in commands {
        match command {
            Command::Send { text, images } => {
                let mut on_event = |event| {
                    let _ = replies.send(Reply::Event(event));
                    ctx.request_repaint();
                };
                let chat: &mut dyn ChatSession = match provider {
                    Provider::Anthropic => &mut anthropic,
                    Provider::ClaudeCode => &mut claude_code,
                    Provider::OpenAi => &mut openai,
                };
                let reply = match chat.send_with_images(&text, &images, &mut tools, &mut on_event, &cancel) {
                    Ok(()) => None,
                    Err(e) if e.is::<Cancelled>() => Some(Reply::Stopped),
                    Err(e) => Some(Reply::Failed(format!("{e:#}"))),
                };
                if let Some(reply) = reply {
                    let _ = replies.send(reply);
                }
                ctx.request_repaint();
            }
            Command::Reset => {
                anthropic.reset();
                claude_code.reset();
                openai.reset();
            }
            Command::Configure(config) => {
                provider = config.provider;
                anthropic.update_settings(config.anthropic);
                claude_code.update_settings(config.claude_code);
                openai.update_settings(config.openai);
            }
        }
    }
}
