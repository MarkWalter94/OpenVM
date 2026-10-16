# ADR-0036 — Guest UI inspection for agents (accessibility tree) and real LLM clients

- Status: Accepted (2026-10-15): direction set; evidence pending
- Date: 2026-10-15
- Owner: project owner (decision); implementation recorded below

## Context

The MCP tools drive the guest visually (`vm.screenshot`, mouse, keyboard): a client needs a multimodal model, and a text-only model cannot use them. No real LLM client has run against LightVM yet (`F5-01`: scripted client only). The first use case is a WPF application (.NET 4.8), whose controls are exposed through Microsoft UI Automation.

## Decision

- **UI inspection through LightVM Tools** (ADR-0031): new session capability `ui.inspect` and MCP tool `vm.ui.tree` returning the accessibility tree of the foreground window (or of a window chosen by title/process): control type, name, automation id, value, enabled/focused state, bounding rectangle in framebuffer coordinates; bounded depth and size, text sanitized like other guest output. Windows: UI Automation (WPF, WinForms, Win32); Linux: AT-SPI where available. Optional `ui.act` capability and `vm.ui.invoke` (invoke, set value, select) on an element of the last tree, checked by the supervisor like input (input ownership, rate limits, audit). Screen coordinates from the tree let text-only models use the existing mouse tools too.
- **Real clients**: the F5 exit criterion (two real clients) is executed with Claude Code and with a multimodal open-weight model through an MCP-capable client (for example a Qwen-VL model behind an OpenAI-compatible endpoint), versions recorded; a text-only model is verified with `vm.ui.tree` instead of screenshots.
- Guest data in the tree is untrusted (section 7.4 "Policy" row): it never changes authorizations.

## Required evidence

- WPF test application in a Windows guest: tree returned, element invoked, result visible in the screenshot; capability refused when not granted. Real clients run end to end. **Not verified.**

## References

Roadmap, sections 9, 10 (F5), 12 (R4a, R6); ADR-0021, ADR-0031, ADR-0034.
