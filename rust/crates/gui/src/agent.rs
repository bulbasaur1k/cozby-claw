//! Фоновый agent-воркер и GUI-реализации портов рантайма.
//!
//! Переиспользуем ядро (`ConversationRuntime`, `Session`, `PermissionPolicy`,
//! реестр инструментов `tools`), а `ApiClient` / `ToolExecutor` /
//! `PermissionPrompter` реализуем заново так, чтобы они слали события в UI-канал
//! вместо stdout. Поток модели направлен на OpenAI-совместимый endpoint (qwen).
//!
//! Воркер сообщает UI «что делает сейчас» через [`AgentToUi::Activity`], а
//! инструмент `Agent` исполняется как под-агент: поднимается отдельный
//! `ConversationRuntime`, его шаги стримятся как [`AgentToUi::SubAgent`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use api::{
    max_tokens_for_model, ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    OutputContentBlock, ProviderClient, StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition,
    ToolResultContentBlock,
};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationMessage, ConversationRuntime,
    McpServerManager, MessageRole, PermissionMode, PermissionPolicy, PermissionPromptDecision,
    PermissionPrompter, PermissionRequest, RuntimeConfig, RuntimeError, Session, ToolError,
    ToolExecutor,
};
use serde_json::Value;
use tools::{mvp_tool_specs, GlobalToolRegistry};

/// Строит политику прав с реальными требованиями каждого инструмента из
/// `mvp_tool_specs`. Без этого `PermissionPolicy` считает любой незарегистрированный
/// инструмент требующим `DangerFullAccess` и спрашивает подтверждение на всё подряд.
fn build_policy(mode: PermissionMode) -> PermissionPolicy {
    mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(mode),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    )
}

use crate::config::ModelConfig;
use crate::protocol::{Activity, AgentHandle, AgentToUi, SubAgentEvent, UiToAgent};

/// Дата для проектного контекста системного промпта (как `DEFAULT_DATE` в CLI).
const GUI_PROMPT_DATE: &str = "2026-06-18";

/// Богатый системный промпт корневого агента (проектный/git-контекст) — общий с
/// CLI через [`appcore::system_prompt`], плюс директива «инспектируй, не спрашивай».
fn gui_system_prompt() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut prompt = appcore::system_prompt(&cwd, GUI_PROMPT_DATE);
    prompt.push(
        "You are ALREADY inside this project directory — when asked about \"this repo\" or \
         \"the current directory\", inspect it with tools instead of asking. You may delegate \
         larger subtasks to a sub-agent via the Agent tool."
            .to_string(),
    );
    prompt
}

/// Системный промпт под-агента: тот же контекст + фокус на одной подзадаче.
fn gui_subagent_system_prompt() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut prompt = appcore::system_prompt(&cwd, GUI_PROMPT_DATE);
    prompt.push(
        "You are now a focused sub-agent: complete the single delegated task using the tools, \
         then give a concise final answer with the result. Do not ask the user questions."
            .to_string(),
    );
    prompt
}

/// Максимальная глубина вложенности под-агентов (под-агент не спавнит под-агентов).
const MAX_SUBAGENT_DEPTH: u8 = 1;

/// Тонкая обёртка над UI-каналом, знающая, top-level это поток или под-агент.
///
/// Для под-агента (`sub_id = Some`) пошаговый текст/usage в основной транскрипт
/// не льются — вместо этого вызовы инструментов отражаются как компактные шаги
/// [`SubAgentEvent::Step`], а индикатор активности не перетирается.
#[derive(Clone)]
struct UiEmit {
    tx: Sender<AgentToUi>,
    sub_id: Option<u64>,
}

impl UiEmit {
    fn top(tx: Sender<AgentToUi>) -> Self {
        Self { tx, sub_id: None }
    }

    fn child(tx: Sender<AgentToUi>, sub_id: u64) -> Self {
        Self {
            tx,
            sub_id: Some(sub_id),
        }
    }

    /// Прямой доступ к каналу (для отправки `SubAgent`-событий самим исполнителем).
    fn raw(&self) -> &Sender<AgentToUi> {
        &self.tx
    }

