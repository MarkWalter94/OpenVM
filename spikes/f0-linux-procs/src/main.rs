//! Spike F0 per ADR-0015: meccanismi Linux di terminazione dei figli del supervisor.
//!
//! Su sistemi diversi da Linux stampa "non applicabile" ed esce con codice 0, così
//! `cargo check --workspace` su Windows resta verde.
//!
//! Il workspace imposta `unsafe_code = "deny"`. Questo spike lo consente solo su Linux
//! perché i meccanismi da provare (`prctl(PR_SET_PDEATHSIG)` in `pre_exec`,
//! `pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll`, `kill(-pgid)`) non
//! hanno un'API sicura stabile nella libreria standard. Ogni blocco `unsafe` porta un
//! commento `SAFETY:`. Non è codice di prodotto.
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
        println!("f0-linux-procs: non applicabile su questo sistema operativo (spike solo Linux)");
    }
}
