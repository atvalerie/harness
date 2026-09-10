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
`response_final`, `tool_call`, `usage`, `notice`, `error`, and `run_finished`.
`text_delta` is intended for live rendering; `response_final` contains the
complete accumulated response and its `complete` flag is false when generation
was cancelled or failed. Reasoning events
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

For a persistent machine-to-machine session, combine `--chat` with JSONL:

```text
holiday.exe --chat --format jsonl --tools auto
```

Send one versioned input event per line on stdin. Speech events are submitted
as user turns; `partial`, `listening`, and `level` events are ignored as
non-submitted input, so a voice frontend can report live state without sending
unfinished transcripts to the model:

```json
{"version":1,"event":"speech","data":{"text":"open my project","sequence":42,"activation":"manual","input_kind":"command"}}
{"version":1,"event":"shutdown"}
```

The stdout stream uses the same versioned event envelope as headless JSONL.
Each submitted speech event gets its own `run_id`; its input metadata is
included in `run_started` and `user_message`, allowing a client to correlate
model text, tool calls, approvals, and tool results with the originating
utterance. Diagnostics should remain on stderr. Use `--tools ask` to deny tool
calls by default, or `--tools auto` only for a deliberately trusted bridge.

Send `{"version":1,"event":"new_session"}` between interactions to clear
conversation history and tool context while keeping the Holiday process,
provider connection, and configuration alive. Holiday responds with a
`session_reset` event.

Frontends may then send a generic runtime `session_config` event to attach
temporary instructions and session-scoped capability declarations to the new session:

```json
{"version":1,"event":"session_config","data":{"session_id":"frontend-42","system_instruction":"Use the frontend's session rules.","capabilities":["frontend.input.v1"]}}
```

Holiday acknowledges this with `session_configured`. The instruction is
appended to requests for the active session only; it is not added to the
global prompt, tool registry, configuration, or saved conversation. The
frontend remains responsible for interpreting and enforcing its capability
identifiers.

With `--tools ask`, a JSONL frontend receives a structured `approval_required`
event instead of an automatic denial. It includes an `approval_id`, the exact
tool preview, and a risk level. Complete it with a correlated response:

```json
{"version":1,"event":"approval_response","data":{"approval_id":"call-42","approved":true,"method":"keybind"}}
```

Approval IDs are single-use and must match the pending tool request. Mutating
actions are marked high-risk and cannot be approved with `method: "voice"`;
they require a deliberate keybind or UI confirmation. A denial or session
shutdown leaves the action unexecuted; a configurable timeout will be added at
the Atlas session-policy layer.

Session files are written atomically after each user prompt and completed assistant/tool turn; they are not rewritten for every token or streaming chunk. New snapshots retain the tool working directory and discovered `AGENTS.md`/`CLAUDE.md` project instructions, so restoring a session restores the project context as well.

Maintained prompts live in `src/prompts/`: `main.md`, `plan.md`, `subagent.md`, and `compaction.md`. They are embedded at build time; rebuilding after an update applies the current guidance even to existing configurations. The main request combines built-in guidance, user customization, labeled project guidance, and the active mode. Tool schemas remain the authority for arguments and availability, and permission enforcement remains in the runtime.

`/sys` displays the effective main prompt. `/sys <text>` sets user customization, `/sys --clear` clears it, and `/save` persists it. The configuration's `system_instruction` field now holds customization only. Versioned migration removes the exact recognized historical default; customized legacy prompts are preserved verbatim, so users can inspect `/sys` and replace obsolete custom wording when needed. Migration is saved with the next configuration save.

Subagents analyze supplied context without tools or independent repository access. Compaction preserves text attachments and image references (not image payloads), prioritizes active constraints and unfinished work, and retains the original history if the summarizer returns an empty result. Prompt changes should be checked with `cargo test`; these tests cover composition, migration, request integration, and compaction data handling, but do not measure live model behavior.
Session-scoped capability declarations are callable function schemas, not
merely labels. When the model calls one, Holiday emits a capability_request
event and pauses the run until the frontend sends a correlated
capability_response containing the request_id, capability name, and an
explicit result such as answered, no_input, cancelled, or error. The frontend
owns the capability side effect; these declarations never enter the global
tool registry or prompt.

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

