//! Фоновый agent-воркер для GUI.
//!
//! Переиспользует ядро (`ConversationRuntime`, `Session`, `PermissionPolicy`,
//! реестр инструментов `tools`); `ApiClient` / `ToolExecutor` / `PermissionPrompter`
//! шлют события в UI-канал. Ход гоняется в отдельном потоке; отмена — атомарный флаг.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use api::{
    max_tokens_for_model, read_base_url, resolve_startup_auth_source, AnthropicClient, ApiError,
    AuthSource, ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    OutputContentBlock, PromptCache, ProviderClient, ProviderSlotKind, ProvidersConfig,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};
use runtime::{
    load_system_prompt, ApiClient, ApiRequest, AssistantEvent, ConfigLoader, ContentBlock,
    ConversationMessage, ConversationRuntime, HookAbortSignal, HookProgressEvent,
    HookProgressReporter, MessageRole, PermissionMode, PermissionPolicy,
    PermissionPromptDecision, PermissionPrompter, PermissionRequest, RuntimeError,
    RuntimeFeatureConfig, Session, ToolError, ToolExecutor,
};
use serde_json::Value;
use tools::{mvp_tool_specs, GlobalToolRegistry};

use super::protocol::{Activity, AgentHandle, AgentToUi, UiToAgent};


/// Строит клиента основной модели из секции `[primary]` файла providers.toml.
/// Поддерживает Anthropic и любой OpenAI-совместимый провайдер.
fn build_provider_client(
    session_id: &str,
    requested_model: &str,
) -> Result<(ProviderClient, String, u32), ApiError> {
    if let Some(slot) = ProvidersConfig::load().primary {
        if slot.model == requested_model {
            match slot.kind {
                ProviderSlotKind::Openai => {
                    return Ok((
                        ProviderClient::OpenAi(slot.openai_client()),
                        slot.model,
                        slot.max_tokens,
                    ));
                }
                ProviderSlotKind::Anthropic => {
                    let auth = if slot.api_key.trim().is_empty() {
                        resolve_auth_source()?
                    } else {
                        AuthSource::ApiKey(slot.api_key.clone())
                    };
                    let base_url = if slot.base_url.trim().is_empty() {
                        read_base_url()
                    } else {
                        slot.base_url.clone()
                    };
                    let client = AnthropicClient::from_auth(auth)
                        .with_base_url(base_url)
                        .with_prompt_cache(PromptCache::new(session_id));
                    return Ok((ProviderClient::Anthropic(client), slot.model, slot.max_tokens));
                }
            }
        }
    }
    let client = AnthropicClient::from_auth(resolve_auth_source()?)
        .with_base_url(read_base_url())
        .with_prompt_cache(PromptCache::new(session_id));
    let max_tokens = max_tokens_for_model(requested_model);
    Ok((
        ProviderClient::Anthropic(client),
        requested_model.to_string(),
        max_tokens,
    ))
}

fn resolve_auth_source() -> Result<AuthSource, ApiError> {
    resolve_startup_auth_source(|| {
        let cwd = std::env::current_dir().map_err(ApiError::from)?;
        let config = ConfigLoader::default_for(&cwd)
            .load()
            .map_err(|error| ApiError::Auth(format!("failed to load OAuth config: {error}")))?;
        Ok(config.oauth().cloned())
    })
}

fn build_policy(mode: PermissionMode) -> PermissionPolicy {
    mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(mode),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    )
}

fn system_prompt(cwd: &Path) -> Vec<String> {
    load_system_prompt(
        cwd.to_path_buf(),
        // Живая дата: захардкоженное «сегодня» ломало модели все рассуждения
        // о времени (сколько бежит билд, свежесть коммитов и т.п.).
        runtime::clock::current_date_utc(),
        std::env::consts::OS,
        "unknown",
    )
    .unwrap_or_else(|_| {
        vec![format!(
            "You are claw, a precise coding assistant running locally inside the project at `{}`. \
             Inspect it with the tools instead of asking. Keep answers concise.",
            cwd.display()
        )]
    })
}

