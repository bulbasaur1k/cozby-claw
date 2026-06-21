# Архитектура Cozby

```mermaid
graph TD
    A[Пользователь CLI (cozby-claw-cli)] -->|Вводит команду| B[LiveCli]
    B --> C[Session]
    C --> D[ConversationRuntime]

    subgraph "Рантайм"
        D --> E[ProviderClient]
        E -->|Использует| F[ProvidersConfig]
        F --> G[Файл ~/.claw/providers.toml]

        D --> H[ExternalConsultRuntime]
        H -->|Также может использовать| F
    end

    subgraph "Провайдеры API"
        E --> I[AnthropicClient or OpenAiCompatClient]
    end

    I -->|HTTP запросы| J[API Антропик или совместимый (e.g. OpenRouter, Qwen)]
    J -->|Ответ| I

    I -->|Источник инструментов и разрешений| D
    D -->|Кэширование промтов| K[PromptCache]

    classDef default fill:#f9f,stroke:#333,stroke-width:1px;
    classDef api fill:#bbf,stroke:#333,stroke-width:1px;
    classDef config fill:#f96,stroke:#333,stroke-width:1px;
    classDef runtime fill:#6f9,stroke:#333,stroke-width:1px;

    class A default
    class B default
    class C default
    class D runtime
    class E default
    class F config
    class G config
    class H runtime
    class I api
    class J api
    class K default
```