    fn text(&self, text: String) {
        if self.sub_id.is_none() && !text.is_empty() {
            let _ = self.tx.send(AgentToUi::Text(text));
        }
    }

    fn thinking(&self, text: String) {
        if self.sub_id.is_none() && !text.is_empty() {
            let _ = self.tx.send(AgentToUi::Thinking(text));
        }
    }

    fn tool_call(&self, name: &str, input: &str) {
        match self.sub_id {
            None => {
                let _ = self.tx.send(AgentToUi::ToolCall {
                    name: name.to_string(),
                    input: input.to_string(),
                });
            }
            Some(id) => {
                let _ = self.tx.send(AgentToUi::SubAgent {
                    id,
                    event: SubAgentEvent::Step(format!("⚙ {name}")),
                });
            }
        }
    }

    fn tool_result(&self, output: &str, is_error: bool) {
        match self.sub_id {
            None => {
                let _ = self.tx.send(AgentToUi::ToolResult {
                    output: output.to_string(),
                    is_error,
                });
            }
            Some(id) => {
                let tag = if is_error { "✘" } else { "✓" };
                let _ = self.tx.send(AgentToUi::SubAgent {
                    id,
                    event: SubAgentEvent::Step(format!("{tag} {}", first_line(output, 120))),
                });
            }
        }
    }

    fn usage(&self, input_tokens: u32, output_tokens: u32) {
        if self.sub_id.is_none() {
            let _ = self.tx.send(AgentToUi::Usage {
                input_tokens,
                output_tokens,
            });
        }
    }

    /// Индикатор активности обновляет только top-level поток, чтобы под-агент не
    /// перетирал статус под-агента, выставленный родителем.
    fn activity(&self, activity: Activity) {
        if self.sub_id.is_none() {
            let _ = self.tx.send(AgentToUi::Activity(activity));
        }
    }
}

/// Запускает agent-воркер в отдельном потоке и возвращает ручку с каналами.
/// `session` — начальная сессия (новая/загруженная); `save_path` — куда
/// автосохранять после каждого хода.
#[must_use]
pub fn spawn_agent(
    config: ModelConfig,
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
            config,
            &to_agent_rx,
            &from_agent_tx,
            perm_rx,
            question_rx,
            session,
            save_path,
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

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn worker(
    config: ModelConfig,
    rx: &Receiver<UiToAgent>,
    tx: &Sender<AgentToUi>,
    perm_rx: Receiver<bool>,
    question_rx: Receiver<String>,
    session: Session,
    save_path: Option<PathBuf>,
    cancel: &Arc<AtomicBool>,
) {
    let emit = UiEmit::top(tx.clone());
    // Фич-конфиг + MCP из `.claw` (как CLI): permission-rules, hooks, авто-компакция,
    // sandbox, external-consult, MCP-серверы. Так GUI уважает настройки проекта.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runtime_config = appcore::load_runtime_config(&cwd);
    let feature_config = appcore::feature_config(runtime_config.as_ref());
    let (mcp, mcp_tools) = runtime_config
        .as_ref()
        .and_then(GuiMcp::build)
        .map_or((None, Vec::new()), |(mcp, tools)| (Some(mcp), tools));

    let api = match GuiApiClient::new(
        &config,
        &session.session_id,
        emit.clone(),
        Arc::clone(cancel),
        true,
        mcp_tools.clone(),
    ) {
        Ok(client) => client,
        Err(error) => {
            let _ = tx.send(AgentToUi::Error(error.to_string()));
            return;
        }
    };
    let tools = GuiToolExecutor {
        registry: GlobalToolRegistry::builtin(),
        emit: emit.clone(),
        cancel: Arc::clone(cancel),
        config: config.clone(),
        depth: 0,
        sub_counter: Arc::new(AtomicU64::new(0)),
        question_rx: Some(question_rx),
        mcp,
    };
    // MCP-инструменты регистрируем в политике как workspace-write (иначе по
    // умолчанию считались бы danger-full-access и спрашивали бы каждый раз).
    let mut policy =
        build_policy(config.permission_mode).with_permission_rules(feature_config.permission_rules());
    for tool in &mcp_tools {
        policy = policy.with_tool_requirement(tool.name.clone(), PermissionMode::WorkspaceWrite);
    }
    let mut runtime = ConversationRuntime::new_with_features(
        session,
        api,
        tools,
        policy,
        gui_system_prompt(),
        &feature_config,
    );
    let mut prompter = GuiPermissionPrompter {
        tx: tx.clone(),
        reply_rx: perm_rx,
    };

    for command in rx {
        match command {
            UiToAgent::Prompt(text) => {
                cancel.store(false, Ordering::SeqCst);
                let outcome = runtime.run_turn(text, Some(&mut prompter));
                // Сохраняем сессию ВСЕГДА (и при ошибке хода тоже), иначе при сбое
                // запроса история терялась бы и сессия выглядела «не сохранённой».
                if let Some(path) = &save_path {
                    let _ = runtime.session().save_to_path(path);
                }
                match outcome {
                    Ok(_) => {
                        let _ = tx.send(AgentToUi::TurnDone);
                    }
                    Err(error) => {
                        let _ = tx.send(AgentToUi::Error(error.to_string()));
                    }
                }
                // Сбрасываем индикатор активности в покой после завершения хода.
                let _ = tx.send(AgentToUi::Activity(Activity::Idle));
            }
        }
    }
}

/// Инструменты, объявляемые модели (встроенный MVP-набор). Для под-агентов
/// инструмент `Agent` исключается, чтобы исключить бесконечную вложенность.
fn tool_definitions(offer_agent: bool) -> Vec<ToolDefinition> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| offer_agent || spec.name != "Agent")
        .map(|spec| ToolDefinition {
            name: spec.name.to_string(),
            description: Some(spec.description.to_string()),
            input_schema: spec.input_schema,
        })
        .collect()
}