/// Запускает agent-воркер в отдельном потоке и возвращает ручку с каналами.
/// `save_path` (если задан) — куда сохранять сессию после каждого хода.
#[must_use]
pub fn spawn_agent(
    model: String,
    mode: PermissionMode,
    session: Session,
    save_path: Option<PathBuf>,
) -> AgentHandle {
    let (to_agent_tx, to_agent_rx) = mpsc::channel::<UiToAgent>();
    let (from_agent_tx, from_agent_rx) = mpsc::channel::<AgentToUi>();
    let (perm_tx, perm_rx) = mpsc::channel::<bool>();
    let (question_tx, question_rx) = mpsc::channel::<String>();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);

    thread::spawn(move || {
        worker(
            model,
            mode,
            session,
            save_path,
            &to_agent_rx,
            &from_agent_tx,
            perm_rx,
            question_rx,
            &worker_cancel,
        );
    });

    AgentHandle {
        to_agent: to_agent_tx,
        from_agent: from_agent_rx,
        permission_reply: perm_tx,
        question_reply: question_tx,
        cancel,
    }
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn worker(
    model: String,
    mode: PermissionMode,
    session: Session,
    save_path: Option<PathBuf>,
    rx: &Receiver<UiToAgent>,
    tx: &Sender<AgentToUi>,
    perm_rx: Receiver<bool>,
    question_rx: Receiver<String>,
    cancel: &Arc<AtomicBool>,
) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let runtime_config = ConfigLoader::default_for(&cwd).load().ok();
    let feature_config = runtime_config
        .as_ref()
        .map_or_else(RuntimeFeatureConfig::default, |config| {
            config.feature_config().clone()
        });

    let api = match TuiApiClient::new(&model, &session.session_id, tx.clone(), Arc::clone(cancel)) {
        Ok(client) => client,
        Err(error) => {
            let _ = tx.send(AgentToUi::Error(error.to_string()));
            return;
        }
    };
    let tools = GuiToolExecutor {
        registry: GlobalToolRegistry::builtin(),
        tx: tx.clone(),
        cancel: Arc::clone(cancel),
        question_rx,
    };
    let policy = build_policy(mode).with_permission_rules(feature_config.permission_rules());
    let mut runtime = ConversationRuntime::new_with_features(
        session,
        api,
        tools,
        policy,
        system_prompt(&cwd),
        &feature_config,
    )
    // Отмена хода (Esc) должна убивать и запущенные хуки, иначе долгий
    // PostToolUse (cargo check и т.п.) продолжает держать ход. Флаг сбрасывается
    // в false перед каждым Prompt — сигнал взводится заново автоматически.
    .with_hook_abort_signal(HookAbortSignal::from_flag(Arc::clone(cancel)))
    .with_hook_progress_reporter(Box::new(GuiHookProgressReporter { tx: tx.clone() }));
    let mut prompter = GuiPermissionPrompter {
        tx: tx.clone(),
        reply_rx: perm_rx,
    };

    for command in rx {
        match command {
            UiToAgent::Prompt(text) => {
                cancel.store(false, Ordering::SeqCst);
                match runtime.run_turn(text, Some(&mut prompter)) {
                    Ok(_) => {
                        let _ = tx.send(AgentToUi::TurnDone);
                    }
                    Err(error) => {
                        let _ = tx.send(AgentToUi::Error(error.to_string()));
                    }
                }
                // Сохраняем сессию ВСЕГДА (и при ошибке хода), чтобы история не терялась.
                if let Some(path) = &save_path {
                    let _ = runtime.session().save_to_path(path);
                }
                let _ = tx.send(AgentToUi::Activity(Activity::Idle));
            }
        }
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    mvp_tool_specs()
        .into_iter()
        .map(|spec| ToolDefinition {
            name: spec.name.to_string(),
            description: Some(spec.description.to_string()),
            input_schema: spec.input_schema,
        })
        .collect()
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then_some(InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

/// `ApiClient`, стримящий ответ модели в UI-канал.
struct TuiApiClient {
    client: ProviderClient,
    model: String,
    max_tokens: u32,
    runtime: tokio::runtime::Runtime,
    tx: Sender<AgentToUi>,
    cancel: Arc<AtomicBool>,
}

impl TuiApiClient {
    fn new(
        model: &str,
        session_id: &str,
        tx: Sender<AgentToUi>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        let (client, model, max_tokens) = build_provider_client(session_id, model)
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        Ok(Self {
            client,
            model,
            max_tokens,
            runtime,
            tx,
            cancel,
        })
    }
}

fn handle_start_block(
    block: OutputContentBlock,
    tx: &Sender<AgentToUi>,
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
) {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let _ = tx.send(AgentToUi::Text(text.clone()));
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            let initial = if input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial));
        }
        OutputContentBlock::Thinking { thinking, .. } => {
            let _ = tx.send(AgentToUi::Thinking(thinking));
        }
        OutputContentBlock::RedactedThinking { .. } => {}
    }
}

