# Gemini Harness

The harness supports Gemini and OpenAI-compatible APIs, per-model reasoning profiles, resumable sessions, automatic compaction, and MCP tools.

Configuration is saved with `/save` under the platform config directory. A compact example:

```json
{
  "provider": "openai",
  "model": "gpt-4o-mini",
  "providers": {
    "openai": {
      "kind": "openai-compatible",
      "base_url": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY",
      "fallback_models": ["gpt-4o-mini"]
    },
    "local": {
      "kind": "openai-compatible",
      "base_url": "http://localhost:11434/v1",
      "api_key_env": "OLLAMA_API_KEY",
      "model": "qwen3:8b",
      "models": ["qwen3:8b", "llama3.2:3b"],
      "fallback_models": ["llama3.2:3b"],
      "headers": { "X-Provider-Client": "gemini-harness" }
    },
    "openrouter": {
      "kind": "openai-compatible",
      "base_url": "https://openrouter.ai/api/v1",
      "api_key_env": "OPENROUTER_API_KEY",
      "model": "meta/muse-spark-1.3-contributor",
      "models": ["meta/muse-spark-1.3-contributor"],
      "fallback_models": []
    }
  },
  "model_profiles": {
    "openai:gpt-4o-mini": {
      "temperature": 0.2,
      "reasoning_enabled": false,
      "max_output_tokens": 4096
    }
  },
  "mcp_servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:/work"],
      "enabled": true
    },
    "remote-tools": {
      "transport": "streamable-http",
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer <token>" },
      "enabled": true
    }
  }
}
```

Any entry under `providers` becomes a selectable provider by name. Set `kind` to `gemini` for Gemini's native API; all other kinds use the OpenAI-compatible `/chat/completions` protocol. Set `base_url`, `api_key_env`, `model`, `models`, `fallback_models`, and optional `headers` per provider. OpenAI-compatible providers may omit `api_key_env` for local servers that do not require authentication. Select one with `/provider <name>` or `--provider <name>`.

Useful commands include `/provider`, `/baseurl`, `/config path`, `/config open`, `/model`, `/reasoning`, `/autocompact`, `/session save`, `/session path`, `/models`, and `/save`.

Use `--config PATH` or the `GEMINI_HARNESS_CONFIG` environment variable to run with a separate configuration file. This is useful for keeping local, work, and hosted-provider setups independent.

For scripting, use headless mode:

```text
gemini-harness.exe -p "Summarize this" --model gemini-3.5-flash-lite
```

Headless mode writes only the model response to stdout; diagnostics and failures go to stderr.

For a persistent text-only back-and-forth session:

```text
gemini-harness.exe --chat
```

Enter one prompt per line. Use `/exit` or `/quit` to end the session. The conversation is saved using the same turn-boundary session persistence as the TUI.

Session files are written atomically after each user prompt and completed assistant/tool turn; they are not rewritten for every token or streaming chunk.
