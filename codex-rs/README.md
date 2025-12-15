# Codex CLI (Rust Implementation)

We provide Codex CLI as a standalone, native executable to ensure a zero-dependency install.

## Installing Codex

Today, the easiest way to install Codex is via `npm`:

```shell
npm i -g @openai/codex
codex
```

You can also install via Homebrew (`brew install --cask codex`) or download a platform-specific release directly from our [GitHub Releases](https://github.com/openai/codex/releases).

## Documentation quickstart

- First run with Codex? Follow the walkthrough in [`docs/getting-started.md`](../docs/getting-started.md) for prompts, keyboard shortcuts, and session management.
- Already shipping with Codex and want deeper control? Jump to [`docs/advanced.md`](../docs/advanced.md) and the configuration reference at [`docs/config.md`](../docs/config.md).

## What's new in the Rust CLI

The Rust implementation is now the maintained Codex CLI and serves as the default experience. It includes a number of features that the legacy TypeScript CLI never supported.

### Config

Codex supports a rich set of configuration options. Note that the Rust CLI uses `config.toml` instead of `config.json`. See [`docs/config.md`](../docs/config.md) for details.

### Model Context Protocol Support

#### MCP client

Codex CLI functions as an MCP client that allows the Codex CLI and IDE extension to connect to MCP servers on startup. See the [`configuration documentation`](../docs/config.md#mcp_servers) for details.

#### MCP server (experimental)

Codex can be launched as an MCP _server_ by running `codex mcp-server`. This allows _other_ MCP clients to use Codex as a tool for another agent.

Use the [`@modelcontextprotocol/inspector`](https://github.com/modelcontextprotocol/inspector) to try it out:

```shell
npx @modelcontextprotocol/inspector codex mcp-server
```

Use `codex mcp` to add/list/get/remove MCP server launchers defined in `config.toml`, and `codex mcp-server` to run the MCP server directly.

### Notifications

You can enable notifications by configuring a script that is run whenever the agent finishes a turn. The [notify documentation](../docs/config.md#notify) includes a detailed example that explains how to get desktop notifications via [terminal-notifier](https://github.com/julienXX/terminal-notifier) on macOS. When Codex detects that it is running under WSL 2 inside Windows Terminal (`WT_SESSION` is set), the TUI automatically falls back to native Windows toast notifications so approval prompts and completed turns surface even though Windows Terminal does not implement OSC 9.

### `codex exec` to run Codex programmatically/non-interactively

To run Codex non-interactively, run `codex exec PROMPT` (you can also pass the prompt via `stdin`) and Codex will work on your task until it decides that it is done and exits. Output is printed to the terminal directly. You can set the `RUST_LOG` environment variable to see more about what's going on.

### Experimenting with the Codex Sandbox

To test to see what happens when a command is run under the sandbox provided by Codex, we provide the following subcommands in Codex CLI:

```
# macOS
codex sandbox macos [--full-auto] [--log-denials] [COMMAND]...

# Linux
codex sandbox linux [--full-auto] [COMMAND]...

# Windows
codex sandbox windows [--full-auto] [COMMAND]...

# Legacy aliases
codex debug seatbelt [--full-auto] [--log-denials] [COMMAND]...
codex debug landlock [--full-auto] [COMMAND]...
```

### Selecting a sandbox policy via `--sandbox`

The Rust CLI exposes a dedicated `--sandbox` (`-s`) flag that lets you pick the sandbox policy **without** having to reach for the generic `-c/--config` option:

```shell
# Run Codex with the default, read-only sandbox
codex --sandbox read-only

# Allow the agent to write within the current workspace while still blocking network access
codex --sandbox workspace-write

# Danger! Disable sandboxing entirely (only do this if you are already running in a container or other isolated env)
codex --sandbox danger-full-access
```

The same setting can be persisted in `~/.codex/config.toml` via the top-level `sandbox_mode = "MODE"` key, e.g. `sandbox_mode = "workspace-write"`.

### Managing the session history with `/hist`

The fullscreen TUI exposes a `/hist` slash command that lets you inspect and curate the entries stored in `~/.codex/history.jsonl` (see `core/src/message_history.rs`). Type the command in the input box just like any other prompt—Codex will intercept it locally without sending anything to the model. Indices in every subcommand are **1-based** and match the numbering rendered in the history view.

- `/hist ls [snapshots|assistant|reasoning|user]` — Shows the full history by default, or filters down to snapshots (saved Git states), assistant replies, reasoning traces, or user messages only.
- `/hist ll [count]` — Prints the last `count` items (defaults to 10) so you can spot recent artifacts quickly.
- `/hist la <index>` — Shows a small window around the indexed entry, which is useful when you only remember roughly where an event happened.
- `/hist view <index>` — Expands a single entry with its full JSON payload and tool outputs.
- `/hist del <index> | del-range <start>,<end> | del-before <index> | del-after <index> | del-last [count]` — Removes one item, a contiguous range, everything up to an index (inclusive), everything after an index, or the last *N* items. Use this to prune sensitive data before exporting sessions.
- `/hist compact <start>,<end>` — Spawns a background compaction task that asks Codex to summarize and collapse a noisy span of turns. When it finishes, the resulting summary replaces the selected range.
- `/hist undo [<index>]` — Reverts history edits starting at `index` (or the most recent edit if omitted). Any associated `GhostSnapshot` entries restore the saved working tree state as part of the undo.

Every destructive action emits a confirmation message in the transcript, and actions that mutate the stored history also rewrite the persisted rollout so the TUI, the `codex exec` CLI, and external integrations stay in sync.

### Managing chat sessions with `/chat`

Use `/chat` when you need to inspect or manipulate the persisted chat rollouts that live under `~/.codex`. Just like `/hist`, type the command straight into the composer—Codex intercepts it locally:

- `/chat ls` — Lists the 20 most recent chat sessions along with their creation times, followed by any saved checkpoints found in `~/.codex/checkpoints`.
- `/chat delete <id>` — Permanently removes the session whose UUID matches `id`.
- `/chat resume <tag>` — Restores a previously saved checkpoint (looked up by tag) or an archived session (looked up by UUID) and makes it the active conversation.
- `/chat share [path]` — Copies the currently loaded session to `path` (defaults to `codex-conversation-<timestamp>.json` in the CWD) so you can hand it off to other tools.
- `/chat save <tag>` — Snapshots the active session into `~/.codex/checkpoints/<hash>-<project>-<tag>.json`, letting you return to that state later via `/chat resume`.

Destructive operations confirm their outcome in the transcript, and Codex keeps the on-disk rollout synchronized so the CLI, TUI, and external integrations all see the same session state.

## Code Organization

This folder is the root of a Cargo workspace. It contains quite a bit of experimental code, but here are the key crates:

- [`core/`](./core) contains the business logic for Codex. Ultimately, we hope this to be a library crate that is generally useful for building other Rust/native applications that use Codex.
- [`exec/`](./exec) "headless" CLI for use in automation.
- [`tui/`](./tui) CLI that launches a fullscreen TUI built with [Ratatui](https://ratatui.rs/).
- [`cli/`](./cli) CLI multitool that provides the aforementioned CLIs via subcommands.
