# F1-07 — Licence inventory, `cargo-deny` policy and Linux packaging dependencies

- Execution date (UTC): 2026-10-09
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 6.0 (licences of third-party components), 6.3 (pinned versions and lockfile), 10 F0 ("dependencies/licences" closable within F1) and F1; backlog F1-19 and F1-14 in `docs/RELEASE_PLAN.md`
- Related ADRs: ADR-0023 (updated), ADR-0021, ADR-0014, ADR-0024
- Overall outcome: **verified** for the inventory from the lockfile and for `cargo deny check` on the workspace; **not executed** for the SBOM and for the runtime verification of the listed Linux libraries

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/f1-07/` (outside the repository): `tree-normal-all.txt`, `tree-normal-build-all.txt`, `tree-prodotto.txt`, `tree-prodotto-x86_64-pc-windows-msvc.txt`, `tree-prodotto-x86_64-unknown-linux-gnu.txt`, `tree-rfb-client.txt`, `licenze-prodotto-conteggi.txt`, `cargo-deny-prima.txt`, `cargo-deny-dopo.txt`.

## Purpose

1. Reproducible inventory, from the lockfile, of the licences of all the workspace's dependencies, including `rfb-client` (ADR-0024) and the new MCP spike (`lightvm-mcp`, F1-06).
2. List of copyleft licences or licences incompatible with MIT distribution of the binary.
3. Policy in `deny.toml` that blocks disallowed licences and sources other than crates.io.
4. List of Linux libraries loaded with `dlopen` to declare in the packaging (F1-14).

It does not demonstrate: legal compliance (human review of the non-standard licences is needed, ADR-0023); SBOM; licences of external components that are not redistributed (QEMU, firmware, guest), which remain outside the MVP installer; actual presence of the Linux libraries at runtime on a physical Ubuntu host.

## Environment

| Item | Value |
| --- | --- |
| Host | F0 host of ADR-0018, Windows 11 Pro 10.0.26200 |
| Toolchain | Rust 1.99.0, cargo 1.99.0 |
| Tools | `cargo tree` (cargo 1.99.0); `cargo-deny` 0.20.2 already installed in the user profile (no installation in this test); `cargo-about` and `cargo-cyclonedx` not installed and not used |
| Lockfile | the workspace's `Cargo.lock` at the time of the test, including the dependencies of `lightvm-mcp` |
| Network | crates.io through cargo; **RustSec database downloaded from GitHub** by `cargo deny check` on the first run (the tool's default behaviour); the second run is `--frozen`, without network |

## Procedure

```text
# Full inventory (all members, spikes included), normal dependencies, all targets
cargo tree --workspace -e normal --target all --prefix none --format "{p}|{l}" | sort -u
# With build dependencies (tools used by build.rs, not linked into the binary)
cargo tree --workspace -e normal,build --target all --prefix none --format "{p}|{l}" | sort -u
# Product set (without the spikes as roots), per target
cargo tree -p lightvm-desktop -p lightvm-service -p qemu-backend -p vm-core -p vm-storage \
  -p lightvm-proto -p rfb-client -p lightvm-mcp -e normal --target <triple> --prefix none --format "{p}|{l}"
