# ADR-0023 — Dependencies and licences

- Status: Accepted for the licence policy, sources and CI checks (evidence in `docs/verification/F1-04-service-ipc-ci.md` and `docs/verification/F1-07-licences-packaging.md`); SBOM format and tool and review of the egui fonts open until F6
- Date: 2026-10-09
- Owner: to be assigned
- To be closed by: F1 (policy and checks: closed); final verification, SBOM and third-party inventory in F6

## Context

LightVM's original code is distributed under the MIT licence; the licences of third-party components must be inventoried and the compatibility of the distribution verified in F0/F6 (roadmap, section 6.0). Pinned versions and lockfile (section 6.3). F6 requires an SBOM, a licence inventory and vulnerability management. A vulnerable version is not frozen in order to comply with the text (section 17).

## Decision

1. **Allowed licences** for the dependencies linked into the binary: closed list in `deny.toml` — `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `BSL-1.0`, `ISC`, `Unicode-3.0`, `Unlicense`, `Zlib`, `0BSD`. Everything else is denied (GPL, LGPL, AGPL, MPL, EPL, CDDL, SSPL, unknown licences). An `OR` expression passes if it contains an allowed branch, which is the one used (for example `self_cell`: `Apache-2.0 OR GPL-2.0-only`).
2. **Named exceptions**: only `epaint_default_fonts` (`OFL-1.1`, `Ubuntu-font-1.0`, fonts embedded by egui), with human review required before F6.
3. **Sources**: crates.io only; no git dependencies, no alternative registries.
4. **Banned crates and features** (`[bans]`): HTTP clients and stacks (`reqwest`, `hyper`), `oauth2`, system TLS (`openssl-sys`, `native-tls`), `aws-lc-sys`; HTTP/OAuth/client features of the MCP SDK (ADR-0021). Each entry carries its rationale.
5. **CI tools**: `cargo-deny` 0.20.2 (licences, RustSec advisories without exceptions, sources, bans, duplicates as warnings) in the `cargo-deny` job; weekly Dependabot with groups.
6. **Reproducible inventory** from the lockfile with `cargo tree -e normal --target all --format "{p}|{l}"` (procedure in `F1-07`); in F6 a third-party inventory with licence texts and notices, and an SBOM.
7. **External components not redistributed** (QEMU, firmware, guest): documented in the guide with version, provenance and hash, not included in the MVP installer.
8. **New dependencies**: accepted only with an exact version, a licence in the list, no unnecessary network features, active maintenance and a justified need in the `Cargo.toml` comment; for VNC/RFB libraries ADR-0003/ADR-0024 apply.

Still to be decided (F6): SBOM tool and format (candidates `cargo-cyclonedx`, `cargo-about`; none tested); treatment of the egui fonts.

Criteria:

1. Every dependency has a known licence compatible with the MIT distribution of the binary. — met on the current lockfile (`F1-07` §2–§3): no mandatory copyleft licence, the only non-standard case being the egui fonts.
2. Known advisories checked on every change. — `cargo-deny` job defined; verified locally, CI not yet run (`F1-08`).
3. Inventory reproducible from the lockfile. — met (`F1-07`, procedure).

### Fonts embedded by the desktop UI (2026-10-12)

Besides the egui default fonts, `lightvm-desktop` embeds two fonts with `include_bytes!` (`apps/desktop/src/theme.rs`); both are under the same `OFL-1.1` already accepted for the egui fonts, ship unmodified, and carry their licence text next to them in `apps/desktop/assets/fonts/`. egui's default fonts stay behind them as fallback for glyph coverage.

| Font | Version | File | Source | SHA-256 of the file | Licence |
| --- | --- | --- | --- | --- | --- |
| Departure Mono (Helena Zhang) — headings and labels | 1.500 | `DepartureMono-Regular.otf` | <https://github.com/rektdeckard/departure-mono/releases/download/v1.500/DepartureMono-1.500.zip> (zip SHA-256 `bf3e48059aeef4617ec585bdea81dcc3491c576b3e7a472f52faf40e09ee5c3a`) | `4d53f663155cf8bf7ffc8e688776e719625f7bbb80a8d90073438b249261a2e0` | OFL-1.1 (`DepartureMono-OFL.txt`, identical to `public/assets/LICENSE` in the upstream repository; the repository's MIT `LICENSE` covers only the website) |
| JetBrains Mono (JetBrains) — body text | 2.304 | `JetBrainsMono-Regular.ttf` | <https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip> (zip SHA-256 `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`) | `a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f` | OFL-1.1 (`JetBrainsMono-OFL.txt`, identical to `OFL.txt` upstream) |

They are assets of a workspace crate, not Cargo packages: `cargo-deny` and `lightvm-sbom` do not see them, so this table is their inventory entry; the third-party notices of the F6 package must add the two licence files. OFL conditions kept: the fonts are not sold by themselves, are not modified or renamed, and each copy of the software contains the copyright notice and licence.

### Guest network stack (2026-10-15)

`crates/guest-net` (ADR-0033) adds `smoltcp` 0.14.0 (`0BSD`; default features off, only `std`, `medium-ethernet`, `proto-ipv4`, `socket-tcp`, `auto-icmp-echo-reply`; `proto-dhcpv4`/`socket-udp`/`socket-dhcpv4` only for the tests) with its dependencies `managed` 0.8.0 (`0BSD`), `heapless` 0.9.3 and `hash32` 0.3.1 (`MIT OR Apache-2.0`), `byteorder` 1.5.0 (`Unlicense OR MIT`), `stable_deref_trait` 1.2.1 (`MIT OR Apache-2.0`). Need: a user-mode TCP/IP stack that terminates the guest's TCP so that each guest connection to an approved endpoint gets its own host connection (QEMU `guestfwd` cannot, R6-04 §1). No network features beyond what the stack implements; it opens host connections only through `tokio::net::TcpStream` to the endpoints the user approved. `cargo deny check` result in `docs/verification/R6-06-guest-network.md`.

### Native file export and clipboard images (2026-10-16)

The desktop enables `arboard` 3.6.1's `image-data` feature for explicit image
copies, and directly uses `windows`/`windows-core` 0.62.2 (MIT OR Apache-2.0)
for Windows OLE copy drag-out from its private Outbox cache. These bindings
were already present transitively; no third-party drag implementation ships.
An initial `drag` 2.1.1 experiment was removed because its transitive graph
included the unmaintained `proc-macro-error` (RUSTSEC-2024-0370). The final
lockfile passes `cargo deny --locked check` without advisory exceptions;
duplicate-version warnings remain. The guest image codec itself uses Win32
and Python standard-library code; Linux PNG conversion requires optional
Pillow supplied by the guest system.

### Patched dependencies (2026-10-10)

`egui-winit` 0.36.2 (MIT OR Apache-2.0) is used as a patched copy in `vendor/egui-winit` (`[patch.crates-io]`, `vendor/README.md`) so that the host clipboard is never read (F1-13). Rules: minimal, marked patches, the same version pinned in ADR-0014, original licence unchanged, reapplication and re-verification on every upgrade; the licence inventory counts the copy as the original dependency.

## Alternatives considered

`cargo-deny` for licences and advisories (chosen), `cargo-audit` for advisories (covered by `cargo-deny`), `cargo-about` or `cargo-cyclonedx` for inventory and SBOM (to be tested in F6). The choice of tools does not replace human review of non-standard licences.

## Consequences

- A dependency with an incompatible or unmaintained licence is replaced or the code is brought in-house.
- Vulnerability management in `docs/SECURITY.md` relies on these checks.
- The Linux package declares the libraries loaded with `dlopen` listed in `F1-07` §6 (F1-14).
- The `f0-console` spike was a dependency of `apps/desktop` and had to leave the product graph before F6: **done on 2026-10-11** (`F6-01`, dated section). `tempfile` and `uds_windows` remain in the shipped graph through the product crate `rfb-client`.

## Required evidence

- First check with `cargo-deny`: `docs/verification/F1-04-service-ipc-ci.md`.
- Complete inventory (including `rfb-client` and the MCP SDK), updated policy and `dlopen` list: `docs/verification/F1-07-licences-packaging.md`.
- F6: third-party inventory and SBOM on the final artifact.

## References

Roadmap, sections 6.0, 6.3, 6.4, 10 (F0, F1, F6), 14, 17.