/// Конвертирует историю рантайма в формат запроса провайдера.
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
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

/// `ApiClient`, стримящий ответ OpenAI-совместимой модели в UI-канал.
struct GuiApiClient {
    /// Клиент провайдера (Anthropic или OpenAI-совместимый) — выбран `appcore`
    /// из `providers.toml`, как в CLI.
    client: ProviderClient,
    model: String,
    max_tokens: u32,
    runtime: tokio::runtime::Runtime,
    emit: UiEmit,
    cancel: Arc<AtomicBool>,
    /// Объявлять ли модели инструмент `Agent` (false для под-агентов).
    offer_agent: bool,
    /// Объявления MCP-инструментов, добавляемые к встроенным.
    mcp_tools: Vec<ToolDefinition>,
}

impl GuiApiClient {
    fn new(
        config: &ModelConfig,
        session_id: &str,
        emit: UiEmit,
        cancel: Arc<AtomicBool>,
        offer_agent: bool,
        mcp_tools: Vec<ToolDefinition>,
    ) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        let (client, model, max_tokens) =
            appcore::build_provider_client(session_id, &config.model)
                .map_err(|error| RuntimeError::new(error.to_string()))?;
        Ok(Self {
            client,
            model,
            max_tokens,
            runtime,
            emit,
            cancel,
            offer_agent,
            mcp_tools,
        })
    }
}

/// Кладёт стартовый блок (из `message_start` / `content_block_start`) в события.
fn handle_start_block(
    block: OutputContentBlock,
    emit: &UiEmit,
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
) {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                emit.text(text.clone());
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // При стриминге стартовый вход пустой ({}); реальный придёт дельтами.
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
            emit.thinking(thinking);
        }
        OutputContentBlock::RedactedThinking { .. } => {}
    }
}

