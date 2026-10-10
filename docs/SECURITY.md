# Security

Last updated: 2026-10-09 (F0 review)

Operational document required by section 17 of `docs/ROADMAP.md`: implementation of the controls and updated residual risks. **It does not weaken the threat model to make tests pass.** The requirements are in section 7 of the roadmap; this document records their operational translation and their status. No control is implemented in the product: all of them are "planned". Where an F0 spike demonstrated feasibility on the first host (Windows 11), the status reads "planned, feasibility proven in F0-xx": it is a feasibility test, not a control implemented or verified in the product (review in `docs/verification/F0-09-evidence-review.md`).

## 1. Operational threat model

### 1.1 Assets to protect

| Asset | Concrete examples in LightVM |
| --- | --- |
| Host files and credentials | User files, OS credential store, provider credentials of the agent client, signing keys |
| Other VMs | Disks, baselines, firmware and artefacts of VMs other than the authorized one |
| Policies and grants | Global and per-VM policy, session grants, pairing credentials |
| Control endpoints | Supervisor IPC, QMP channel, display channel |
| Resource availability | Host CPU, memory and storage; reserve space for manifests and outcomes |
| Report integrity | JSON reports, audit, artefact hashes, screenshots |
| Baseline | Immutable disk and firmware state of the baseline |

### 1.2 Attackers and inputs considered hostile (section 7.1)

| Hostile source | Surface reached | Main controls |
| --- | --- | --- |
| Guest content | QEMU emulated devices, framebuffer, logs, displayed text | Offline profile, minimal devices, rendering as text, RFB client limited to a restricted subset |
| Test output | Reports, logs, screenshots | Rendering as text or limited Markdown, size limits, no link opened automatically |
| MCP client with insufficient permissions | MCP adapter, IPC | Limited grant, check on receipt and before forwarding, rate limit |
| Received parameters | Validation in the core, QEMU command generation | IDs instead of paths, controlled grammar of QEMU options |
| Imported images | Media registry, QEMU | Managed read-only copy with hash; no import of external QCOW2 in the MVP |
| Local process without credential | IPC, QMP, display channel | Private OS channels, ACLs/permissions, per-run credentials |
| Text in the guest instructing the agent | External agent | No LLM reasoning changes the policy; policy in the core |

### 1.3 Declared out of scope (section 7.1)

- Sandbox for deliberately hostile malware.
- Resistance to a compromised host administrator.
- Absolute separation from malicious processes of the same user.
- Filesystem confinement of a compromised QEMU process: the absence of shared folders limits the intended paths, it does not prove confinement.
- Revocation of shells, filesystems or tools that the agent client already has outside LightVM.
- Tamper-resistant forensic evidence: the local audit can be modified by the same user (section 9).
- Protection against the volume filling up because of other host processes (section 7.2).

### 1.4 Additional confinement

Status at the F0 review: Windows tested with the F0 spikes, Linux deferred to R2a (section 7.2; ADR-0017, ADR-0016). For each host the following are recorded separately: restrictive guest configuration, API control, host process sandbox. A missing mandatory control blocks AI Mode; a missing additional control is a declared limit, not an equivalent guarantee.

| Level | Windows | Linux |
| --- | --- | --- |
| Restrictive guest configuration | planned, feasibility proven in F0-02 (`-nodefaults -nic none`, virtio HID devices) | planned |
| API control (supervisor, policy) | planned | planned |
| QEMU host process sandbox | not available in F0: QEMU runs with the user's identity, without elevation, reduced environment and limited handles (F0-06); restricted token or AppContainer to be tested in F1 (ADR-0025). Declared limit | not tested (R2a) |

## 2. Controls of the MVP AI profile (section 7.2)

