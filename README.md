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
      "api_key_env": "OLLAMA_API_KEY"
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

Useful commands include `/provider`, `/baseurl`, `/model`, `/reasoning`, `/autocompact`, `/session save`, `/session path`, `/models`, and `/save`.

For scripting, use headless mode:

```text
gemini-harness.exe -p "Summarize this" --model gemini-3.8-flash
```

Headless mode writes only the model response to stdout; diagnostics and failures go to stderr.

Session files are written atomically after each user prompt and completed assistant/tool turn; they are not rewritten for every token or streaming chunk.
