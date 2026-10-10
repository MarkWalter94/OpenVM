# F1-01 — Shared types, VM model, state machine, resources, sessions and minimal audit

- Execution date (UTC): 2026-10-09
- Executed by: development agent (proto/core stream)
- Roadmap step: section 10, F1 (backlog `RELEASE_PLAN.md`: F1-01 for types and schemas, F1-04 for model, states, resources and media registry as a model, F1-10 for the minimal audit)
- Related ADRs: ADR-0005, ADR-0006, ADR-0007, ADR-0008, ADR-0017, ADR-0026 (Proposed)
- Overall outcome: partial (all items in this file are verified **with synthetic tests**; no item concerns QEMU, real files or hardware)

## Purpose

Demonstrate with automated tests, on fake in-memory ports, that:

1. `lightvm-proto` exposes shared types with JSON Schemas generated from a single source, compared with the committed files by a test that fails if the schemas are not regenerated, and a stable table of public error codes (section 6.3);
2. `vm-core` applies the state machine, the per-state guards, the per-VM queue, resource validation per user and per host, verification of approved media, agent sessions with grants, allowlists, quotas, expiries and revocation, and records the audit of creation, start, stop and denials (sections 6.3, 7.2 points 7 and 9, 7.3, 7.4, 10 F1);
3. the state derives from the backend's observations (`BackendObservation`) and not from the persisted state alone nor from late backend outcomes;
4. the distinction between `unauthenticated` and `permission_denied` is defined, implemented and tested (proposed in ADR-0026).

This test does **not** demonstrate: real boot, process management, QMP, protected file opening, managed read-only copy of media, hashes computed on real files, SQLite persistence, IPC. These are synthetic tests with simulated backend, catalog, clock, audit and media inspector: they do not replace the real boot required by the F1 exit criterion (sections 10 F1 and 16 point 5).

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | development host; not relevant for tests without I/O |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator and state | not used |
| Toolchain / relevant versions | rustc 1.99.0 (b940084d7 2026-09-28), host `x86_64-pc-windows-msvc`; cargo 1.99.0; Build Tools 2022 loaded by the `msvc.cmd` wrapper; `schemars` =1.2.2, `proptest` 1.11.0, `serde_json` 1.0.151 (from `Cargo.lock`) |
| QEMU binary (version, origin, SHA-256) | not used |
| Firmware (file, version, SHA-256) | not used |
| Guest / media (name, version, SHA-256) | none: simulated media with fictitious hashes |
| Graphics session (local, RDP, headless) | not relevant |
| Other conditions (antivirus, group policies, etc.) | workspace with changes in progress by other streams on `qemu-backend` and `lightvm-service` (see Limits) |

## Procedure

All commands are run from `E:\OpenVM` through the wrapper that loads the MSVC environment.

1. Initial test count, before the changes:

   ```text
   cargo test -p lightvm-proto -p vm-core
   ```

2. Changes:
   - `lightvm-proto`: new code `ErrorCode::Unauthenticated` appended at the end (additive), shortcut `PublicError::unauthenticated`, documentation of the semantics of the three authentication/authorization codes in the `error` module; `ProtocolErrorCode::Unauthenticated` documented as kept but not emitted.
   - `vm-core`: sessions with codes `unauthenticated` / `session_expired` / `permission_denied` (expiry evaluated before pause); queue discarded on termination with the code consistent with the cause (`discard_code_for`); user queue discarded by the Emergency Stop with `state_conflict`; check at start of the total guest memory against host memory minus the reserve.
   - Tests: `lightvm-proto/tests/error_codes.rs` (table frozen at 10 codes, consistency with the committed schema, semantics of the authentication codes); `vm-core/tests/negatives.rs` and `vm-core/tests/observations.rs` (new); update of `vm-core/tests/scenarios.rs` to the new codes; fakes extended in `vm-core/tests/common/mod.rs` (log of persisted transitions, audit that can fail, writable/moved/unreadable media).

3. Check of the strictness of the snapshot test, before regenerating:

   ```text
   cargo test -p lightvm-proto --test schema_snapshot
   ```

