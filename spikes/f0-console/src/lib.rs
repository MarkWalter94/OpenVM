//! Spike F0-04 (non è codice di prodotto): client QMP minimo per input e
//! `screendump`, conversione PPM→PNG, tabella di digitazione US e statistiche.
//!
//! Il client QMP accetta soltanto un percorso di socket Unix (canale privato,
//! ADR-0012) e non espone un `execute` generico verso la UI: offre metodi
//! tipizzati per input, cattura e stato. Il QMP di prodotto vive in
//! `crates/qemu-backend` a partire da F1.

#![forbid(unsafe_code)]

pub mod image;
pub mod qmp;
pub mod stats;
pub mod typing;
