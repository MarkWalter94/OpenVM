//! `lightvm-ipc-probe`: client di prova per l'IPC del servizio.
//!
//! Uso: `lightvm-ipc-probe --endpoint <endpoint>`; la credenziale (64 caratteri
//! esadecimali) si legge dalla prima riga dello stdin, mai dagli argomenti. Una
//! riga vuota prova l'handshake senza credenziale.
//!
//! Stampa una riga `PROBE ...` e termina con: 0 connesso, handshake accettato e
//! `ping` riuscito; 10 connessione rifiutata dal sistema operativo; 11
//! handshake rifiutato; 12 altro errore.

use std::io::BufRead;
use std::process::ExitCode;

use lightvm_service::client::Client;
use lightvm_service::compat::{Request, ResponseBody, ServerMessage};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let endpoint = match (args.next().as_deref(), args.next()) {
        (Some("--endpoint"), Some(e)) => e,
        _ => {
            eprintln!("uso: lightvm-ipc-probe --endpoint <endpoint> (credenziale su stdin)");
            return ExitCode::from(12);
        }
    };
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    let credential = Some(line.trim().to_owned()).filter(|s| !s.is_empty());
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            println!("PROBE errore=runtime:{e}");
            return ExitCode::from(12);
        }
    };
    rt.block_on(async move {
        let mut c = match Client::connect(&endpoint).await {
            Ok(c) => c,
            Err(e) => {
                println!(
                    "PROBE connessione=rifiutata errore_os={:?} messaggio=\"{e}\"",
                    e.raw_os_error()
                );
                return ExitCode::from(10);
            }
        };
        match c.hello_with(vec![1], credential).await {
            Ok(ServerMessage::HelloOk {
                connection_id,
                role,
                ..
            }) => {
                match c.request(Request::Ping).await {
                    Ok(ServerMessage::Response {
                        result: ResponseBody::Pong,
                        ..
                    }) => {
                        println!(
                            "PROBE connessione=ok handshake=ok connection_id={connection_id} ruolo={role:?} ping=pong"
                        );
                        ExitCode::SUCCESS
                    }
                    other => {
                        println!("PROBE connessione=ok handshake=ok ping={other:?}");
                        ExitCode::from(12)
                    }
                }
            }
            Ok(other) => {
                println!("PROBE connessione=ok handshake=rifiutato risposta={other:?}");
                ExitCode::from(11)
            }
            Err(e) => {
                println!("PROBE connessione=ok handshake=errore:{e}");
                ExitCode::from(12)
            }
        }
    })
}