4. Regeneration of the schemas from the single source:

   ```text
   LIGHTVM_UPDATE_SCHEMAS=1 cargo test -p lightvm-proto --test schema_snapshot
   ```

5. Check that the property test detects a disallowed transition: temporary disabling of the `can_transition_to` guard in `Core::set_state` (backup copy of the file in the scratchpad), run of `cargo test -p vm-core --test observations`, restore of the file and check that the temporary change is no longer present.

6. Final commands:

   ```text
   cargo fmt -p lightvm-proto -p vm-core -- --check
   cargo clippy -p lightvm-proto -p vm-core --all-targets -- -D warnings
   cargo test -p lightvm-proto -p vm-core
   cargo check --workspace --all-targets
   cargo check --workspace --all-targets --exclude qemu-backend
   cargo check -p vm-storage -p lightvm-proto -p vm-core -p rfb-client -p lightvm-desktop --all-targets
   ```

## Results

### Test count

| Crate / file | Before | After |
| --- | --- | --- |
| `lightvm-proto` `tests/error_codes.rs` | 4 | 6 |
| `lightvm-proto` `tests/roundtrip.rs` | 18 | 18 |
| `lightvm-proto` `tests/schema_snapshot.rs` | 3 | 3 |
| `lightvm-proto` unit tests in `src/` | 0 | 4 (in `src/ipc.rs`, added by another stream, not by this one) |
| `vm-core` `tests/scenarios.rs` | 28 | 28 (7 assertions updated to the new codes) |
| `vm-core` `tests/negatives.rs` | — | 20 |
| `vm-core` `tests/observations.rs` | — | 8 (of which 1 property test, 256 cases) |
| **Total `lightvm-proto`** | 25 | 31 |
| **Total `vm-core`** | 28 | 56 |

### Items