| # | Control | Status | Implementation phase | Planned evidence |
| --- | --- | --- | --- | --- |
| 1 | Approved hardware acceleration and QEMU configuration; no silent fallback to TCG | planned, feasibility proven in F0-02 (real WHPX probe, no TCG in any F0 test) | F0 probe; F1 diagnostics and refusal | F1 criterion "clear error when the accelerator is missing" (ADR-0007) |
| 2 | No guest NIC, default network peripherals excluded, no forwarding | planned, feasibility proven in F0-02 §5 (`-nodefaults -nic none`; checked with `info network`, not with `query-rx-filter`) | F1 (default offline profile) | F1 criterion "no unexpected NIC"; Network negative test (ADR-0004) |
| 3 | No host sharing, clipboard, USB passthrough, physical disk or unnecessary device; only allowlisted virtual HIDs | planned, feasibility proven in F0-02 and F0-04 (only `virtio-tablet`/`virtio-keyboard`; RFB client without clipboard). Open risk: egui-winit reads the host clipboard on Ctrl+V (text discarded by the UI; backlog F1-13) | F1 | Comparison between generated configuration and devices observed via QMP; Console negative test |
| 4 | Processes without elevation, cleaned environment, supervision of the whole process tree | planned, feasibility proven in F0-06 (Job Object from creation, `KILL_ON_JOB_CLOSE`, environment with only `SystemRoot`, limited handles, supervisor death without orphans); UI crash not tested | F0 model test; F1 implementation | F1 criterion "20 cycles without orphans" (ADR-0002, ADR-0015) |
| 5 | Disks, ISOs and artefacts through internal registries; the client supplies IDs, not paths | planned; baseline exclusion through share mode proven in F0-05 (ADR-0025) | F1 media registry; F4–F5 sessions and MCP | Host and File negative tests (ADR-0008) |
| 6 | Private QMP, protected console, authenticated IPC, restrictive OS permissions | planned, feasibility proven in F0-02, F0-04 and F0-06 (Unix socket in a private directory with user-only ACL; refusal at low integrity); second user and per-run display credential not tested; IPC from F1 | F0 test; F1 QMP/IPC; F2 console | IPC/console negative test (ADR-0012, ADR-0013) |
| 7 | Sessions with expiry, VM allowlist, allowed actions, quotas, revocation, input ownership | planned | F4 | Sessions and VM negative tests; F4 exit criterion (ADR-0006) |
| 8 | CPU/memory limits on the process tree; storage budget and host reserve with the nature of the control declared | planned, feasibility proven in F0-08 (job CPU hard cap and memory limit with real effect on QEMU/WHPX; no hard storage quota without administrator: application-level budget) | F0 identification; F3 catalog quotas; F4 enforcement on sessions | Resources negative test; tests under pressure (ADR-0017) |
| 9 | Audit of actions and denials, minimisation of secrets and sensitive content | planned | F1 minimal; F3 storage; F4 complete | Review of the logs produced by the negative tests; absence of secrets in reports |
| 10 | Stop and revocation independent of the model and of the client's response | planned, feasibility proven in F0-06 for the Emergency Stop (`TerminateJobObject`, independent of QMP: p95 640.2 ms over 30 samples at desktop, 273.3 ms over 100 samples during boot); revocation from F4 | F0 termination test; F4 implementation | Revocation and Lifecycle negative tests; revocation and Emergency Stop measurements of section 13 (ADR-0009) |

Statuses used in this table: `planned`, `implemented`, `verified in simulation/test`, `verified on hardware`, `missing (blocks AI Mode)`. Every status change refers to a file in `docs/verification/`.

## 3. Mandatory negative tests (section 7.4)

Checklist to be completed with a reference to the verification file on the product. No item has been run on the product; the notes point to the relevant F0 feasibility tests. Passing these tests demonstrates the tested properties, not the absence of every possible VM escape.

- [ ] **Sessions** — token missing, expired, revoked or reused after restart: denial with no effect on the VM.
- [ ] **VM** — session on A requests a screenshot or input on B: denial, no data from B.
- [ ] **Host** — path traversal, UNC, device path, symlink/junction, QEMU options in parameters: refusal.
- [ ] **Network** — guest to host, LAN, Internet, IPv6 in the offline profile: no path.
- [ ] **IPC/console** — local process without credential towards IPC or display channel: connection refused. *F0-04 §6: low-integrity process of the same user refused on the QMP and VNC sockets; second user not tested (P-5).*
- [ ] **Policy** — text in the guest asks to disable the controls: authorizations unchanged.
- [ ] **Revocation** — input flood during Emergency Stop: no forwarding after revocation, requests already sent identified, process termination verified.
- [ ] **Storage** — concurrent restore at boot; deletion of a referenced base: operation prevented. *F0-05 §7: with the supervisor's handle in `FILE_SHARE_READ`, writing, renaming and deleting the base are refused.*
- [ ] **Crash** — process killed during creation, rollback or DB update: safe state, baseline intact. *F0-05 §8: base unchanged after killing QEMU while writing and `qemu-img convert`; the partial file passes `qemu-img check`.*
- [ ] **Resources** — oversized VM; screenshot/log flood: refusal or containment.
- [ ] **Report** — logs with HTML/script, terminal sequences, dangerous links: text only, no active markup, no automatic opening.
- [ ] **Lifecycle** — client lost, UI closed, supervisor terminated, host resumed from suspend: session revoked, no autonomous restart, processes handled according to policy. *F0-06 §2a: supervisor terminated → QEMU terminated by the operating system; UI closed and suspend not tested.*
- [ ] **Console** — clipboard, power control or input on the RFB channel outside the bridge: messages not implemented, display credential not available to other processes. *F0-04 §6: `rfb-client` sends only messages 0, 2, 3 (test `read_only.rs`, verified in simulation/test); per-run credential not yet implemented.*
- [ ] **File** — ISO replaced after approval; QEMU options in metadata: identity verified, no interpretation as host configuration.