## Optional Lightpanda browser

Holiday can connect to a local Lightpanda browser through its existing
streamable HTTP MCP transport. Lightpanda is an independent browser sidecar;
it is not a Holiday library dependency and it is not Atlas-specific. The same
MCP registry is attached in the TUI, one-shot, text-chat, and JSONL frontends,
so Atlas's JSONL bridge can use the configured browser tools too.

The intended Windows deployment is Lightpanda running in Arch Linux under
WSL2 with telemetry disabled:

    export LIGHTPANDA_DISABLE_TELEMETRY=true
    lightpanda mcp --host 127.0.0.1 --port 9223

Add the service to the Holiday configuration once the sidecar is running:

    "lightpanda": {
      "transport": "streamable-http",
      "url": "http://127.0.0.1:9223/mcp",
      "enabled": true
    }

Lightpanda tools remain optional and hidden from ordinary requests until a
profile or tool-discovery flow activates them. Atlas should preload only a
curated read-only browser profile for voice sessions. Clicks, form submission,
authentication, cookies, and other state-changing browser operations must
remain approval-gated.

Lightpanda controls websites and DOMs, not native Windows applications or
arbitrary screen pixels. The future Windows computer-automation layer is a
separate integration.

## Provider credentials and model metadata

API keys saved with `/key` are now scoped to the selected provider and stored only
in the native credential store (Windows Credential Manager, macOS Keychain, or
Linux Secret Service). Storage errors are reported; new plaintext fallback files
are not written. On headless Linux without an unlocked Secret Service, use the
provider's configured `api_key_env` instead.

**Migration:** legacy shared `.key` files and the unscoped `api_key` keyring entry
are no longer read automatically: they contain no indication of which provider
owns the key. Select the intended provider and enter `/key` again, or configure its
environment variable. Existing files are not deleted. Legacy provider-specific
Codex credential files remain readable for compatibility; newly saved credentials
use the secure store.

A provider with no configured or remembered model now requires an explicit model
selection rather than inheriting the previous provider's model. An empty
`providers.<name>.fallback_models` list disables fallbacks for that provider.
Provider, endpoint, and key changes refresh model metadata; outdated asynchronous
catalog results are ignored. Headless generation also attempts a bounded metadata
lookup before constructing its request.

### Context limits for compatible providers (including BearLab)

Catalog context limits, input-only limits, and output limits are distinct. When a
provider's `/models` response omits limits, Holiday does **not** infer them from the
model name. Set a verified provider/model-scoped override in `model_profiles`:

```json
{
  "model_profiles": {
    "bearlab:YOUR_MODEL_ID": {
      "context_window": 128000,
      "max_output_tokens": 8192
    }
  }
}
```

The numbers above are illustrative, **not verified BearLab limits**. Replace them
with limits confirmed for your endpoint and model. `context_window` is the combined
input/output window; `input_token_limit` can be set separately for a provider that
also limits input. Positive explicit context overrides take precedence over catalog
metadata. Output budgets are clamped to a catalog's advertised output maximum.
Token counts remain estimates, not a model-specific tokenizer calculation.

Streaming now treats an unexpected EOF, malformed tool arguments, and explicit
incomplete responses as errors rather than successful completion. Generation uses
a connection deadline and a read-idle timeout rather than a fixed 120-second total
lifetime; catalog requests retain a bounded total timeout.


## Interactive workspace

### Commands, palette, and composer

- **Ctrl+K** or `/palette`: searchable command palette. Enter inserts the selected
  command; press Enter in the composer to execute it. `/help`, command dispatch,
  aliases, and command-name completion share `src/commands.rs`.
- **Tab** completes command names and provider/model arguments. `/attach` and
  `/resume` also offer filesystem completions. Ambiguous command names open the
  palette; ambiguous arguments show candidates in the status line. Paths with
  spaces can be quoted.
- **Ctrl+R** or `/history TEXT`: search previous prompts and restore one without
  immediately sending it. `/key` is deliberately excluded from prompt history.