impl ApiClient for TuiApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let _ = self.tx.send(AgentToUi::Activity(Activity::Model));
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens.min(max_tokens_for_model(&self.model)),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty())
                .then(|| request.system_prompt.join("\n\n")),
            tools: Some(tool_definitions()),
            tool_choice: Some(ToolChoice::Auto),
            stream: true,
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        let cancel = Arc::clone(&self.cancel);

        self.runtime.block_on(async move {
            let mut stream = client
                .stream_message(&message_request)
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            let mut events = Vec::new();
            let mut pending_tool: Option<(String, String, String)> = None;
            let mut saw_stop = false;

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?
            {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            handle_start_block(block, &tx, &mut events, &mut pending_tool);
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        handle_start_block(start.content_block, &tx, &mut events, &mut pending_tool);
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                let _ = tx.send(AgentToUi::Text(text.clone()));
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { thinking } => {
                            let _ = tx.send(AgentToUi::Thinking(thinking));
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some((_, _, input)) = &mut pending_tool {
                                input.push_str(&partial_json);
                            }
                        }
                        ContentBlockDelta::SignatureDelta { .. } => {}
                    },
                    ApiStreamEvent::ContentBlockStop(_) => {
                        if let Some((id, name, input)) = pending_tool.take() {
                            let _ = tx.send(AgentToUi::ToolCall {
                                name: name.clone(),
                                input: input.clone(),
                            });
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        let usage = delta.usage.token_usage();
                        let _ = tx.send(AgentToUi::Usage {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                        });
                        events.push(AssistantEvent::Usage(usage));
                    }
                    ApiStreamEvent::MessageStop(_) => {
                        saw_stop = true;
                        events.push(AssistantEvent::MessageStop);
                    }
                }
            }

            if !saw_stop
                && events.iter().any(|event| {
                    matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                        || matches!(event, AssistantEvent::ToolUse { .. })
                })
            {
                events.push(AssistantEvent::MessageStop);
            }
            Ok(events)
        })
    }
}

/// `ToolExecutor` поверх встроенного реестра; результат шлёт в UI-канал.
struct GuiToolExecutor {
    registry: GlobalToolRegistry,
    tx: Sender<AgentToUi>,
    cancel: Arc<AtomicBool>,
    question_rx: Receiver<String>,
}

