//! Spike F0 for ADR-0015: Linux mechanisms for terminating the supervisor's children.
//!
//! On systems other than Linux it prints "not applicable" and exits with code 0, so
//! `cargo check --workspace` on Windows stays green.
//!
//! The workspace sets `unsafe_code = "deny"`. This spike allows it only on Linux
//! because the mechanisms under test (`prctl(PR_SET_PDEATHSIG)` in `pre_exec`,
//! `pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll`, `kill(-pgid)`) do not
//! have a stable safe API in the standard library. Every `unsafe` block carries a
//! `SAFETY:` comment. Not product code.
#![cfg_attr(target_os = "linux", allow(unsafe_code))]

#[cfg(target_os = "linux")]
mod linux;

fn main() {
    #[cfg(target_os = "linux")]
    {
        std::process::exit(linux::main());
    }
    #[cfg(not(target_os = "linux"))]
    {
        println!("f0-linux-procs: not applicable on this operating system (Linux-only spike)");
    }
}