impl ApiClient for GuiApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        // Сообщаем UI, что сейчас идёт запрос к модели.
        self.emit.activity(Activity::Model);
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens.min(max_tokens_for_model(&self.model)),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty())
                .then(|| request.system_prompt.join("\n\n")),
            tools: Some({
                let mut tools = tool_definitions(self.offer_agent);
                tools.extend(self.mcp_tools.iter().cloned());
                tools
            }),
            tool_choice: Some(ToolChoice::Auto),
            stream: true,
        };
        let client = self.client.clone();
        let emit = self.emit.clone();
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
                // Кооперативная отмена: выходим до ContentBlockStop, поэтому
                // незавершённый tool-call не эмитится и цикл рантайма завершается.
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            handle_start_block(block, &emit, &mut events, &mut pending_tool);
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        handle_start_block(
                            start.content_block,
                            &emit,
                            &mut events,
                            &mut pending_tool,
                        );
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                emit.text(text.clone());
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { thinking } => {
                            emit.thinking(thinking);
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
                            emit.tool_call(&name, &input);
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        let usage = delta.usage.token_usage();
                        emit.usage(usage.input_tokens, usage.output_tokens);
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

/// MCP-серверы для GUI: собственный tokio-runtime + менеджер. Поднимается из
/// `.claw`-конфига (как CLI). Если серверов нет — не создаётся (None).
struct GuiMcp {
    runtime: tokio::runtime::Runtime,
    manager: McpServerManager,
}

impl GuiMcp {
    /// Поднимает MCP-серверы и обнаруживает инструменты. Возвращает состояние и
    /// объявления инструментов для модели, либо `None`, если серверы не настроены.
    fn build(runtime_config: &RuntimeConfig) -> Option<(Self, Vec<ToolDefinition>)> {
        let mut manager = McpServerManager::from_runtime_config(runtime_config);
        if manager.server_names().is_empty() {
            return None;
        }
        let runtime = tokio::runtime::Runtime::new().ok()?;
        let discovery = runtime.block_on(manager.discover_tools_best_effort());
        let tools = discovery
            .tools
            .iter()
            .map(|tool| ToolDefinition {
                name: tool.qualified_name.clone(),
                description: Some(tool.tool.description.clone().unwrap_or_else(|| {
                    format!("Invoke MCP tool `{}`.", tool.qualified_name)
                })),
                input_schema: tool.tool.input_schema.clone().unwrap_or_else(
                    || serde_json::json!({ "type": "object", "additionalProperties": true }),
                ),
            })
            .collect();
        Some((Self { runtime, manager }, tools))
    }

    /// Вызывает MCP-инструмент по квалифицированному имени, возвращая текстовый
    /// результат (pretty JSON), как делает CLI.
    fn call(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        let arguments = if input.trim().is_empty() {
            None
        } else {
            serde_json::from_str(input).ok()
        };
        let response = self
            .runtime
            .block_on(self.manager.call_tool(tool_name, arguments))
            .map_err(|error| ToolError::new(error.to_string()))?;
        if let Some(error) = response.error {
            return Err(ToolError::new(format!(
                "MCP tool `{tool_name}` error: {} ({})",
                error.message, error.code
            )));
        }
        let result = response
            .result
            .ok_or_else(|| ToolError::new(format!("MCP tool `{tool_name}` returned no result")))?;
        serde_json::to_string_pretty(&result).map_err(|error| ToolError::new(error.to_string()))
    }
}

/// `ToolExecutor` поверх встроенного реестра; результат шлёт в UI. Инструмент
/// `Agent` перехватывается и исполняется как под-агент (см. [`run_subagent`]),
/// `mcp__*` — маршрутизируется в [`GuiMcp`].
struct GuiToolExecutor {
    registry: GlobalToolRegistry,
    emit: UiEmit,
    cancel: Arc<AtomicBool>,
    config: ModelConfig,
    /// Глубина вложенности (0 — корневой агент).
    depth: u8,
    /// Счётчик идентификаторов под-агентов (общий на дерево вызовов).
    sub_counter: Arc<AtomicU64>,
    /// Канал ответов на `AskUserQuestion` от UI. `None` у под-агентов — они не
    /// спрашивают пользователя (иначе зависли бы на чтении ответа).
    question_rx: Option<Receiver<String>>,
    /// MCP-серверы (только у корневого исполнителя; у под-агентов `None`).
    mcp: Option<GuiMcp>,
}

impl ToolExecutor for GuiToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(ToolError::new("cancelled by user"));
        }

        if tool_name == "Agent" {
            return self.run_subagent(input);
        }
        // `AskUserQuestion` встроенного реестра читает stdin — в GUI это вешает
        // воркер навсегда. Перехватываем и спрашиваем через модал в UI.
        if tool_name == "AskUserQuestion" {
            return self.ask_user(input);
        }

        self.emit.activity(Activity::Tool {
            label: tool_activity_label(tool_name),
        });

        // MCP-инструменты (mcp__server__tool) маршрутизируем в менеджер.
        if tool_name.starts_with("mcp__") {
            let result = match self.mcp.as_mut() {
                Some(mcp) => mcp.call(tool_name, input),
                None => Err(ToolError::new(format!(
                    "MCP tool `{tool_name}` is not available (no MCP servers configured)"
                ))),
            };
            return match result {
                Ok(output) => {
                    self.emit.tool_result(&output, false);
                    Ok(output)
                }
                Err(error) => {
                    self.emit.tool_result(&error.to_string(), true);
                    Err(error)
                }
            };
        }

        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        match self.registry.execute(tool_name, &value) {
            Ok(output) => {
                self.emit.tool_result(&output, false);
                Ok(output)
            }
            Err(error) => {
                self.emit.tool_result(&error, true);
                Err(ToolError::new(error))
            }
        }
    }
}

