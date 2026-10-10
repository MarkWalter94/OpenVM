//! `lightvm-ipc-probe`: test client for the service IPC.
//!
//! Usage: `lightvm-ipc-probe --endpoint <endpoint>`; the credential (64
//! hexadecimal characters) is read from the first line of stdin, never from
//! the arguments. An empty line tries the handshake without a credential.
//!
//! Prints a `PROBE ...` line and exits with: 0 connected, handshake accepted
//! and `ping` succeeded; 10 connection refused by the operating system; 11
//! handshake rejected; 12 other error.

use std::io::BufRead;
use std::process::ExitCode;

use lightvm_proto::ipc::{Request, ResponseBody, ServerMessage};
use lightvm_service::client::Client;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let endpoint = match (args.next().as_deref(), args.next()) {
        (Some("--endpoint"), Some(e)) => e,
        _ => {
            eprintln!("usage: lightvm-ipc-probe --endpoint <endpoint> (credential on stdin)");
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
            println!("PROBE error=runtime:{e}");
            return ExitCode::from(12);
        }
    };
    rt.block_on(async move {
        let mut c = match Client::connect(&endpoint).await {
            Ok(c) => c,
            Err(e) => {
                println!(
                    "PROBE connection=refused os_error={:?} message=\"{e}\"",
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
                            "PROBE connection=ok handshake=ok connection_id={connection_id} role={role:?} ping=pong"
                        );
                        ExitCode::SUCCESS
                    }
                    other => {
                        println!("PROBE connection=ok handshake=ok ping={other:?}");
                        ExitCode::from(12)
                    }
                }
            }
            Ok(other) => {
                println!("PROBE connection=ok handshake=rejected response={other:?}");
                ExitCode::from(11)
            }
            Err(e) => {
                println!("PROBE connection=ok handshake=error:{e}");
                ExitCode::from(12)
            }
        }
    })
}
