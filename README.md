# Holiday

Holiday supports Gemini, direct ChatGPT/Codex authentication and requests, OpenAI-compatible APIs, per-model reasoning profiles, resumable sessions, automatic compaction, and MCP tools.

Configuration is saved with `/save` under the platform config directory. A compact example:

```json
{
  "provider": "openai",
  "model": "gpt-4o-mini",
  "status_bar_position": "top",
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
      "headers": { "X-Provider-Client": "holiday" }
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

Any entry under `providers` becomes a selectable provider by name. Set `kind` to `gemini` for Gemini's API; use `codex` for direct ChatGPT/Codex authentication, or `openai-compatible` for compatible endpoints. OpenAI-compatible providers can set `protocol` to `auto`, `chat-completions`, or `responses`. `auto` selects the documented Zen protocol by model; custom providers should set `responses` when their endpoint is `/responses`. Set `base_url`, `api_key_env`, `model`, `models`, `fallback_models`, optional `headers`, and `stream_usage` per provider. OpenAI-compatible providers may omit `api_key_env` for local servers that do not require authentication. Select one with `/provider <name>` or `--provider <name>`.

Set the top-level `status_bar_position` to `"top"` or `"bottom"` to place the status bar above the chat or at the bottom of the terminal below the input. It defaults to `"top"`.

Provider-specific authentication, request headers, model catalogs, wire-protocol selection, and account usage formatting are isolated behind adapters. Adding another limited platform can therefore introduce a new adapter while leaving the chat, model picker, reasoning controls, and usage UI shared.

OpenCode Zen is included as the `opencode-zen` provider. Set `OPENCODE_API_KEY`, run `/provider opencode-zen`, then `/models`: Holiday queries Zen's live `https://opencode.ai/zen/v1/models` catalog instead of baking the changing model list into the executable. The built-in model is only a bootstrap choice; use the model picker to choose the current free model(s). Zen's optional streaming usage event is disabled for this provider for compatibility with gateways that reject `stream_options`.

Codex support is direct HTTP and does not launch Codex or use stdin. Run `holiday.exe --login browser` for the local OAuth callback flow or `holiday.exe --login device` for device-code login. Holiday stores the access and refresh credentials in the Holiday keyring namespace, selects the built-in `codex` provider, fetches its live model catalog, and uses the account's Codex backend. Use `/limits` to show the authenticated 5-hour and weekly usage windows.

Useful commands include `/help`, `/status`, `/context`, `/pwd`, `/tools`, `/providers`, `/provider <name>`, `/baseurl`, `/config path`, `/config open`, `/model`, `/reasoning`, `/autocompact`, `/compact`, `/session save`, `/session path`, `/sessions`, `/resume`, `/retry`, `/fork`, `/models`, `/clear`, `/new`, and `/save`.

The interactive composer accepts `Ctrl+Enter` or `Ctrl+J` for newlines. Multiline or large pasted text is kept as an editable draft block instead of flooding the prompt; `Ctrl+V` can attach a Windows clipboard image, and `/attach <path>`, `/attachments`, `/edit <n>`, and `/remove <n>` manage blocks. `Ctrl+X` removes the last draft block. Tool results are shown as compact one-line activity records while their full provider response remains in the conversation state.

The model picker remembers the last model used for each provider across new chats and relaunches. Press `K` in the model picker, or use `/thinking`, to choose the model's supported reasoning level (`off`, `low`, `medium`, `high`, `xhigh`, or `max` where supported). Gemini 3 requests use `thinkingLevel`; Gemini 2.5 compatibility uses the provider's legacy budget mapping; OpenAI-compatible and Codex reasoning models use the provider's reasoning setting.

Use `--config PATH` or the `HOLIDAY_CONFIG` environment variable to run with a separate configuration file. This is useful for keeping local, work, and hosted-provider setups independent.

The interactive TUI opens without requiring an API key, so you can explore commands and settings first. Add a key later with `/key <provider_api_key>` or the active provider's environment variable. Headless requests still require credentials when the selected provider requires them.

