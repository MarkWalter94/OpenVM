//! `help`: reference documentation of the LightVM MCP adapter.
//!
//! Describes how LightVM works (sessions, every tool, keys, errors,
//! limits) so a client has the whole picture in one place. It is a
//! reference, not instructions: it states what each tool does and how the
//! system behaves, and never tells the agent what to do, so it cannot
//! compete with the task the user gives in the prompt. Static text plus
//! the current session state; no session needed, no supervisor call. Tests
//! keep the tool sections, the key allowlist and the error codes in step
//! with the code.

use crate::keys;

/// Topics accepted by `help` besides the tool names.
pub const TOPICS: [&str; 6] = ["overview", "session", "tools", "keys", "errors", "limits"];

const OVERVIEW: &str = "\
# LightVM MCP adapter: overview

LightVM runs virtual machines (VMs) on the user's computer. This adapter exposes **one** of them \
to an MCP client inside a **session** approved by the user in the LightVM app. Access is the \
same a person has sitting at the VM's screen: screenshots of the display, mouse and keyboard \
input, and power operations (start, stop, reset). There is no shell, file or network channel \
between the client and the VM.

## Components
- **Session**: created by `session_request` and approved (or denied) by the user in the LightVM \
app. It is bound to one VM, a set of capabilities, quotas and a duration chosen by the user.
- **Capabilities**: `vm.list`, `vm.status`, `vm.start`, `vm.stop`, `vm.reset`, `vm.screenshot`, \
`vm.input`, `vm.snapshot`, `vm.restore`, `vm.logs`. Each VM tool requires one of them; a call \
without it is refused.
- **Input control**: keyboard and mouse belong either to the user or to the session, never \
both. The user hands control over and takes it back in the app.
- **Operations**: start, stop, snapshot and restore are operations with an `operation_id`; \
`operation_get` reports their state.
- **Baselines**: saved states of a stopped VM's disk and UEFI variables. A session may restore \
the baseline the VM had when the session started, or checkpoints it created itself.
- **Report**: LightVM records every call of the session in a report kept by the app. The outcome \
declared by the client (`session_declare`) is recorded as a claim; the final outcome comes from \
a verifier or from the user.

## Tool groups
- Session: `session_request`, `session_status`, `session_declare`, `session_end`.
- VM state and power: `vm_list`, `vm_status`, `vm_start`, `vm_stop`, `vm_reset`.
- Screen and input: `vm_screenshot`, `vm_mouse_move`, `vm_mouse_click`, `vm_keyboard_type`, \
`vm_keyboard_press`.
- Baselines: `vm_baselines`, `vm_snapshot`, `vm_restore`.
- Diagnostics: `vm_logs`, `operation_get`, `report_get`.
- Reference: `help`.

## Topics of `help`
`overview` (this page), `session` (life cycle of a session), `tools` (every tool), a tool name \
(one tool), `keys` (key names and typeable characters), `errors` (error codes), `limits` (what \
the adapter and the VM lack).
";

const SESSION: &str = "\
# Session life cycle

States reported by `session_status` (`state`):

| State | Meaning |
| --- | --- |
| `none` | No session has been requested by this adapter. |
| `pending` | A request is waiting for the user's decision in the LightVM app. It expires after 5 minutes. |
| `active` | Approved. The result lists the VM (`vm_ids`), the granted `capabilities` and `expires_at`. |
| `closed` | Denied, expired, revoked, ended or lost; `reason` says which. A new session requires a new request and a new approval. |

## Request and approval
- `session_request` takes a `purpose`, shown to the user as the session objective.
- The result carries a `verification_code` (e.g. `482-913`). The LightVM app shows the same code \
next to the request, and the user compares the two before approving: matching codes prove the \
request in the app is this one.
- A request needs a valid pairing: the user writes it in the app (Agents page, **Pair a \
client…**, with agent access switched on); it is valid for 10 minutes. Without it the request is \
refused with `unauthenticated`. The adapter reads the pairing at each request, so it does not \
need to be restarted after a new pairing.
- `session_status` with `wait_s` (up to 50 s) returns as soon as the user decides, or when \
`wait_s` elapses with the request still pending.
- One session at a time. A request while one is pending or active returns that one.