| Item | Outcome | Evidence |
| --- | --- | --- |
| The snapshot test fails if the schemas are not regenerated | verified (synthetic) | step 3: `committed_schemas_match_generated ... FAILED` with the list `error_code.json`, `public_error.json`, `protocol_error.json`, `reply_error.json`, `operation_record.json`, `audit_event.json`, `caller.json: diverso dal generato`; the test also reports missing files and files no longer generated |
| Regeneration from a single source with `LIGHTVM_UPDATE_SCHEMAS=1` | verified (synthetic) | step 4: `3 passed`; `git diff --stat` on the schemas: 7 files changed, 33 lines added and 8 removed, only the types that contain `ErrorCode` or the changed documentation |
| Stable error code table | verified (synthetic) | `error_codes_are_frozen`: 10 codes in frozen order with serialized name and default `retryable`; `committed_error_code_schema_lists_every_code`: the committed schema lists exactly the 10 names |
| Distinction `unauthenticated` / `session_expired` / `permission_denied` | verified (synthetic) | `authentication_codes_are_distinct_and_not_retryable`; `unauthenticated_and_permission_denied_are_distinct` (same response for a wrong credential on an authorized and an unauthorized VM; valid credential outside the allowlist or without capability → `permission_denied`); `every_non_expiry_termination_cause_yields_unauthenticated`; `expired_session_is_session_expired_before_and_after_tick`; `missing_wrong_expired_revoked_or_restarted_token_is_denied_without_effect` |
| Disallowed state | verified (synthetic) | `failed_vm_rejects_every_mutation_until_reconciled`, `starting_vm_accepts_only_force_stop`, `incompatible_operations_are_rejected`, `guard_table_matches_architecture` |
| Busy VM | verified (synthetic) | `busy_vm_rejects_concurrent_lifecycle_mutations` (`vm_busy` retryable, a single force-stop), `only_one_operation_in_flight_per_vm`, `hard_queue_limit_applies_to_the_user_too` |
| Quotas exceeded | verified (synthetic) | `quotas_are_enforced`, `quota_denials_are_audited_and_global_quotas_bound_the_grant` (the global policy also limits an unlimited grant), `screenshot_over_byte_quota_fails_without_updating_geometry` |
| Revoked or expired session | verified (synthetic) | `revoked_session_is_unauthenticated_everywhere_and_queue_is_discarded`, `expired_session_is_session_expired_before_and_after_tick`, `renewal_cannot_extend_beyond_grant_or_lease_limit`, `lease_expiry_revokes_and_requests_emergency_stop`, `revocation_between_receipt_and_forwarding_discards_without_effect` |
| Unapproved media | verified (synthetic) | `unapproved_media_never_reaches_the_backend` (writable copy, different OS identity, replaced content → `permission_denied`; inspection impossible → `infrastructure_error`; no `start` call to the backend), `media_removed_from_registry_discards_start`, `empty_media_cannot_be_registered`, `replaced_media_is_not_used`, `verify_media_checks_every_field` |
| Resources beyond the limit (user and host) | verified (synthetic) | `user_limits_and_host_limits_are_intersected`, `total_guest_memory_is_bounded_by_host_memory_minus_reserve` (16+16 GiB on 28 GiB available rejected, 16+8 accepted), `host_capacity_change_is_rechecked_at_start_and_before_forwarding`, `create_vm_validates_resources_and_media`, `running_vm_limit_is_enforced` |
| Reconciliation from `BackendObservation` | verified (synthetic) | `qmp_ready_promotes_starting_before_the_backend_result`, `start_failed_observation_wins_over_a_late_success`, `unexpected_exit_while_running_fails_and_discards_the_queue`, `exit_during_stop_is_a_clean_stop_even_if_backend_reports_late_or_fails`, `force_stop_failure_requires_explicit_reconciliation`, `stale_and_unknown_observations_are_ignored`, `load_catalog_never_trusts_a_persisted_process_state`, `stale_run_id_is_rejected_and_queued_ops_invalidated` |
| Property: only transitions allowed by the table | verified (synthetic) | `prop_any_sequence_respects_the_transition_table`: 256 random sequences of up to 59 steps of requests, outcomes including late or failed ones, observations including non-current ones, Emergency Stop and reconciliation; invariants: every persisted transition is allowed by `VmState::can_transition_to`, process ⇒ `run_id`, `stopped` ⇒ no `run_id`, at most one open run, every backend start has a recorded run, audit with consecutive `seq` |
| The property test detects a removed guard | verified (synthetic) | step 5: with the guard disabled `prop_any_sequence_respects_the_transition_table ... FAILED` (`transizione stopped → recovering non ammessa`) and `force_stop_failure_requires_explicit_reconciliation ... FAILED`; after the restore the temporary change is not present (count 0) |
| Minimal audit of creation, start, stop and denials | verified (synthetic) | `minimal_audit_covers_create_start_stop_and_denials` (creation succeeded and denied; start and stop `accepted`, `forwarded`, `succeeded`; denials `permission_denied`, `unauthenticated`, `state_conflict` with VM, operation and capability), `start_stop_cycles_use_fresh_run_ids_and_audit_in_order`, `audit_is_ordered_and_contains_no_secrets`, `agent_session_limit_and_rejected_sessions_are_audited` |
| Audit unavailable before the effect | verified (synthetic) | `audit_failure_before_effect_blocks_the_operation`: error `infrastructure_error`, no operation queued, no call to the backend |
| Formatting | verified | `cargo fmt -p lightvm-proto -p vm-core -- --check`: no differences, exit code 0 |
| Lint | verified | `cargo clippy -p lightvm-proto -p vm-core --all-targets -- -D warnings`: `Finished`, no warnings |
| Tests of the two crates | verified | `cargo test -p lightvm-proto -p vm-core`: `lightvm-proto` 4 + 6 + 18 + 3, `vm-core` 20 + 8 + 28, all `ok`, 0 failed |
| Dependents not broken | partial | `cargo check --workspace --all-targets` fails in `qemu-backend` (`file not found for module compat`, `unresolved import crate::input`, …) and, with `qemu-backend` excluded, in `lightvm-service` (`file not found for module backend`, `cannot find module or crate vm_core`, `unresolved import vm_storage`): module and dependency errors from work in progress by other streams, none referring to `ErrorCode` or to `vm-core` APIs. `cargo check -p vm-storage -p lightvm-proto -p vm-core -p rfb-client -p lightvm-desktop --all-targets`: `Finished`. In `lightvm-service` there are no exhaustive `match`es on `ErrorCode` nor uses of `ErrorCode::ALL` (search in the source). Final rerun of `cargo check --workspace --all-targets`, after the other streams progressed: `qemu-backend` and the `lightvm-service` library (which now uses `vm_core::Core` and the ports) compile; a single error remains in the test `lightvm-service/tests/ipc.rs` (`unresolved import lightvm_service::compat`, `lightvm_service::supervisor::fake`), due to the reorganization of the service modules and not to this stream |

