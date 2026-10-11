# MCP client examples (F5)

The LightVM MCP adapter is `lightvm-mcp` (crate `crates/lightvm-mcp`): the
MCP client launches it over **stdio**; protocol revision **2025-11-25** only.

No file here contains a credential. The adapter takes **no argument**: it
reads the pairing file that the LightVM app writes when you click
**Agents → Pair a client…** (`%LOCALAPPDATA%\LightVM\pairing\pairing.json` on
Windows, in a folder only your account can read; the adapter refuses the
file otherwise). `LIGHTVM_PAIRING_FILE` may name another file of the same
format; it is a path, never the credential. The pairing credential expires
after 10 minutes if no client used it, and grants nothing by itself.

Flow, for any client:

1. In the app: turn agent access on and click *Pair a client…*.
2. Start the client with the configuration below.
3. Ask the model to work in the VM: it calls `session_request` with the
   objective and shows you a verification code.
4. In the app, check that the code matches, pick the VM, the capabilities,
   the duration, and approve; hand over mouse/keyboard control if needed.
5. Revoke or pause from the app at any time; the report of the session is
   in the app (*Sessions*), with its outcome decided by a verifier or by you
   (never by the model's own declaration).

LightVM limits what the client can do **through LightVM**: it cannot take
away the shell, files or other tools the client already has (ROADMAP 7.1).
For a session meant to act only in the VM, restrict the client's own tools
too, as the Claude Code example does.

## Clients

| Client | Version tested | Status | Files |
| --- | --- | --- | --- |
| Claude Code (CLI) | 2.1.220 | configuration and invocation prepared; the real run on 2026-10-11 could not start (the CLI was logged out: `claude auth status` → `loggedIn: false`). See `docs/verification/F5-01-mcp-reports.md`. | `claude-code/` |
| pi coding agent | 0.85.0 | **no native MCP support** (no `--mcp-config` or equivalent; MCP would need a third-party extension, not installed): no configuration published (ADR-0021: examples only for clients actually tested) | `pi/README.md` |

### Claude Code

`claude-code/mcp.json` (adapter at its installed path; adjust `command`).
Headless run restricted to the LightVM tools only:

```text
claude -p "<objective>" --mcp-config examples/mcp/claude-code/mcp.json --strict-mcp-config \
  --tools "" --allowedTools mcp__lightvm --permission-mode dontAsk \
  --no-session-persistence --output-format stream-json --verbose --max-budget-usd 4
```

- `--tools ""` removes the built-in tools (shell, files, web) for this run;
  `--allowedTools mcp__lightvm` allows the LightVM tools only;
  `--strict-mcp-config` ignores other MCP servers of your settings.
- Tool names are `vm_list`, `vm_screenshot`, … (the roadmap's `vm.list`,
  `vm.screenshot`, … with `_` because model APIs refuse `.` in tool names).

`claude-code/mcp-custom-pairing-file.json` shows `LIGHTVM_PAIRING_FILE`
(a path; `<you>` is a placeholder).