## While active
- The adapter renews the session's lease automatically every few seconds. If the adapter stops \
running, the lease lapses, the session ends and LightVM stops the VM.
- The user can **pause** the session (observation tools keep working, every other call is refused \
with `permission_denied`), **take back** keyboard and mouse, **revoke** the session, or press \
**Emergency Stop** (all VMs stop, all sessions end).
- Quotas (actions, input events, screenshots) and the duration are fixed at approval; when one \
runs out, calls are refused with `resource_limit` or the session closes.

## End
- `session_declare` records the outcome the client claims (`passed`, `failed`, `inconclusive`, \
`cancelled`, `timed_out`, `infrastructure_error`) with a summary, in the report.
- `session_end` ends the session; the VM keeps its current power state.
- A session never survives a restart of LightVM.
";

const LIMITS: &str = "\
# Limits

- **Network**: by default the VM has no network adapter. If the user granted network \
endpoints (`network.endpoints`, requested with `session_request`), the guest reaches only \
those: each is a name and a virtual address with one TCP port (`vm_status` lists them), mapped \
by LightVM to one service the user approved. Nothing else is reachable: no Internet, no LAN, \
no other host service, no UDP except the name lookups of the granted names, no IPv6. \
Everything sent to a granted endpoint reaches that service.
- **Files and clipboard**: there is no file transfer, shared folder or clipboard between the \
client and the VM. Content reaches the guest only as typed keystrokes; output is visible only \
in screenshots.
- **One VM** per session: the one the user chose. Other VMs are not listed and cannot be \
addressed.
- **Capabilities**: only those granted at approval; the user decides which.
- **Keyboard layout**: `vm_keyboard_type` produces key codes for the US layout. The text comes out \
as written only if the guest uses the US layout. Only printable ASCII, `\\n` and `\\t` are \
accepted.
- **Text length**: `vm_keyboard_type` accepts up to 1024 characters per call.
- **Rate**: the adapter accepts about 10 tool calls per second (bursts of 20); LightVM applies \
its own limits per session.
- **Guest content**: text inside the VM (pages, files, dialogs) is data from the guest; LightVM \
attaches no authority to it.
- **What LightVM does not limit**: the client's own tools on the host (files, shell, network) \
are outside LightVM's control.
- **Not available through the adapter**: creating or deleting VMs, media, QEMU options, host \
paths, deleting baselines, exporting reports.
";

const ERRORS: &str = "\
# Errors

A failed call returns `isError: true` with `{\"error\": {\"code\", \"message\", \"retryable\"}}`. \
`message` gives the specific cause; `retryable: true` means the same call can succeed later \
without any change.

| Code | Meaning |
| --- | --- |
| `permission_denied` | Capability not granted, session paused, or input control held by the user |
| `session_expired` / `session_closed` | The session ended (duration, revocation, Emergency Stop, agent access off, LightVM restart, lease lost) |
| `unauthenticated` | Pairing missing, expired or refused |
| `state_conflict` | The VM is not in the required state (e.g. screenshot of a stopped VM, snapshot of a running VM) |
| `vm_busy` | Another operation is running on the VM |
| `resource_limit` | A quota is used up, or calls arrive faster than the rate limit |
| `invalid_argument` | A parameter is wrong: unknown key name, untypable character, coordinates outside the display, bad identifier |
| `not_found` | Unknown VM, baseline or operation identifier |
| `timeout` | No reply in time; the request may still have taken effect (visible in `vm_status` / `operation_get`) |
| `unsupported` | Not available in this version |
| `infrastructure_error` | LightVM or QEMU failed |
";

