# F5-04 — Two real LLM clients complete the fixture

Date: 2026-10-18. Windows/WHPX, approved Ubuntu 26.04.1 live ISO,
4 vCPUs, 4096 MiB, offline network and the ADR-0019 fixture ISO.
**Both clients passed the complete scenario**, including an independent
pixel verdict and the absence of the marker after baseline restore.

| Client | Model | Agent interaction | Complete test | Result |
| --- | --- | --- | --- | --- |
| Claude Code 2.1.302 | `claude-opus-5-5`, observed in its transcript | 34.30 s | 493.92 s | passed |
| Codex CLI 0.163.0-alpha.10.2 | CLI default; no model override, ID not emitted | 42.18 s | 497.68 s | passed |

One attempt per client; no model retry or successful attempt selected from
hidden failures. Both used their existing authenticated accounts. Claude's
reported cost was USD 0.2084818, below the harness's USD 4 limit. Codex used
the existing ChatGPT login; no dollar cost was emitted.

## Actual path

`crates/lightvm-mcp/tests/f5_real_client.rs::e2e_real_client_runs_the_fixture`
starts the production CoreEngine/service and the actual `lightvm-mcp`
executable, driven over stdio by the selected real client. The harness
creates an empty-disk baseline, boots the live desktop and then asks the
model to request a session, inspect screenshots, open a terminal, type
the fixture command, inspect the result and declare its outcome.

The harness approves the request and hands over input through UI-role IPC,
standing in for the user's click in Agents. It does not type the fixture
command for the model. Both models declared `passed`; revocation terminated
their sessions, and the independent fixture verifier found the required
four blocks and the yellow marker bar in each final screenshot. Each report
retains three screenshots and records `outcome=passed`,
`outcome_source=verifier`, `termination=revoked`.

The last verified screenshots were:

- Claude: SHA-256 prefix `a065fa472ba1a646`, 99 session events.
- Codex: SHA-256 prefix `bc34940501681013`, 98 session events.

After each client exited successfully, the harness stopped its VM,
restored the baseline, booted again and independently checked
`fx.sh mostra -> Ok(Absent)` (V6). Audits contained 978 and 1001 events;
neither contained the typed `fx.sh` command. JSON and Markdown reports,
screenshots and client transcripts were exported.

The harness now fails if there is no session, no declaration/revocation,
a failed client exit, or no independently verified marker, even when the
final restore succeeds. Previously that last restore alone could allow
an unsuccessful real-client attempt to produce a passing test result.

## Configuration and reproduction

Claude retains the restricted configuration from F5-01: only LightVM MCP,
no built-in tools, `dontAsk`, no session persistence and the budget limit.
Codex uses invocation-local MCP settings with `--ignore-user-config`,
`--ephemeral`, `--sandbox read-only`, shell/subagents/plugins disabled and
web search disabled. The pairing file remains private to the disposable
catalog. Neither client's permanent configuration was changed. The
Codex invocation follows the official [non-interactive mode](https://developers.openai.com/codex/noninteractive)
and [MCP configuration](https://developers.openai.com/codex/mcp) documentation;
the installed CLI's help/features output was also checked.

Use the same QEMU/ISO environment as F5-01 and select
`LIGHTVM_F5_CLIENT=claude` or `codex`, `LIGHTVM_F5_CLIENT_EXE` as the actual
CLI path, `LIGHTVM_F5_EVIDENCE_DIR` on E: and a distinct `LIGHTVM_F5_RUN`.
Run the named ignored test with `--ignored --exact --nocapture --test-threads=1`.
The adapter must be built first. `LIGHTVM_F5_MODEL` is optional; neither
attempt set it. Temporary files and build output used E:.

Evidence: `E:\OpenVM-tests\f5-clients-20261018`, subdirectories `claude-1`
and `codex-1`, both console logs, the harness source, binary hashes and
versions in `provenance.json`, and a verified `SHA256SUMS.json` inventory.
The transcripts were checked for credential/token/authorization keys;
none were found. No owned QEMU/MCP process remained after completion.

## What this closes

This closes F5-01's outstanding **successful path with each of two real
LLM clients**. The ten infrastructure attempts and outcome-state matrix
remain the separate scripted evidence in F5-03; these two attempts are
not a claim of ten model-driven runs. Human interaction with the Agents UI,
other clients/models, installed guest applications and earlier phase exit
conditions are not qualified by this test. No overall release is declared.

Concurrent code-review fixes to the MCP adapter and other components began
after these measured runs. The retained executable hashes identify the
versions actually exercised; this evidence does not certify later uncommitted
review changes or a future release build.
