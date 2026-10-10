//! Prova P-5 (docs/STATUS.md): un secondo utente locale non raggiunge
//! l'endpoint IPC del servizio. Usa lo stesso modulo `transport` del servizio
//! (directory privata, permessi, verifica dell'identità del peer), senza
//! catalogo né QEMU, così si compila anche da solo per Linux (musl statico) e
//! si esegue dentro una VM. Non è un artefatto di prodotto.
//!
//! ```text
//! lightvm-peer-check server --dir DIR     # stampa l'endpoint, poi un rigo per connessione
//! lightvm-peer-check client --endpoint E  # CONNESSO / ERRORE / CHIUSO DAL SERVER
//! ```

use std::process::ExitCode;
use std::time::Duration;

use lightvm_service::transport::{self, Accepted, EndpointOptions, Listener};
use tokio::io::AsyncReadExt as _;

fn uso() -> ExitCode {
    eprintln!("uso: lightvm-peer-check server --dir DIR | client --endpoint E");
    ExitCode::from(2)
}

async fn server(dir: &str) -> ExitCode {
    let l = match Listener::bind(&EndpointOptions {
        runtime_dir: Some(dir.into()),
        catalog_dir: dir.into(),
    }) {
        Ok(l) => l,
        Err(e) => {
            println!("SERVER ERRORE bind: {e}");
            return ExitCode::from(1);
        }
    };
    println!("SERVER ENDPOINT {}", l.endpoint());
    let _ = std::fs::write(std::path::Path::new(dir).join("endpoint.txt"), l.endpoint());
    let mut aperte = Vec::new();
    loop {
        match l.accept().await {
            Ok(Accepted::Ok(s, peer)) => {
                println!(
                    "SERVER ACCETTATO identita={} pid={:?}",
                    peer.os_identity, peer.pid
                );
                aperte.push(s);
            }
            Ok(Accepted::Rejected(motivo)) => println!("SERVER RIFIUTATO {motivo}"),
            Err(e) => println!("SERVER ERRORE accept: {e}"),
        }
    }
}

async fn client(endpoint: &str) -> ExitCode {
    match transport::connect(endpoint).await {
        Err(e) => {
            println!("CLIENT ERRORE {e} (kind {:?})", e.kind());
            ExitCode::from(1)
        }
        Ok(mut s) => {
            // Un peer rifiutato viene chiuso subito dal server: la lettura
            // restituisce 0 byte. Un peer accettato resta aperto.
            let mut b = [0u8; 1];
            match tokio::time::timeout(Duration::from_secs(2), s.read(&mut b)).await {
                Ok(Ok(0)) | Ok(Err(_)) => {
                    println!("CLIENT CHIUSO DAL SERVER");
                    ExitCode::from(1)
                }
                Ok(Ok(_)) | Err(_) => {
                    println!("CLIENT CONNESSO");
                    ExitCode::SUCCESS
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["server", "--dir", d] => rt.block_on(server(d)),
        ["client", "--endpoint", e] => rt.block_on(client(e)),
        _ => uso(),
    }
}