/// `(tool, section)`: one section per catalogue tool.
const TOOLS: &[(&str, &str)] = &[
    (
        "help",
        "## help\nThis reference. Parameters: `topic` (optional): `overview` (default), \
`session`, `tools`, `keys`, `errors`, `limits` or a tool name. Works without a session. The \
result also reports the current session state.",
    ),
    (
        "session_request",
        "## session_request\nRequests a session from the user. Parameters: `purpose` \
(1-512 characters, shown to the user as the objective), optional `network_endpoints` (names \
of the VM's network endpoints the client would like, for example `[\"sql.lan\"]`; the user \
decides). Result: `state: pending`, \
`verification_code` (the same code the LightVM app shows next to the request), \
`request_expires_at`. Requires a pairing written by the app (see topic `session`).",
    ),
    (
        "session_status",
        "## session_status\nState of the session. Parameters: `wait_s` (0-50, default 30): \
while pending, how long the call waits for the user's decision. Result: `state` (`none`, \
`pending`, `active`, `closed`), `message`, and when active `session_id`, `vm_ids`, \
`capabilities`, `expires_at`; when closed, `reason`.",
    ),
    (
        "session_declare",
        "## session_declare\nRecords in the report the outcome the client claims. Parameters: \
`claimed_outcome` (`passed`, `failed`, `inconclusive`, `cancelled`, `timed_out`, \
`infrastructure_error`), `summary` (1-2000 characters). The report outcome itself is set by a \
verifier or by the user.",
    ),
    (
        "session_end",
        "## session_end\nEnds the session. No parameters. The VM keeps its power state. Result: \
number of queued operations discarded and of operations already forwarded (these may still \
take effect).",
    ),
    (
        "vm_list",
        "## vm_list\nThe VM of the session (`vm.list`). No parameters. Result: `vms` with \
`vm_id`, `name`, `state`, `run_id`, `display`, `input_owner`. The `vm_id` identifies the VM in \
every other VM tool.",
    ),
    (
        "vm_status",
        "## vm_status\nState of one VM (`vm.status`). Parameters: `vm_id`. Result: `state` \
(`stopped`, `starting`, `running`, `stopping`, `failed`, `recovering`), `run_id` of the current \
run, `display` (width and height in pixels), `input_owner` (true when the session holds \
keyboard and mouse), `network` (granted endpoints: `name`, `address`, `port` to use inside the \
guest; absent when the VM has no network for the session).",
    ),
    (
        "vm_start",
        "## vm_start\nStarts a stopped VM (`vm.start`). Parameters: `vm_id`. Returns when the VM \
process runs or the start failed. The guest operating system then boots on its own; its \
progress is visible in screenshots (typically 20-60 s for a desktop).",
    ),
    (
        "vm_stop",
        "## vm_stop\nStops the VM (`vm.stop`). Parameters: `vm_id`, `force` (default false). \
Without `force`: an ACPI shutdown request, as with a power button; the guest shuts down on its \
own and `vm_status` shows `stopping` then `stopped`. A guest may ignore it. With `force: true`: \
the VM process is terminated at once; unsaved guest data is lost.",
    ),
    (
        "vm_reset",
        "## vm_reset\nHardware reset of a running VM (`vm.reset`). Parameters: `vm_id`. Unsaved \
guest data is lost; the guest reboots.",
    ),
    (
        "vm_screenshot",
        "## vm_screenshot\nImage of the VM display (`vm.screenshot`; VM running). Parameters: \
`vm_id`, `scale` (`full` default, `half` = half width and height, fewer tokens). Result: a PNG \
image and `display` (width and height of the VM display), `image_width`, `image_height`, \
`run_id`, `captured_at`. Mouse coordinates are in display pixels at any scale: on a `half` image, one \
image pixel is two display pixels. The display updates asynchronously, so an image taken right \
after an action may not show its effect yet.",
    ),
    (
        "vm_mouse_move",
        "## vm_mouse_move\nMoves the pointer (`vm.input`, input control). Parameters: `vm_id`, \
`x`, `y` in display pixels, (0,0) top left. Coordinates outside the display are refused.",
    ),
    (
        "vm_mouse_click",
        "## vm_mouse_click\nClicks (`vm.input`, input control). Parameters: `vm_id`; `x` and `y` \
(optional, display pixels: the pointer moves there before clicking; without them the click \
happens at the current pointer position); `button` (`left` default, `middle`, `right`); \
`double` (true = double click).",
    ),
    (
        "vm_keyboard_type",
        "## vm_keyboard_type\nTypes text as key presses (`vm.input`, input control). Parameters: \
`vm_id`, `text` (1-1024 characters: printable ASCII, `\\n` = Enter, `\\t` = Tab), `layout` (only \
`us`). A text containing any other character is refused as a whole, before anything is typed. \
Keys go to whatever has the focus in the guest. Programs that auto-indent or auto-close \
brackets alter typed text. LightVM stores only the text's length and SHA-256, never the text.",
    ),
    (
        "vm_keyboard_press",
        "## vm_keyboard_press\nPresses a key or a chord (`vm.input`, input control). Parameters: \
`vm_id`, `keys` (one key such as `ret`, `esc`, `f5`, `up`, or up to 8 keys joined by `+` such as \
`ctrl+c`, `ctrl+alt+t`, `alt+f4`), `repeat` (1-20, default 1). The keys of a chord are pressed in \
order and released in reverse order. Key names: topic `keys`.",
    ),
    (
        "vm_snapshot",
        "## vm_snapshot\nSaves the stopped VM's disk and UEFI variables as a new baseline \
(`vm.snapshot`). Parameters: `vm_id`, `label` (1-200 characters). Refused while the VM is \
running. Never overwrites an existing baseline. Long operation (minutes for large disks).",
    ),
    (
        "vm_restore",
        "## vm_restore\nReturns the stopped VM to a baseline (`vm.restore`). Parameters: `vm_id`, \
`baseline_id` (one with `restorable: true` in `vm_baselines`: the session's initial baseline or \
a checkpoint created by the session). The VM's current disk state is discarded. Refused while \
the VM is running. Long operation.",
    ),
    (
        "vm_baselines",
        "## vm_baselines\nBaselines of the VM (`vm.status`). Parameters: `vm_id`. Result: each \
baseline with its label, creation time and `restorable` (true for the session's initial \
baseline and its own checkpoints).",
    ),
    (
        "vm_logs",
        "## vm_logs\nLightVM events of the session (calls, VM state changes, screenshots) and QEMU \
diagnostics of the current run, without host paths (`vm.logs`). Parameters: `vm_id`, \
`after_seq` (only entries after this sequence number, `last_seq` of an earlier result), `limit` \
(1-200, default 50). The guest's own logs are not included.",
    ),
    (
        "operation_get",
        "## operation_get\nState of an operation of the session (`vm.status`). Parameters: \
`operation_id` from an earlier result. Result: `kind`, `status`, `run_id` and, on failure, `error_code`.",
    ),
    (
        "report_get",
        "## report_get\nSummary of the session report: events, screenshots, current outcome and \
its source (`pending`, `termination`, `verifier`, `user`). Parameters: `session_id` (optional; \
only the current session is visible).",
    ),
];