- **Ctrl+Z / Ctrl+Y**: undo/redo draft edits. **Ctrl+Left / Ctrl+Right** move by word;
  **Ctrl+Backspace** deletes the preceding word. Horizontal editing respects
  grapheme boundaries; wrapping uses terminal display width.
- `/editor`: edit the text draft in `$VISUAL` or `$EDITOR`, defaulting to Notepad on
  Windows and `vi` elsewhere. The variable must be an executable name/path, not a
  shell command with arguments. Attachment blocks remain separate. This is an
  interactive-terminal action, not a headless command.
- Enter while the engine is busy queues the prompt and attachments. `/queue TEXT`
  queues explicitly; `/interrupt TEXT` cancels the current turn and sends new text.
  **Ctrl+G** cancels without sending a new prompt. Queued prompts run in FIFO order
  once the interactive engine becomes idle. Drafts are retained when switching
  between sessions within the running application.

Provider/model changes and destructive conversation commands are marked **idle
only** in help. They are rejected during generation rather than mutating an active
request. Overlay state is centralized; reasoning settings can return to the model
picker, and Escape closes the active overlay.

### Transcript and context

Transcript items show stable-in-the-current-history numbers. `/inspect N` toggles
expanded details; expanding a tool record also reveals its provider output.
Reasoning summaries are collapsed until expanded. `/search TEXT` filters the
transcript, and `/search` clears the filter. `/jump N`, `/jump prev`, `/jump next`,
and `/jump error` focus a message, adjacent user turn, or latest error; Alt+Up/Down
navigate user turns. `/search` also clears this focus. `/copy N` copies a message;
`/copy N code B` copies its B-th fenced code block. `/copy` copies the latest answer.
Clipboard failures are reported instead of silently claiming success.

Scrolling up anchors the viewport while new output arrives. Completed Markdown
is cached, only viewport lines are submitted to the terminal widget, and idle
rendering no longer rebuilds frames at 60 FPS. Active animations use a bounded
12.5 FPS tick. Terminal polling runs on a dedicated input thread, paused while the
external editor owns the terminal.

`/context` now includes an estimated breakdown of instructions, conversation,
tool history, schemas and text attachments; image count; output reserve; metadata
source; and requested versus actually selected model/protocol. JSONL consumers
also receive a `model_selected` event. Usage records use the actual selected model
when the provider falls back. Estimates are not exact tokenizer measurements.

### Managed tasks and scoped workers

`/tasks` opens the task panel. Type `task-N` to inspect its bounded live output,
or `cancel task-N` and Enter to request cancellation. The equivalent commands are
`/tasks task-N` and `/tasks cancel task-N`. Model generations, tool calls, shell
processes, and workers have task records. Completed records are pruned as new
records arrive; active tasks remain inspectable.

Background `run_command` calls no longer discard output or merely detach a PID.
They return a managed task ID. Shell stdout/stderr is drained continuously with
bounded retention. Timeouts and cancellation terminate the tracked shell, with
Windows `taskkill /T` or Unix process-group termination for descendants. These are
non-interactive commands (stdin is closed), not a PTY/sandbox or a guarantee of
control over deliberately detached processes. Background tasks are still subject
to their configured timeout and end when the harness runtime exits.

Workers inherit only filesystem tools whose parent permission is explicitly
`allow`. They cannot grant approvals, invoke shell/MCP tools, or escape the recorded
workspace root. A worker is read-only unless `spawn_agent` receives `write_paths`,
a list of exact files, and the parent already allows workspace writes. Overlapping
active worker write scopes are rejected. Workers have a 12-tool-round limit and
an approximate 64k-token serialized-input budget per work cycle. Stop requests
interrupt generation rather than waiting for a model response. This implementation
uses shared-workspace file scopes, **not isolated Git worktrees**; parent shell
commands and unrelated external editors are not isolated by those scopes.

### File review and recovery

`/review` opens checkpoint history; type a checkpoint number to inspect its diff.
`/rollback N` restores that file checkpoint only if its current bytes still match
the checkpoint's recorded after-state and it remains in the current workspace.
It does **not** rewind conversation state, run Git reset, or overwrite detected
later edits. `/fork`, `/retry`, and `/clear` operate on conversation state only.