# rfb-client only
cargo tree -p rfb-client -e normal --target all --prefix none --format "{p}|{l}"
# Policy
cargo deny --locked check          # before changing deny.toml
cargo deny --frozen check          # after the change, without network
# Features of the Linux graph for the dlopen list
cargo tree -p lightvm-desktop --target x86_64-unknown-linux-gnu -e features --prefix none
```

## Results

### 1. Size of the inventory

| Set | Distinct packages (name + version) | Of which workspace crates |
| --- | --- | --- |
| Full workspace, normal dependencies, all targets | 426 | 12 |
| Product set, all targets | 423 | 9 |
| Product set, `x86_64-pc-windows-msvc` | 227 | 9 |
| Product set, `x86_64-unknown-linux-gnu` | 286 | 9 |
| `rfb-client`, all targets | 41 | 1 |
| Build dependencies only added (`-e normal,build` versus `-e normal`) | 17 | 0 |

The workspace crates all have the MIT licence. The spike `f0-console` appears in the product set, because `apps/desktop` depends on it (F0-04 real console): a spike should not end up in the published binary (see Actions).

### 2. Declared licences (product set, all targets)

Count per declared SPDX expression, workspace crates excluded (`licenze-prodotto-conteggi.txt`).

| Expression | Packages |
| --- | --- |
| `MIT OR Apache-2.0` / `Apache-2.0 OR MIT` / `MIT/Apache-2.0` / `Apache-2.0/MIT` / `MIT / Apache-2.0` | 198 + 41 + 17 + 2 + 1 |
| `MIT` | 87 |
| `Apache-2.0` | 12 |
| `Zlib OR Apache-2.0 OR MIT`, `MIT OR Apache-2.0 OR Zlib`, `MIT OR Zlib OR Apache-2.0` | 10 + 6 + 2 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 7 |
| `Unlicense OR MIT` | 5 |
| `ISC` | 3 |
| `Zlib`, `BSL-1.0`, `0BSD` | 2 each |
| `BSD-3-Clause OR MIT OR Apache-2.0`, `BSD-3-Clause OR Apache-2.0`, `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 each |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 |
| `BSD-3-Clause`, `Apache-2.0 OR ISC OR MIT`, `Apache-2.0 OR GPL-2.0-only`, `Apache-2.0 AND MIT`, `Apache-2.0 AND ISC`, `0BSD OR MIT OR Apache-2.0`, `0BSD OR Apache-2.0`, `(MIT OR Apache-2.0) AND Unicode-3.0`, `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | 1 each |

### 3. Copyleft, non-standard licences or licences with obligations to manage

| Crate | Licence | Target | Assessment |
| --- | --- | --- | --- |
| `self_cell` 1.3.0 | `Apache-2.0 OR GPL-2.0-only` | all | Apache-2.0 chosen; no copyleft obligation |
| `r-efi` 5.3.0, 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | UEFI only (not present in the Windows and Linux graphs) | MIT or Apache-2.0 chosen |
| `epaint_default_fonts` 0.36.2 | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | all (fonts embedded in the UI) | **non-standard**: fonts with their own licences, not copyleft for the software but with conditions (licence text to distribute; restrictions on the name of modified fonts). Allowed only as an exception for this crate; human review required (ADR-0023) |
| `ring` 0.17.14 | `Apache-2.0 AND ISC` | all (from `rustls`, `rfb-client`) | permissive; contains C and assembly code derived from BoringSSL compiled into the binary; notices to reproduce |
| `rmcp`, `rmcp-macros` 3.5.1; `winit` 0.30.13; `glutin` 0.32.3 and `glutin_*_sys`; `accesskit_winit` 0.32.2; `codespan-reporting` 0.13.1; `spirv` 0.4.0; `gethostname` 1.1.0 (Linux); `unicode-general-category` 1.1.0 | `Apache-2.0` | various | permissive, compatible with MIT distribution of the binary; requires the licence text and any `NOTICE` notices in the third-party inventory |
| `clipboard-win` 5.4.1, `error-code` 3.4.0 | `BSL-1.0` | Windows | permissive |
| `libsqlite3-sys` 0.38.2 (feature `bundled` of `rusqlite`) | `MIT` (the crate); SQLite is public domain | all | SQLite C source compiled into the binary; to be reported in the SBOM as a C component |
| `unicode-ident` | `(MIT OR Apache-2.0) AND Unicode-3.0` | procedural macros only (compile time) | not linked into the binary |

**Outcome: no mandatory copyleft licence and no licence incompatible with MIT.** The only copyleft options (GPL-2.0-only, LGPL-2.1-or-later) appear in `OR` expressions with permissive alternatives. The licences of the egui fonts are the only non-standard case.

### 4. `rfb-client` (ADR-0024)

41 packages (`tree-rfb-client.txt`): `flate2`/`miniz_oxide`/`adler2`/`crc32fast`/`simd-adler32` (ZRLE), `socket2`, `uds_windows` (Windows), `rustls` + `ring` + `rustls-webpki` + `untrusted` + `subtle` + `zeroize` + `rustls-pki-types` (VeNCrypt), `tempfile`, `getrandom`, the `windows-*` and `rustix`/`linux-raw-sys` families. Licences: all permissive (`ISC` for `rustls-webpki` and `untrusted`; `BSD-3-Clause` for `subtle`; `Apache-2.0 AND ISC` for `ring`; the others MIT, Apache-2.0 or dual). No problems.

### 5. `cargo deny check`

| Run | Outcome | Evidence |
| --- | --- | --- |
| Before the change (`--locked`, with RustSec database update) | verified | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0` (`cargo-deny-prima.txt`) |
| After changing `deny.toml` (`--frozen`) | verified | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0`; 22 non-blocking `duplicate` warnings (`bitflags`, `getrandom`, `windows-sys`, `syn`, `thiserror`, `accesskit_consumer`, `objc2`/`calloop`/`smithay-client-toolkit` families, etc.) (`cargo-deny-dopo.txt`) |
| Final repetition on the current lockfile (`--frozen`, 15:51 UTC, after other streams changed the lockfile) | verified | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0`, 22 `duplicate` warnings (`cargo-deny-finale.txt`) |
| Negative test of the new rules (banned crates or features introduced on purpose) | **not executed** | the rules are verified only on the current graph, which contains no banned crates |