/// Section of one tool, if it exists.
#[must_use]
pub fn tool_section(name: &str) -> Option<&'static str> {
    TOOLS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Names of the tools that have a section.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().map(|(n, _)| *n)
}

fn keys_text() -> String {
    let mut s = String::from(
        "# Keys\n\n## vm_keyboard_press\nOne key, or up to 8 keys joined by `+` (`ctrl+alt+t`); \
names are case-insensitive.\n\nKey names: letters `a`-`z`, digits `0`-`9`, ",
    );
    let names: Vec<String> = keys::ALLOWED.iter().map(|k| format!("`{k}`")).collect();
    s.push_str(&names.join(", "));
    s.push_str(
        ".\n\nAliases: `enter`/`return` = `ret`, `space` = `spc`, `escape` = `esc`, `control` = \
`ctrl`, `super`/`win`/`windows`/`meta` = `meta_l` (Windows/Super key), `pageup`/`pagedown` = \
`pgup`/`pgdn`, `del` = `delete`, `ins` = `insert`, `arrowup`/`arrowdown`/`arrowleft`/`arrowright` \
= `up`/`down`/`left`/`right`.\n\n## vm_keyboard_type\nPrintable ASCII (letters, digits, space \
and `` ` ~ ! @ # $ % ^ & * ( ) - _ = + [ ] { } \\ | ; : ' \" , . < > / ? ``), `\\n` (Enter) and \
`\\t` (Tab), mapped to the US layout. Other characters (accented letters, emoji, other \
alphabets) are refused.\n",
    );
    s
}