impl GuiToolExecutor {
    /// Исполняет инструмент `Agent`: поднимает отдельный `ConversationRuntime`
    /// на подзадачу, стримит его шаги как `SubAgent`-события и возвращает финал.
    fn run_subagent(&mut self, input: &str) -> Result<String, ToolError> {
        if self.depth >= MAX_SUBAGENT_DEPTH {
            return Err(ToolError::new(
                "nested sub-agents are not supported (max depth reached)",
            ));
        }
        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid Agent input JSON: {error}")))?;
        let task = value
            .get("prompt")
            .and_then(Value::as_str)
            .or_else(|| value.get("description").and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();
        if task.is_empty() {
            return Err(ToolError::new("Agent: empty prompt/description"));
        }

        let id = self.sub_counter.fetch_add(1, Ordering::SeqCst);
        let tx = self.emit.raw().clone();
        let _ = tx.send(AgentToUi::SubAgent {
            id,
            event: SubAgentEvent::Started {
                description: task.clone(),
            },
        });
        self.emit.activity(Activity::SubAgent {
            label: first_line(&task, 60),
        });

        let result = run_subagent(
            &self.config,
            tx.clone(),
            &self.cancel,
            id,
            self.depth + 1,
            &task,
        );
        let _ = tx.send(AgentToUi::SubAgent {
            id,
            event: SubAgentEvent::Finished {
                ok: result.is_ok(),
            },
        });
        result
    }

    /// Перехват `AskUserQuestion`: отправляет вопрос в UI и ждёт ответ из канала
    /// (вместо чтения stdin). Под-агенты канала не имеют — отвечают «нет UI».
    fn ask_user(&mut self, input: &str) -> Result<String, ToolError> {
        let Some(question_rx) = &self.question_rx else {
            return Ok("No interactive UI available to a sub-agent; proceed using your \
                       best judgment without asking the user."
                .to_string());
        };
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

        self.emit.activity(Activity::Tool {
            label: "waiting for your answer".to_string(),
        });
        let _ = self
            .emit
            .raw()
            .send(AgentToUi::AskUser { question, options });
        // Блокируемся до ответа из UI; пустая строка = пользователь пропустил.
        match question_rx.recv() {
            Ok(answer) if answer.trim().is_empty() => {
                Ok("User skipped the question; proceed with your best judgment.".to_string())
            }
            Ok(answer) => Ok(answer),
            Err(_) => Err(ToolError::new("UI disconnected while awaiting an answer")),
        }
    }
}

/// Прогоняет один ход под-агента до конца и возвращает его финальный текст.
fn run_subagent(
    config: &ModelConfig,
    tx: Sender<AgentToUi>,
    cancel: &Arc<AtomicBool>,
    sub_id: u64,
    depth: u8,
    task: &str,
) -> Result<String, ToolError> {
    let emit = UiEmit::child(tx, sub_id);
    // Сессия под-агента создаётся заранее, чтобы её id ушёл в prompt-cache клиента.
    let session = Session::new();
    let api = GuiApiClient::new(
        config,
        &session.session_id,
        emit.clone(),
        Arc::clone(cancel),
        false,
        Vec::new(),
    )
    .map_err(|error| ToolError::new(error.to_string()))?;
    let tools = GuiToolExecutor {
        registry: GlobalToolRegistry::builtin(),
        emit,
        cancel: Arc::clone(cancel),
        config: config.clone(),
        depth,
        sub_counter: Arc::new(AtomicU64::new(0)),
        // Под-агент не задаёт вопросов пользователю и не использует MCP.
        question_rx: None,
        mcp: None,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let feature_config = appcore::feature_config(appcore::load_runtime_config(&cwd).as_ref());
    let policy =
        build_policy(config.permission_mode).with_permission_rules(feature_config.permission_rules());
    let mut runtime = ConversationRuntime::new_with_features(
        session,
        api,
        tools,
        policy,
        gui_subagent_system_prompt(),
        &feature_config,
    );
    // Под-агент не открывает модальных запросов разрешения: на «prompt» — отказ.
    let mut prompter = AutoDenyPrompter;
    runtime
        .run_turn(task.to_string(), Some(&mut prompter))
        .map_err(|error| ToolError::new(error.to_string()))?;
    let answer = last_assistant_text(runtime.session());
    Ok(if answer.trim().is_empty() {
        "(sub-agent finished without a text answer)".to_string()
    } else {
        answer
    })
}

/// Финальный текст ассистента из сессии (последнее assistant-сообщение).
fn last_assistant_text(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, MessageRole::Assistant))
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Человекочитаемая категория инструмента для индикатора активности.
fn tool_activity_label(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
        return format!("MCP · {server}");
    }
    match name {
        "bash" | "PowerShell" | "REPL" => "shell".to_string(),
        "WebFetch" => "web fetch".to_string(),
        "WebSearch" => "web search".to_string(),
        "read_file" | "write_file" | "edit_file" | "glob_search" | "grep_search"
        | "NotebookEdit" => "files".to_string(),
        other => other.to_string(),
    }
}