Changes to `deny.toml`:

- comment making explicit the denial of everything not in `allow` (GPL, LGPL, AGPL, MPL, EPL, CDDL, SSPL, unknown licences) and the treatment of `OR` expressions;
- `[bans] deny`: `reqwest`, `hyper` (offline profile, ADR-0004), `oauth2` (MCP over stdio without HTTP OAuth), `openssl-sys`, `native-tls` (TLS only via rustls, ADR-0013), `aws-lc-sys` (CMake/NASM on the build host, F0-04), each with a rationale;
- `[bans] features`: HTTP, OAuth and client features of `rmcp` banned (ADR-0021);
- `[sources]`: substantially unchanged (crates.io only, no git, no unknown registry), empty `[sources.allow-org]` added.

### 6. Linux libraries loaded with `dlopen` (F1-14)

From the feature graph of `lightvm-desktop` for `x86_64-unknown-linux-gnu` (`winit` with `x11` and `wayland`, `xkbcommon-dl` with `x11`, `wayland-sys` with `dlopen` and `egl`, `x11-dl`, `x11rb` with `dl-libxcb`, `glutin` with `egl` and `glx`, `wgpu-hal` with `vulkan` and `gles`) and from the observation in F0-07 (X11 panic without `libxkbcommon-x11.so.0`). Package names are those of Ubuntu LTS. **List derived from the code and the features, not verified with `ldd`/tracing on a physical Ubuntu host.**

| Library (soname) | Ubuntu package | Loaded by | When needed |
| --- | --- | --- | --- |
| `libxkbcommon.so.0` | `libxkbcommon0` | `xkbcommon-dl` | always (X11 and Wayland keyboard) |
| `libxkbcommon-x11.so.0` | `libxkbcommon-x11-0` | `xkbcommon-dl` (feature `x11`) | X11 sessions; missing → panic (D-17) |
| `libwayland-client.so.0` | `libwayland-client0` | `wayland-sys` (`dlopen`) | Wayland sessions |
| `libwayland-cursor.so.0` | `libwayland-cursor0` | `wayland-sys` through `winit`/`sctk` | cursors in Wayland |
| `libwayland-egl.so.1` | `libwayland-egl1` | `wayland-sys` (feature `egl`) | glow/GLES on Wayland |
| `libX11.so.6`, `libX11-xcb.so.1`, `libXcursor.so.1`, `libXrandr.so.2`, `libXi.so.6` | `libx11-6`, `libx11-xcb1`, `libxcursor1`, `libxrandr2`, `libxi6` | `x11-dl` (winit, glutin) | X11 sessions |
| `libxcb.so.1` | `libxcb1` | `x11rb` (`dl-libxcb`) | X11 sessions |
| `libEGL.so.1` | `libegl1` (+ Mesa driver `libegl-mesa0`) | `glutin` (`egl`), `wgpu-hal` (`gles`) | glow/GLES backend on EGL |
| `libGL.so.1` / GLX | `libgl1` (+ `libgl1-mesa-dri`) | `glutin` (`glx`) | glow backend on X11 without EGL |
| `libvulkan.so.1` | `libvulkan1` (+ ICD, for example `mesa-vulkan-drivers`) | `wgpu-hal` (`vulkan`) | wgpu backend |

