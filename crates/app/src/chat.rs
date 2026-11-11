// Built-in chat panel. The conversation runs on a background thread with its own
// tool host, which edits the project files. The viewer picks those edits up from
// disk, the same way it does for an external MCP client.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui;
use intercraft_llm::{
    AnthropicChat, AnthropicSettings, Cancelled, ChatEvent, ChatSession, ToolDef, ToolExecutor,
    Usage, ANTHROPIC_MODELS,
};
use intercraft_tools::ToolHost;
use serde_json::Value;

use crate::settings::{self, KeySource, Settings, EFFORTS};

enum Command {
    Send(String),
    Reset,
    Settings(AnthropicSettings),
}

enum Reply {
    Event(ChatEvent),
    Failed(String),
    Stopped,
}

enum Message {
    User(String),
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
}

pub struct ChatPanel {
    project_dir: PathBuf,
    settings: Settings,
    api_key: String,
    key_source: KeySource,
    key_input: String,
    settings_open: bool,
    settings_error: Option<String>,

    worker: Option<(Sender<Command>, Receiver<Reply>)>,
    cancel: Arc<AtomicBool>,
    busy: bool,
    messages: Vec<Message>,
    input: String,
    usage: Usage,
    ctx: egui::Context,
}

impl ChatPanel {
    pub fn new(ctx: egui::Context, project_dir: PathBuf) -> Self {
        let (api_key, key_source) = settings::load_api_key();
        Self {
            project_dir,
            settings: Settings::load(),
            settings_open: key_source == KeySource::Missing,
            api_key,
            key_source,
            key_input: String::new(),
            settings_error: None,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: false,
            messages: Vec::new(),
            input: String::new(),
            usage: Usage::default(),
            ctx,
        }
    }

    fn llm_settings(&self) -> AnthropicSettings {
        AnthropicSettings {
            api_key: self.api_key.clone(),
            model: self.settings.model.clone(),
            effort: self.settings.effort.clone(),
            system: intercraft_tools::GUIDE.to_owned(),
        }
    }

    fn worker(&mut self) -> &Sender<Command> {
        if self.worker.is_none() {
            let (cmd_tx, cmd_rx) = channel();
            let (reply_tx, reply_rx) = channel();
            let ctx = self.ctx.clone();
            let cancel = self.cancel.clone();
            let settings = self.llm_settings();
            let dir = self.project_dir.clone();
            std::thread::spawn(move || run_worker(dir, settings, cmd_rx, reply_tx, cancel, ctx));
            self.worker = Some((cmd_tx, reply_rx));
        }
        &self.worker.as_ref().expect("worker was just started").0
    }

    fn push_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.settings_error = Some(format!("Couldn't save settings: {e:#}"));
        }
        if self.worker.is_some() {
            let settings = self.llm_settings();
            let _ = self.worker().send(Command::Settings(settings));
        }
    }

    /// `context` is a short note about what the user is looking at, sent with their message.
    fn send(&mut self, context: Option<String>) {
        let text = self.input.trim().to_owned();
        if text.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.messages.push(Message::User(text.clone()));
        let full = match context {
            Some(context) => format!("{text}\n\n[{context}]"),
            None => text,
        };
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        let _ = self.worker().send(Command::Send(full));
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
            self.messages.push(Message::Assistant { model: model.clone(), items: Vec::new() });
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
                items.push(Item::Tool(ToolCall { id, name, input, result: None }));
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
        self.settings_ui(ui);
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
                    let can_send = !self.input.trim().is_empty() && !self.api_key.is_empty();
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.usage.input_tokens + self.usage.cache_read_tokens > 0 {
                        ui.weak(format!(
                            "context {}k · cached {}k",
                            (self.usage.input_tokens + self.usage.cache_read_tokens + self.usage.cache_write_tokens) / 1000,
                            self.usage.cache_read_tokens / 1000
                        ))
                        .on_hover_text("Size of the last request and how much of it came from the prompt cache.");
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    ui.weak("Ask for a part, for example: \"Make a 60 x 40 x 5 mm plate with four M4 holes 6 mm in from the corners.\"");
                }
                for message in &self.messages {
                    message_ui(ui, message);
                    ui.add_space(6.0);
                }
            });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let model_name = ANTHROPIC_MODELS
            .iter()
            .find(|(id, _)| *id == self.settings.model)
            .map_or(self.settings.model.clone(), |(_, name)| (*name).to_owned());
        let header = format!("{model_name} · effort {}", self.settings.effort);
        let response = egui::CollapsingHeader::new(header)
            .id_salt("chat_settings")
            .open(self.settings_open.then_some(true))
            .show(ui, |ui| {
                let mut changed = false;
                egui::Grid::new("chat_settings_grid").num_columns(2).show(ui, |ui| {
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
            });
        // Opened automatically when no key is set; after that the user decides.
        if response.header_response.clicked() || self.key_source != KeySource::Missing {
            self.settings_open = false;
        }
    }
}

fn message_ui(ui: &mut egui::Ui, message: &Message) {
    match message {
        Message::User(text) => {
            egui::Frame::new()
                .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.3))
                .corner_radius(6.0)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(text);
                });
        }
        Message::Assistant { model, items } => {
            if !model.is_empty() {
                ui.weak(model);
            }
            for item in items {
                match item {
                    Item::Text(text) => {
                        ui.label(text.trim());
                    }
                    Item::Note(text) => {
                        ui.label(egui::RichText::new(text.trim()).italics().weak());
                    }
                    Item::Tool(call) => tool_ui(ui, call),
                }
            }
        }
        Message::Notice(text) => {
            ui.weak(text);
        }
        Message::Error(text) => {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
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
                ui.weak(call_summary(&call.input));
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
        intercraft_tools::definitions()
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
    settings: AnthropicSettings,
    commands: Receiver<Command>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    let mut host = ToolHost::new(dir);
    host.warm_up();
    let mut tools = HostTools(host);
    let mut chat = AnthropicChat::new(settings);

    for command in commands {
        match command {
            Command::Send(text) => {
                let mut on_event = |event| {
                    let _ = replies.send(Reply::Event(event));
                    ctx.request_repaint();
                };
                let reply = match chat.send(&text, &mut tools, &mut on_event, &cancel) {
                    Ok(()) => None,
                    Err(e) if e.is::<Cancelled>() => Some(Reply::Stopped),
                    Err(e) => Some(Reply::Failed(format!("{e:#}"))),
                };
                if let Some(reply) = reply {
                    let _ = replies.send(reply);
                }
                ctx.request_repaint();
            }
            Command::Reset => chat.reset(),
            Command::Settings(settings) => chat.update_settings(settings),
        }
    }
}