fn tools_text() -> String {
    let mut s = String::from(
        "# Tools\n\nVM tools identify the VM by the `vm_id` from `vm_list` and work only in an \
active session; the capability each one requires is in brackets. `vm.input` tools also require \
input control. Session tools and `help` require no capability.\n\n",
    );
    for (_, section) in TOOLS {
        s.push_str(section);
        s.push_str("\n\n");
    }
    s
}

/// Reference text of a topic: `Ok(text)`, or `Err` for an unknown topic.
pub fn guide(topic: &str) -> Result<String, String> {
    let t = topic.trim().to_ascii_lowercase();
    Ok(match t.as_str() {
        "" | "overview" | "start" | "help" | "info" => OVERVIEW.to_owned(),
        "session" => SESSION.to_owned(),
        "tools" => tools_text(),
        "keys" => keys_text(),
        "errors" => ERRORS.to_owned(),
        "limits" => LIMITS.to_owned(),
        other => match tool_section(other) {
            Some(s) => s.to_owned(),
            None => {
                return Err(format!(
                    "unknown topic; topics: {}, or a tool name",
                    TOPICS.join(", ")
                ));
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::catalogue;

    #[test]
    fn every_tool_has_a_section_and_every_section_a_tool() {
        let names: Vec<String> = catalogue()
            .iter()
            .map(|d| d.tool.name.to_string())
            .collect();
        for n in &names {
            assert!(tool_section(n).is_some(), "no help section for {n}");
            assert!(tools_text().contains(&format!("## {n}\n")), "{n}");
            assert!(
                OVERVIEW.contains(&format!("`{n}`")),
                "{n} not in the overview"
            );
        }
        for n in tool_names() {
            assert!(
                names.iter().any(|x| x == n),
                "help section for unknown tool {n}"
            );
        }
    }

    #[test]
    fn topics_render_and_unknown_topics_are_refused() {
        for t in TOPICS
            .iter()
            .copied()
            .chain(["", "  START ", "vm_keyboard_press"])
        {
            let g = guide(t).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert!(g.starts_with('#'), "{t}");
        }
        assert!(guide("nope").unwrap_err().contains("unknown topic"));
    }

    /// Reference, not instructions: no imperative wording aimed at the
    /// agent that could compete with the user's prompt.
    #[test]
    fn the_reference_gives_no_instructions() {
        let mut all = [OVERVIEW, SESSION, LIMITS, ERRORS].join("\n");
        all.push_str(&tools_text());
        all.push_str(&keys_text());
        let lower = all.to_ascii_lowercase();
        for bad in [
            "you must",
            "you should",
            "always ",
            "never assume",
            "ask the user",
            "tell the user",
            "call it ",
            "first call",
            "start here",
            "do not ",
            "don't",
            "make sure",
            "instead of",
            "next step",
        ] {
            assert!(!lower.contains(bad), "imperative wording: {bad:?}");
        }
    }

    #[test]
    fn keys_topic_lists_every_allowed_key() {
        let k = keys_text();
        for name in keys::ALLOWED {
            assert!(k.contains(&format!("`{name}`")), "{name}");
            assert!(keys::qcode_of(name).is_some(), "{name}");
        }
        for alias in [
            "enter", "space", "escape", "control", "super", "pageup", "del",
        ] {
            assert!(keys::qcode_of(alias).is_some(), "{alias}");
        }
    }

    #[test]
    fn errors_topic_covers_every_error_code() {
        for c in lightvm_proto::ErrorCode::ALL {
            assert!(
                ERRORS.contains(&format!("`{}`", c.as_str())),
                "{}",
                c.as_str()
            );
        }
    }
}