## Documented decision: `unauthenticated` and `permission_denied`

Proposed in ADR-0026, implemented in `lightvm-proto` (documentation of the `error` module) and in `vm-core` (`session.rs`):

| Code | When | Examples |
| --- | --- | --- |
| `unauthenticated` | no valid credential | IPC credential or session token missing, malformed, unknown, revoked, reused after a supervisor restart; session terminated for a cause other than expiry; request before the handshake |
| `session_expired` | credential recognized but expired | lease not renewed, grant past its expiry; takes precedence over pause |
| `permission_denied` | caller identified but not authorized | VM outside the allowlist (even nonexistent), capability missing from the effective policy, mutation while paused, insufficient IPC role, VM creation by an agent, media different from the approved one |

Within `unauthenticated` the reasons are not distinguished towards the client (same message `UNAUTHENTICATED_MESSAGE`). The protocol code `unauthenticated` stays in the contract but is not emitted. None of the three codes is automatically retryable.

## Limits

- **All results are synthetic.** Backend, catalog, clock, identifier generator, audit and media inspector are fake and in memory. No QEMU process, no files, no SQLite, no IPC.
- The media registry is verified only as a **model**: comparison between observed facts and the registry. Managed read-only copy, protected opening against reparse points (`CREATE_NEW`) and hash computation on the opened file belong to `vm-storage`/`qemu-backend` (F1-04, ADR-0008) and are not tested here.
- The overall storage budget (overlays, logs, screenshots, reserve) is not accounted in the core (ADR-0017); the disk limit applies per single VM against free space minus the reserve.
- Per-VM resource limits (`effective_resource_limits` with VM level) exist as a pure function but the core does not receive them yet: only user and host limits apply.
- vCPUs are not summed across running VMs (overallocation allowed); with `max_running_vms = 1` (MVP) the total memory check does not trigger.
- A guest power-off from inside while the VM is `running` arrives as an unexpected exit and leads to `failed`, unless the backend classifies it as expected (see Proposed decisions).
- Denied read-only `Core::operation` queries do not produce an audit event; denied `vm_status` and `list_vms` are recorded with action `vm_state_observed`.
- `cargo check --workspace --all-targets` is not green because of work in progress by other streams: at the last run only the test `lightvm-service/tests/ipc.rs` fails because of imports of removed modules. The `lightvm-service` library compiles with the `vm-core` APIs modified by this stream.

## Required actions

1. `lightvm-service` (stream F1-04/F1-11): return `unauthenticated` instead of `permission_denied` for a missing, malformed, unknown or revoked credential and for requests before `hello`; keep `permission_denied` for an insufficient role; update `tests/ipc.rs` and `F1-04-service-ipc-ci.md`. Rerun `cargo check --workspace --all-targets` when `qemu-backend` and `lightvm-service` compile.
2. `docs/ARCHITECTURE.md`: add `unauthenticated` to the list of public errors. `docs/adr/README.md`: add ADR-0026 to the index.
3. `vm-storage`/`qemu-backend` (F1-04): implement `MediaInspector` on the opened handle with the protections of ADR-0008, so that the verification also covers the use.
4. Wire up the per-VM limits (`effective_resource_limits` with VM level) when the catalog persists them.
5. Real tests of the F1 exit criterion (F1-12): still to be run; this file does not replace them.

## Proposed decisions

- ADR-0026 (Proposed): codes `unauthenticated` / `session_expired` / `permission_denied` as above.
- Power-off from the guest: propose that the backend marks as `expected` an exit preceded by a QMP `SHUTDOWN` event originating from the guest, so that the VM returns to `stopped` and not `failed`; to be decided with the QMP stream (F1-06).
- User queue discarded by an Emergency Stop: code `state_conflict` (the VM is stopping), instead of `permission_denied`.
