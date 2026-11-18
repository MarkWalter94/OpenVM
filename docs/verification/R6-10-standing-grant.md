# R6-10 — Standing grant: unattended agent access to one VM (ADR-0039)

- Execution date (UTC): 2026-11-18
- Executed by: development agent on the project host
- Roadmap step: section 12, R6 (owner request); declared exception to sections 4 and 7.3 (ADR-0039)
- Related ADRs: ADR-0039 (decision and implementation), ADR-0006 (amended), ADR-0033, ADR-0034, ADR-0026
- Branch: `feat/standing-grant` (from `main` at `1e8ebe2`)
- Status: see the summary table (§4)

The file name is R6-10 because R6-09 is already taken by the Tools idle-reconnect work in another worktree (`R6-09-tools-idle-reconnect.md`).

## Purpose

Record what was built for ADR-0039 and what was executed to verify it: persistence of the grant and of the hash of its credential, automatic approval and denial without queueing, end of the grant's sessions without stopping the VM (D1), revocation, Emergency Stop, deletion of the VM, suspension on a configuration change, baselines across the sessions of a grant, the MCP adapter's renewal and the app. What this file does NOT demonstrate is listed in §5.

## Environment

| Item | Value |
| --- | --- |
| Host | Windows 11 Pro 10.0.26200, WHPX |
| QEMU | 11.1.0 (`lab/qemu/qemu-11.1.0`, ADR-0010) |
| Guest (hardware cycle) | Ubuntu 26.04.1 desktop live ISO (`lab/media/ubuntu-26.04.1-desktop-amd64.iso`), booted only (nothing installed) |
| Build | separate `CARGO_TARGET_DIR` (`E:\OpenVM-tests\standing-grant-target`), MSVC Build Tools 2022 environment |

## 1. Tests in simulation (no QEMU)

| # (spec §5) | Item | Test | Outcome |
| --- | --- | --- | --- |
| — | Catalog: one live grant, terminal states stay terminal, bad hash refused; baselines of a grant; deleting the VM revokes its grant in the same transaction | `vm-storage` `standing::tests` (3) | verified in simulation/test |
| — | Credential known by hash only: verifies, expires, revoked with the non-UI credentials | `lightvm-service` `credential::tests::hashed_credentials_verify_expire_and_revoke` | verified in simulation/test |
| 2, 11 | Spec validation (7-day bound, normalisation, endpoints need `network.endpoints`, lease bounds) and conformity (intersection, VM, endpoint, client name, suspended, expired) | `lightvm-service` `standing::tests` (4) | verified in simulation/test |
| 4 | Core: client loss, lease expiry, grant expiry keep the VM running for a grant's session; UI close and host resume stop it; the agent access switch leaves it | `vm-core` `tests/scenarios.rs::standing_grant_sessions_keep_the_vm_running_on_expiry_and_client_loss` | verified in simulation/test |
| 1 | Grant survives a restart (credential still works, request approved without the switch and without the UI); the catalog file does not contain the credential; an expired grant is not live after a restart and its credential is refused; expiry audited | `lightvm-service` `tests/standing_grant.rs::grant_survives_a_restart_and_an_expired_one_does_not` | verified in simulation/test |
| 2 | Other VM, endpoint outside the grant, capabilities outside the grant denied with a reason and nothing queued; extra capabilities reduced to the grant's; `standing_grant_id` on the session; a second request while a session is live denied; counters; audit actor `standing_grant` and no `user` session creation | `…::conforming_requests_are_approved_others_denied_and_never_queued` | verified in simulation/test |
| 3 | With a grant live, an ordinary pairing credential still needs the switch and the user's decision | `…::ordinary_pairing_gets_no_automatic_approval` | verified in simulation/test |
| 4 | Client loss, lease expiry, grant expiry on a grant's session: VM running, no Emergency Stop; the grant expires and its credential is refused; the same client loss on a session approved by the user stops the VM | `…::client_loss_lease_and_grant_expiry_leave_the_vm_running` | verified in simulation/test |
| 5 | Revocation: live session ended (`revoked`), both connections opened with the credential closed, credential refused, VM running, event | `…::revocation_ends_the_session_and_the_credential_on_open_connections` | verified in simulation/test |
| 6 | Emergency Stop: grant revoked (also after a restart), VMs stopped, session ended | `…::emergency_stop_revokes_the_grant_and_stops_the_vms` | verified in simulation/test |
| 7 | Role matrix: the four new requests refused to a pairing connection; the standing credential cannot create, read, revoke or confirm grants nor use UI requests | `tests/f4.rs::request_role_matrix`, `…::duration_over_seven_days_is_refused_and_agents_cannot_grant_themselves` | verified in simulation/test |
| 8 | Second grant refused while one is live; unknown VM refused; deleting the VM revokes the grant (event, also after a restart) | `…::one_live_grant_and_vm_deletion_revokes_it` | verified in simulation/test |
| 9 | A configuration change suspends the grant (also after a restart); requests denied; Confirm makes it active with the new revision; audit | `…::configuration_change_suspends_until_confirmed` | verified in simulation/test |
| 10 | Baselines created by the grant's first session restorable by a later session of the same grant after a restart; not by a session approved by the user | `…::baselines_of_a_grant_are_restorable_by_its_later_sessions_only` | verified in simulation/test |
| 11 | Duration over 7 days and empty capabilities refused; exactly 7 days accepted | `…::duration_over_seven_days_is_refused_and_agents_cannot_grant_themselves` | verified in simulation/test |
| — | The agent access switch does not end the grant's session | `…::agent_access_switch_off_leaves_the_standing_session` | verified in simulation/test |
| 11 | Schemas regenerated | `lightvm-proto` `tests/schema_snapshot.rs` | verified in simulation/test |