impl ToolExecutor for GuiToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(ToolError::new("cancelled by user"));
        }
        if tool_name == "AskUserQuestion" {
            return self.ask_user(input);
        }

        let _ = self.tx.send(AgentToUi::Activity(Activity::Tool {
            label: tool_activity_label(tool_name),
        }));

        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        match self.registry.execute(tool_name, &value) {
            Ok(output) => {
                let _ = self.tx.send(AgentToUi::ToolResult {
                    output: output.clone(),
                    is_error: false,
                });
                Ok(output)
            }
            Err(error) => {
                let _ = self.tx.send(AgentToUi::ToolResult {
                    output: error.clone(),
                    is_error: true,
                });
                Err(ToolError::new(error))
            }
        }
    }
}

impl GuiToolExecutor {
    fn ask_user(&mut self, input: &str) -> Result<String, ToolError> {
        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid AskUserQuestion JSON: {error}")))?;
        let question = value
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("(no question text)")
            .to_string();
        let options = value
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let _ = self.tx.send(AgentToUi::Activity(Activity::Waiting {
            label: "your answer".to_string(),
        }));
        let _ = self.tx.send(AgentToUi::AskUser { question, options });
        match self.question_rx.recv() {
            Ok(answer) if answer.trim().is_empty() => {
                Ok("User skipped the question; proceed with your best judgment.".to_string())
            }
            Ok(answer) => Ok(answer),
            Err(_) => Err(ToolError::new("UI disconnected while awaiting an answer")),
        }
    }
}

/// Показывает в футере, какой hook сейчас выполняется — без этого пауза после
/// правки файла (verify-хук гоняет check/lint) выглядит как зависание.
struct GuiHookProgressReporter {
    tx: Sender<AgentToUi>,
}

impl HookProgressReporter for GuiHookProgressReporter {
    fn on_event(&mut self, event: &HookProgressEvent) {
        if let HookProgressEvent::Started { command, .. } = event {
            let _ = self.tx.send(AgentToUi::Activity(Activity::Tool {
                label: format!("hook · {}", hook_label(command)),
            }));
        }
    }
}

/// Короткая подпись hook-команды: имя скрипта, если он есть, иначе первое слово.
fn hook_label(command: &str) -> String {
    let tokens = command
        .split_whitespace()
        .map(|token| token.trim_matches(['"', '\'']))
        .collect::<Vec<_>>();
    let script = tokens
        .iter()
        .find(|token| {
            Path::new(token)
                .extension()
                .is_some_and(|ext| matches!(ext.to_str(), Some("sh" | "py" | "js" | "rb")))
        })
        .or_else(|| tokens.first());
    script.map_or_else(
        || "hook".to_string(),
        |token| {
            Path::new(token)
                .file_name()
                .map_or_else(|| (*token).to_string(), |name| name.to_string_lossy().into_owned())
        },
    )
}

fn tool_activity_label(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
        return format!("MCP · {server}");
    }
    match name {
        "bash" | "PowerShell" | "REPL" => "shell".to_string(),
        "WebFetch" => "web fetch".to_string(),
        "WebSearch" => "web search".to_string(),
        "read_file" | "write_file" | "edit_file" | "glob_search" | "grep_search" => {
            "files".to_string()
        }
        other => other.to_string(),
    }
}

/// `PermissionPrompter`, отдающий запрос в UI и ждущий ответ из канала.
struct GuiPermissionPrompter {
    tx: Sender<AgentToUi>,
    reply_rx: Receiver<bool>,
}

impl PermissionPrompter for GuiPermissionPrompter {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        let _ = self.tx.send(AgentToUi::Activity(Activity::Waiting {
            label: "permission".to_string(),
        }));
        let _ = self.tx.send(AgentToUi::PermissionAsk {
            tool_name: request.tool_name.clone(),
            input: request.input.clone(),
            reason: request.reason.clone(),
        });
        match self.reply_rx.recv() {
            Ok(true) => PermissionPromptDecision::Allow,
            Ok(false) => PermissionPromptDecision::Deny {
                reason: "denied by user".to_string(),
            },
            Err(_) => PermissionPromptDecision::Deny {
                reason: "ui disconnected".to_string(),
            },
        }
    }
}