These need no system libraries: SQLite (bundled, `bundled`), TLS (`rustls` + `ring`, static), AccessKit on Linux (`zbus` in pure Rust; only a session bus is needed for accessibility at runtime). The same list is used in the CI workflow (`APT_RUNTIME`, `.github/workflows/ci.yml`) and will have to appear in the dependencies of the Linux package (section 5, Ubuntu and Debian/Fedora rows; R2a).

## Limits

- `cargo tree` reports the `license` field declared in the manifests: it does not read the licence files nor detect included code with a different licence (for example embedded C sources). `cargo-deny` also uses licence-file recognition with a 0.8 threshold.
- The `cargo tree` lists were extracted at 15:2x UTC on a lockfile that other streams were modifying; development dependencies (for example those of the egui_kittest snapshots) are not included in the lists, but they are in the final `cargo deny` check.
- The "all targets" set includes undeclared platforms (macOS, wasm, UEFI): a prudent overestimate.
- No SBOM produced; `cargo-about` and `cargo-cyclonedx` not installed (task rule).
- The first run of `cargo deny` downloaded the RustSec database from GitHub.
- The `dlopen` list is not verified at runtime on physical Ubuntu; the `smoke-render-software` smoke in CI (not yet run, `F1-08-ci.md`) is the first planned verification.
- No legal review.

## Required actions

1. F6: generate the third-party inventory with licence texts and notices (Apache-2.0 `NOTICE`, OFL/Ubuntu fonts, `ring`) and the SBOM (candidates: `cargo-about`, `cargo-cyclonedx`), with the C components of SQLite and `ring` declared.
2. Human review of the licences of the `epaint_default_fonts` fonts, or replacement with fonts under a known licence (ADR-0023).
3. Remove the dependency of `apps/desktop` on the spike `f0-console` before F6 (product code must not depend on `spikes/`).
4. F1-14 / R2a: verify the `dlopen` list with `ldd`, `LD_DEBUG=libs` or `strace -e openat` during the smoke on Ubuntu (CI and physical host) and declare it in the package.
5. Negative test of `deny.toml` (banned crates or features introduced on a test branch) at the first CI run.

## Proposed decisions

- **ADR-0023:** policy = closed list of permissive licences in `deny.toml` (MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2/3-Clause, BSL-1.0, ISC, Unicode-3.0, Unlicense, Zlib, 0BSD), named exception for the egui fonts, `OR` expressions resolved on the permissive branch; tools `cargo-deny` 0.20.2 in CI (licences, RustSec advisories without exceptions, crates.io sources only, banned crates and features); SBOM and third-party inventory in F6 with a tool to be tested; external components (QEMU, firmware, guest) documented and not redistributed.
- **F1-14:** list of `dlopen` libraries of section 6 as declared dependencies of the Linux package.

## Files produced

- `deny.toml` (updated)
- `docs/verification/F1-07-licences-packaging.md` (this file)
- `lab/logs/f1-07/*` (outside the repository)


## Update 2026-10-10 (late evening): getrandom 0.4.3 and sha2 0.11.0

Section appended (rule 8). Dependabot updates merged into `main`: `getrandom` 0.3.4 → 0.4.3 (`lightvm-service`), `sha2` 0.10.9 → 0.11.0 (`qemu-backend`, `vm-storage`, `spikes/f0-storage`). New or changed transitive crates: `digest` 0.11.3, `block-buffer` 0.12.1, `crypto-common` 0.2.2, `cpufeatures` 0.3.1, `hybrid-array` 0.4.15 (replaces `generic-array` 0.14.7), `const-oid` 0.10.2. All `MIT OR Apache-2.0`, allowed by `deny.toml`. No code change needed; fmt, clippy `-D warnings` (all four variants) and `cargo test --workspace --locked` green: 365 passed, 0 failed, 16 ignored.