Automatic checkpoints cover `write_file` and `edit_file` within the workspace,
including edits made through worker tools. Files above 2 MB or paths whose parent
cannot be resolved cannot be checkpointed, and these tool calls fail before the
edit. Arbitrary shell commands, MCP side effects, directory operations, and moves
are **not** automatically reversible. Checkpoints retain file bytes, not original
filesystem metadata/ACLs; rollback uses optimistic conflict detection, not an OS
transaction against concurrent external editors.

Session persistence now includes an append-only `.events.jsonl` write-ahead log,
with incremental history records and periodic full checkpoints. A truncated final
record is ignored during recovery. `.changes.jsonl` separately persists file
checkpoints, including worker changes that complete while the parent is idle.
Keep these sidecars with the session JSON. Export uses recovered current state;
deleting a session also removes its journals. Journals contain conversation and
file contents and should be treated as sensitive. They currently have no automatic
disk-retention policy. Live process handles are not restored after a crash.

### BearLab metadata fallback

For a provider named `bearlab` (or containing that name), exact model-ID matches
can inherit missing context, output, and reasoning metadata from the Codex catalog.
An explicit mapping can also be configured for any compatible provider:

```json
{
  "model_profiles": {
    "bearlab:YOUR_GATEWAY_MODEL": {
      "codex_model": "EXACT_CODEX_CATALOG_MODEL_ID"
    }
  }
}
```

No approximate name matching is performed. Gateway limits are retained, and
explicit profile overrides take precedence. The source is labelled as a Codex
fallback, not a verified gateway guarantee. The resolver uses a cached catalog or
independently stored Codex OAuth credentials for a bounded refresh; it **never sends
BearLab credentials to Codex**. Without a catalog or Codex credentials, missing
limits remain unknown and explicit overrides continue to work.

### Development checks

```text
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

Tests include command aliases/quoted paths, overlay rendering at small terminal
sizes, draft undo/redo, queueing, stale catalogs, scoped path rejection, optimistic
rollback, journal recovery, and actual managed-process cancellation. Live provider
compatibility, native clipboard/editor integrations, and all target operating
systems still need end-to-end verification before a release.


### Recovery and transcript controls

- Typing `/` displays live command suggestions above the prompt. Up/Down selects,
  Tab inserts, and Esc dismisses. Enter submits the command as typed. Ctrl+K still
  opens the full searchable palette.
- Message IDs appear beside timestamps, e.g. `12:34:56 #20`. Use `/inspect 20`
  (alias `/expand 20`) to toggle expansion and focus that item. Tool expansion
  shows recorded arguments and readable output, within the persisted output cap.
  Ctrl+O toggles the focused item, or the latest tool/thought/assistant item when
  no item is focused. `/search` with no arguments clears the focused view.
- `/retry` now retries the continuation with conversation and completed tool
  results preserved. It does not delete the previous assistant/tool turn or undo
  filesystem changes. A provider failure is not evidence that a tool's side effects
  were undone.
- Approval panels show the shell tool's reason, or the model's most recent public
  narration in the current user turn. Missing explanations are labeled explicitly;
  private reasoning is never substituted. Shell schemas require `reason` and
  `expected_effect`; other MCP tool schemas are passed through unchanged.
- MCP startup failures are recorded in the transcript. Streamable HTTP supports
  incremental SSE responses, negotiated protocol headers, session IDs and paginated
  tool discovery. Legacy HTTP+SSE (`transport: "sse"`) is not implemented; use a
  server's Streamable HTTP endpoint rather than its legacy SSE endpoint.


### Bounded compaction and unattended review

Compaction now makes **one request**, with no automatic retry or model fallback and a
**60-second total deadline**. A 504 may mean the provider is still processing the
request; retry explicitly with `/compact` rather than silently duplicating it.
Esc cancels the local task; late results cannot overwrite the conversation. Failure,
empty output, or an incomplete checkpoint retains the original history. The handoff
prompt prioritizes authorization, the next step, verification evidence and pending
non-idempotent operations. Local cancellation cannot guarantee provider cancellation.