/// Первая непустая строка `text`, обрезанная до `max` символов (для подписей).
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let truncated: String = line.chars().take(max).collect();
    format!("{truncated}…")
}

/// `PermissionPrompter`, отдающий запрос в UI и ждущий ответ из канала.
struct GuiPermissionPrompter {
    tx: Sender<AgentToUi>,
    reply_rx: Receiver<bool>,
}

impl PermissionPrompter for GuiPermissionPrompter {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
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

/// Под-агенты не показывают модальных окон: любой запрос разрешения отклоняется.
struct AutoDenyPrompter;

impl PermissionPrompter for AutoDenyPrompter {
    fn decide(&mut self, _request: &PermissionRequest) -> PermissionPromptDecision {
        PermissionPromptDecision::Deny {
            reason: "sub-agent cannot request interactive permissions".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{first_line, tool_activity_label, tool_definitions};

    #[test]
    fn activity_labels_categorize_tools() {
        assert_eq!(tool_activity_label("bash"), "shell");
        assert_eq!(tool_activity_label("WebFetch"), "web fetch");
        assert_eq!(tool_activity_label("read_file"), "files");
        assert_eq!(
            tool_activity_label("mcp__github_server__create_issue"),
            "MCP · github_server"
        );
        // Незнакомый инструмент показывается как есть.
        assert_eq!(tool_activity_label("CustomThing"), "CustomThing");
    }

    #[test]
    fn first_line_takes_first_nonblank_and_truncates() {
        assert_eq!(first_line("\n\n  hello  \nworld", 80), "hello");
        let long = "x".repeat(200);
        let cut = first_line(&long, 10);
        assert_eq!(cut.chars().count(), 11, "10 chars + ellipsis");
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn subagents_are_not_offered_the_agent_tool() {
        assert!(
            tool_definitions(true).iter().any(|tool| tool.name == "Agent"),
            "top-level offers Agent"
        );
        assert!(
            !tool_definitions(false).iter().any(|tool| tool.name == "Agent"),
            "sub-agents must not be offered Agent (recursion guard)"
        );
    }
}
