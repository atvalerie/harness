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

Any entry under `providers` becomes a selectable provider by name. Set `kind` to `gemini` for Gemini's native API; OpenAI-compatible providers can set `protocol` to `auto`, `chat-completions`, or `responses`. `auto` selects the documented Zen protocol by model; custom providers should set `responses` when their endpoint is `/responses`. Set `base_url`, `api_key_env`, `model`, `models`, `fallback_models`, optional `headers`, and `stream_usage` per provider. OpenAI-compatible providers may omit `api_key_env` for local servers that do not require authentication. Select one with `/provider <name>` or `--provider <name>`.

OpenCode Zen is included as the `opencode-zen` provider. Set `OPENCODE_API_KEY`, run `/provider opencode-zen`, then `/models`: the harness queries Zen's live `https://opencode.ai/zen/v1/models` catalog instead of baking the changing model list into the executable. The built-in model is only a bootstrap choice; use the model picker to choose the current free model(s). Zen's optional streaming usage event is disabled for this provider for compatibility with gateways that reject `stream_options`.

Useful commands include `/providers`, `/provider <name>`, `/baseurl`, `/config path`, `/config open`, `/model`, `/reasoning`, `/autocompact`, `/session save`, `/session path`, `/models`, and `/save`.

Use `--config PATH` or the `GEMINI_HARNESS_CONFIG` environment variable to run with a separate configuration file. This is useful for keeping local, work, and hosted-provider setups independent.

For scripting, use headless mode:

```text
gemini-harness.exe -p "Summarize this" --model gemini-3.5-flash-lite
```

Headless mode writes only the model response to stdout; diagnostics and failures go to stderr.

For integrations that need streaming, use JSON Lines mode. Each line is a
versioned event with a sequence number, run ID, timestamp, and event payload:

```text
gemini-harness.exe -p "Summarize this" --format jsonl
```

Events include `run_started`, `user_message`, `reasoning_delta`, `text_delta`,
`tool_call`, `usage`, `notice`, `error`, and `run_finished`. Reasoning events
represent provider-emitted reasoning summaries when available; they are not a
guarantee of hidden chain-of-thought access.

For a persistent text-only back-and-forth session:

```text
gemini-harness.exe --chat
```

Enter one prompt per line. Use `/exit` or `/quit` to end the session. The conversation is saved using the same turn-boundary session persistence as the TUI.

Session files are written atomically after each user prompt and completed assistant/tool turn; they are not rewritten for every token or streaming chunk.
They also retain per-request token usage, including whether a record was
provider-reported or estimated. Use `/usage` inside the TUI to inspect totals.