For selective unattended execution, open `/permissions` and cycle the desired groups
to **REVIEW**. Policies are: ASK (human), ALLOW (no review), REVIEW (model review),
and DENY (blocked). Save the permissions screen to persist the choices. REVIEW is
opt-in; existing policies are unchanged. It ignores session-wide tool allowances and
reviews each invocation separately. Plan-mode mutation restrictions and DENY still win.

For the JSONL frontend, use `--tools review`, for example:

```powershell
holiday.exe --chat --format jsonl --tools review
```

This uses the exact `codex-auto-review` model on the **active provider**, with a
30-second deadline, no tools, and no retries or fallback to another model. The provider
must expose that model using its configured protocol. Missing model support, timeout,
malformed decisions, or more than 160 KB of review input require human approval; context
is not silently truncated. No live-provider availability is assumed.

The reviewer evaluates user intent, exact scope, risk, reversibility and whether the
step is worth taking. Only sufficiently authorized low/medium-risk actions can be
auto-approved. High-risk, ambiguous or consequential actions pause with a concrete
question; explicit violations are denied. JSONL emits `auto_review` and, when needed,
`approval_required`. Reply using the matching approval ID, for example:

```json
{"event":"approval_response","approval_id":"<from approval_required>","approved":true,"method":"text"}
```

Review escalations require `text`, `keybind`, or `ui`, not voice or an unspecified
method. TUI users can approve the exact displayed action once or reject it; choosing
session allowance does not whitelist REVIEW actions. No response means no execution.
`--tools auto` retains its legacy unconditional approval behavior for non-REVIEW gates;
it is **not** the safe unattended-review mode. A model reviewer reduces interruptions,
but is not an OS sandbox or a guarantee that arbitrary commands are safe.


### Quick global auto-review toggle

Keep your usual permission groups on ASK, then use:

- `/auto on` ? temporarily review **ASK groups** with `codex-auto-review`,
  ALLOW runs directly and DENY stays blocked. The status bar shows **AUTO REVIEW**.
- `/auto off` ? restore your normal group policies without rewriting settings.
- `/auto` or `/auto status` ? show the current mode.

Toggle while idle; stop an active turn first. This does not retroactively approve a
pending action or bypass DENY/plan mode. High-risk and uncertain actions still pause
for confirmation. Session-wide allowlists cannot bypass this override. Scoped workers inherit only explicitly ALLOW-listed filesystem tools; ASK and REVIEW
tools are not granted to workers.

The toggle is runtime-only, starts off on launch, and resets on `/new`. It is not
saved in config or session snapshots. `/auto off` restores your configured policies,
not force-manual behavior: groups already on ALLOW or REVIEW keep those settings.
`--tools review` enables the same override for JSONL runs without changing group settings.

Auto-review uses a separate, stateless request, not the coding-agent conversation.
It sends an explicit review task and policy, a JSON-escaped evidence packet, the exact
planned action, and a final reminder to assess rather than execute. No coding-agent
system prompt, private reasoning, assistant dialogue or attachment payloads are replayed.
Authorization messages, previous summaries and recorded denials/review notices are kept
within a 48 KB budget; excess authorization evidence requires human confirmation rather
than silently dropping constraints. At most six recent tool records (16 KB total) are
included, with explicit omission counts. Prior summaries are evidence, not new consent.

The response uses Guardian-style fields: `outcome`, `risk_level`, `user_authorization`,
and `rationale`. Holiday retains its `ask` outcome and requires the full assessment;
it does not infer low risk from a bare `{"outcome":"allow"}`. Existing authorization and
high-risk confirmation rules remain unchanged. This is an adaptation of the public
Codex contract, not a copy of its complete permission policy or execution environment.

The client requests a strict JSON schema via Responses `text.format` or Chat Completions
`response_format`, according to the configured protocol. Unsupported schema requests fail
closed without retries, protocol switching or fallback models. Plain JSON or a single
JSON code fence is accepted; arbitrary prose and incomplete decisions are not approvals.
Errors report response shape and size, not raw potentially sensitive output. Already-ALLOW
actions never call the reviewer. Live compatibility of the new request format still needs
verification; the prior four-request diagnostic does not validate this revision.