For scripting, use headless mode:

```text
holiday.exe -p "Summarize this" --model gpt-4o-mini
```

Headless mode writes only the model response to stdout; diagnostics and failures go to stderr. Continue the newest saved session with `--continue`, or select one by name/path with `--resume NAME_OR_PATH`.

For integrations that need streaming, use JSON Lines mode. Each line is a
versioned event with a sequence number, run ID, timestamp, and event payload:

```text
holiday.exe -p "Summarize this" --format jsonl --tools auto
```

Events include `run_started`, `user_message`, `reasoning_delta`, `text_delta`,
`tool_call`, `usage`, `notice`, `error`, and `run_finished`. Reasoning events
represent provider-emitted reasoning summaries when available; they are not a
guarantee of hidden chain-of-thought access.
Use `--tools auto` to execute declared tools, `--tools deny` to return tool
errors without executing them, or leave the default `--tools ask` to emit an
`approval_required` event and deny the call in non-interactive mode.

For a persistent text-only back-and-forth session:

```text
holiday.exe --chat --continue
```

Enter one prompt per line. The text-only mode accepts the same useful commands as the TUI, including `/help`, `/status`, `/context`, `/compact`, `/resume`, `/retry`, `/fork`, `/clear`, `/new`, `/save`, and `/exit`. The conversation is saved using the same turn-boundary session persistence as the TUI.

Session files are written atomically after each user prompt and completed assistant/tool turn; they are not rewritten for every token or streaming chunk. New snapshots retain the tool working directory and discovered `AGENTS.md`/`CLAUDE.md` project instructions, so restoring a session restores the project context as well.

Maintained prompts live in `src/prompts/`: `main.md`, `plan.md`, `subagent.md`, and `compaction.md`. They are embedded at build time; rebuilding after an update applies the current guidance even to existing configurations. The main request combines built-in guidance, user customization, labeled project guidance, and the active mode. Tool schemas remain the authority for arguments and availability, and permission enforcement remains in the runtime.

`/sys` displays the effective main prompt. `/sys <text>` sets user customization, `/sys --clear` clears it, and `/save` persists it. The configuration's `system_instruction` field now holds customization only. Versioned migration removes the exact recognized historical default; customized legacy prompts are preserved verbatim, so users can inspect `/sys` and replace obsolete custom wording when needed. Migration is saved with the next configuration save.

Subagents analyze supplied context without tools or independent repository access. Compaction preserves text attachments and image references (not image payloads), prioritizes active constraints and unfinished work, and retains the original history if the summarizer returns an empty result. Prompt changes should be checked with `cargo test`; these tests cover composition, migration, request integration, and compaction data handling, but do not measure live model behavior.
They also retain per-request token usage, including whether a record was
provider-reported or estimated. Use `/usage` inside the TUI to inspect totals.

Specialized and MCP tools are kept registered but hidden from normal requests
to reduce context usage. Ask the model to search for a capability when needed;
matching full schemas are enabled on the following turn. The built-in
edit_file tool applies precise exact-text patches with the normal diff review.
Shell commands support timeout_seconds, max_output_chars, and background.

Use /plan for a read-only planning pass and /todos or /todo add ... for
session-persistent tasks. In-process agents are available through spawn_agent,
agent_status, agent_inspect, message_agent, and kill_agent; they retain a
bounded conversation and can be given follow-up instructions without copying
the parent transcript.

In the TUI, a structured plan automatically opens a Plan Ready prompt after
generation. Select Execute plan or Keep planning with Up/Down and Enter.
The slash commands remain available for text-only mode and automation.

Plan mode is an approval workflow: use /plan to enter read-only planning,
/plan continue to request another planning turn, and /plan off to approve a
properly formatted ## Plan with numbered steps and begin execution. TODOs can
be managed by the model through the todo tool or by the user with /todo.
Set /todo mode next or /todo mode force to either deliver user TODO changes
on the next model turn or interrupt the current generation and apply them now.