## 2. MCP adapter and app

| Item | Test | Outcome |
| --- | --- | --- |
| A session obtained without any approval in the app (`standing: true`, nothing queued, switch off, capabilities = grant ∩ requested); help topic `standing` | `lightvm-mcp` `tests/standing.rs::standing_session_without_ui_approval` | verified in simulation/test |
| A grant with a 1 s lease kept alive (renewal at a third of the granted lease); the session ended (revoked by the user) is followed by a new one obtained by itself; VM running | `…::ended_session_is_renewed_automatically` | verified in simulation/test |
| Service shut down: `renewing`, VM tools refused with `session_renewing` (retryable), bounded attempts with backoff; service restarted on the same catalog with a new endpoint in the pairing file: new session, exactly one live; again after a second restart | `…::reconnects_after_a_service_restart` | verified in simulation/test |
| Revocation of the grant: renewal stops (`closed`), no new session, VM running | `…::revocation_stops_the_renewal` | verified in simulation/test |
| Explicit `session_end`: no renewal; a new `session_request` works | `…::explicit_session_end_does_not_renew` | verified in simulation/test |
| Creation form: every capability offered, destructive ones unticked, 1/3/7 days, summary and confirmation before the request; cancel | `lightvm-desktop` `tests/standing_ui.rs::creation_form_offers_every_capability_and_creates_only_after_the_summary`, `…::creation_can_be_cancelled_and_goes_back_to_the_choices` | verified in simulation/test |
| Ordinary approval form offers `vm.snapshot`, `vm.restore`, `vm.logs` | `…::ordinary_approval_form_offers_snapshot_restore_and_logs` | verified in simulation/test |
| Notice at start acknowledged by the user, not shown before the grant is read | `…::notice_at_start_must_be_acknowledged`, `…::no_notice_before_the_grant_is_read` | verified in simulation/test |
| Indicator on every page with Revoke and Emergency Stop; Agents page with the live grant; suspended state with Confirm; missing pairing file reported; Settings → Security | `…::indicator_on_every_page_with_revoke_and_emergency_stop`, `…::agents_page_shows_the_live_grant`, `…::suspended_grant_explains_why_and_is_confirmed_again`, `…::missing_pairing_file_makes_the_grant_unusable`, `…::settings_security_states_the_unattended_access` | verified in simulation/test |
| Pairing file of a live grant kept at stop and switch-off, rewritten with the new endpoint keeping the credential, removed at revocation, expiry and Emergency Stop | `lightvm-desktop` `src/service.rs` `standing_grant_pairing_file_follows_the_new_endpoint`, `standing_grant_file_at_start_without_grant_or_without_file`, `standing_grant_created_and_revoked_over_ipc`, `service_stop_keeps_the_pairing_file_of_a_live_grant` (fake supervisor over the real IPC) | verified in simulation/test |

## 3. Hardware cycle (real QEMU)

Test `crates/lightvm-service/tests/standing_grant_real.rs` (`--ignored`), real QEMU 11.1.0 with WHPX, Ubuntu live ISO booted (not installed), grant with a 1 s lease. Logs: scratch directory of the session (`r6-10-real*.log`), not kept in the repository.

| Step | Outcome |
| --- | --- |
| VM started by the user; grant created | verified on hardware |
| Session 1 approved without a click (`standing: true`), real screenshot (7503 B); ended by **lease loss**: VM `running`, **same run** | verified on hardware (two runs) |
| Session 2 approved without a click, real screenshot (16717 B); ended by **client loss**: VM `running`, same run | verified on hardware (two runs) |
| Restart of LightVM (service and supervisor; the VMs stop in an orderly way, QMP `quit`, ~15 s): grant still live after the restart, switch off | verified on hardware (two runs) |
| Session 3 after the restart (no click): VM started again by the user, real screenshot (33292 B), explicit `session_end`, VM `running` | verified on hardware (third run, host idle, 376 s in total) |
| Session 4 (no click), then revocation of the grant: session `revoked` 0.88 s after the request, credential refused at the handshake, VM `running` on the same run | verified on hardware (third run) |
| Earlier runs: after the restart WHPX did not initialise (`accelerator not available: WHPX did not initialise`; once at the very first start `WHPX: Failed to setup partition, hr=800705aa`, insufficient system resources) while the host was loaded by parallel builds and the owner's work | host condition, not a failure of the grant; the third run on an idle host passed |
| An earlier run failed at session 3 because the test client could not renew a 1 s lease while waiting for the deferred reply of a start: the service closed the session as designed; the test now lets the user start the VM | test defect, fixed |

Complete cycle verified on hardware: three renewals of the session without a click (lease loss, client loss, explicit end), a LightVM restart in the middle, the VM running between the sessions of the grant, revocation.

## 4. Summary

| Area | Outcome |
| --- | --- |
| Catalog, core, service, role matrix, schemas (§5 items 1–11) | verified in simulation/test |
| MCP adapter renewal and reconnection | verified in simulation/test |
| App (form, indicator, notice, Security, pairing file) | verified in simulation/test |
| Hardware: three renewals with the VM running, grant across a LightVM restart, revocation with the VM running | verified on hardware |

## 5. Not demonstrated

- A real MCP client (LLM) working for days under a grant: only the in-process adapter tests (§2).
- Revoking **one session** of a grant from the session bar does not stop the agent: the adapter sees the same `closing` as for an expiry and asks for a new session at once. To stop the agent the user revokes the grant (stated in `USER_GUIDE.md` §12 step 10). A distinct closing reason would let the adapter tell them apart; not implemented.
- The VM stopping when LightVM closes is unchanged (UI close keeps stopping); "the VM stays running" holds between the sessions of a grant, not across a restart of LightVM.