## 4. Declared residual risks

| Residual risk | Reason | Available mitigation |
| --- | --- | --- |
| Vulnerabilities in QEMU, the emulated devices or the hypervisor (VM escape) | Third-party code exposed to the guest | Minimal devices, no network, QEMU updates, no sandbox promise |
| Vulnerabilities in Rust dependencies | Third-party code | Inventory, advisories in CI, update or disabling (ADR-0023) |
| Malicious process of the same user | Same OS identity | Private channels and per-run credentials reduce but do not eliminate the risk |
| Tools already held by the agent client | Outside LightVM's control | Document a client configuration consistent with the session |
| Effects of input already delivered to QEMU after revocation | Cannot be undone retroactively | Report distinguishing discarded queue, forwarded requests and observed effects |
| Storage budget exceeded before the stop (if there is no hard quota) | Periodic application-level check | Thresholds, reserve, maximum check frequency declared (ADR-0017) |
| Secrets visible in screenshots | Guest content | Explicit controls on capture, retention and deletion; no automatic recognition promised |
| Locally modifiable audit | Same user | Artefact hashes detect alterations relative to the manifest, not coordinated tampering |
| Overlay in use writable by another process of the same user | QEMU opens images with `FILE_SHARE_WRITE`; the share mode does not distinguish processes (`F0-05` §7.3) | Baseline protected by the supervisor's handle; overlay in a per-run directory with restricted ACL; separate identity for QEMU to be tested in F1 (ADR-0025) |
| QEMU image locking ineffective on Windows | Behaviour of build 11.1.0 (`F0-02` §8.3) | Exclusion guaranteed by the supervisor, never by `qemu-img` refusing (ADR-0025) |
| Display credential not distinct from the QMP one at ACL level | UI and supervisor run as the same user; the sockets have the same ACL | Only the supervisor knows and occupies the QMP socket (single client); per-run credential to be implemented by F2 (ADR-0013) |
| Expired Authenticode signature of the QEMU binaries | Certificate of the build's signer expired (`F0-02` §2.2) | Hashes of the approved binaries checked before use; winget checksum and manifest; additional verification before F6 (ADR-0010) |
| Remote protocol drivers included in the QEMU build (`http`, `ssh`, `nbd`, …) | Third-party build | Allowlisted protocols and structured representations (section 6.3; backlog F1-08) |
| Host clipboard read by egui-winit on Ctrl+V | Library behaviour (`F0-04` §4) | Text discarded by the UI; path blocked before F2 (backlog F1-13) |
| Memory limit exceeded at steady state: QEMU crash instead of a handled error | Observed behaviour (`F0-08`) | `resource_limit` classification from the job notification; overlay not promoted (ADR-0017) |

## 5. Vulnerability handling (minimal process)

1. **Reporting.** Private reporting channel to be defined when the public repository is created; until then, reports go to the security owner (to be assigned). Reports are not discussed in public issues before the fix.
2. **Assessment.** Classification by severity and by affected capability (QMP, IPC, console, MCP, paths, processes, storage).
3. **Dependencies.** Advisories checked on every change (ADR-0023); for QEMU and firmware, monitoring of security advisories for the version recorded in `docs/COMPATIBILITY.md`.
4. **Response** (section 17): update of the dependency, new verification, or disabling of the affected capability. A vulnerable version is not kept just to comply with the text of the roadmap.
5. **Recording.** Every intervention updates this document, the matrix and, if needed, a verification file.
6. **Release.** No known and unmitigated critical or high vulnerability within the scope at release (F6 exit criterion).

## 6. Rules on secrets

- Credentials in the OS credential store; never in process arguments, reports, logs or committed examples (section 9).
- No provider or MCP client credential passed to QEMU; environment of child processes cleaned (section 6.3).
- Runners with untrusted guests or images have no production credentials (section 13).
- The `.gitignore` file excludes keys and `.env` files; it does not replace review before committing.
