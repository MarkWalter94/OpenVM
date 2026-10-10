//! Check P-5 (docs/STATUS.md): a second local user cannot reach the service
//! IPC endpoint. Uses the same `transport` module as the service (private
//! directory, permissions, peer identity check), without catalog or QEMU, so
//! it also builds on its own for Linux (static musl) and runs inside a VM.
//! Not a product artifact.
//!
//! ```text
//! lightvm-peer-check server --dir DIR     # prints the endpoint, then one line per connection
//! lightvm-peer-check client --endpoint E  # CONNECTED / ERROR / CLOSED BY SERVER
//! ```

use std::process::ExitCode;
use std::time::Duration;

use lightvm_service::transport::{self, Accepted, EndpointOptions, Listener};
use tokio::io::AsyncReadExt as _;

fn usage() -> ExitCode {
    eprintln!("usage: lightvm-peer-check server --dir DIR | client --endpoint E");
    ExitCode::from(2)
}

async fn server(dir: &str) -> ExitCode {
    let l = match Listener::bind(&EndpointOptions {
        runtime_dir: Some(dir.into()),
        catalog_dir: dir.into(),
    }) {
        Ok(l) => l,
        Err(e) => {
            println!("SERVER ERROR bind: {e}");
            return ExitCode::from(1);
        }
    };
    println!("SERVER ENDPOINT {}", l.endpoint());
    let _ = std::fs::write(std::path::Path::new(dir).join("endpoint.txt"), l.endpoint());
    let mut open = Vec::new();
    loop {
        match l.accept().await {
            Ok(Accepted::Ok(s, peer)) => {
                println!(
                    "SERVER ACCEPTED identity={} pid={:?}",
                    peer.os_identity, peer.pid
                );
                open.push(s);
            }
            Ok(Accepted::Rejected(reason)) => println!("SERVER REJECTED {reason}"),
            Err(e) => println!("SERVER ERROR accept: {e}"),
        }
    }
}

async fn client(endpoint: &str) -> ExitCode {
    match transport::connect(endpoint).await {
        Err(e) => {
            println!("CLIENT ERROR {e} (kind {:?})", e.kind());
            ExitCode::from(1)
        }
        Ok(mut s) => {
            // A rejected peer is closed immediately by the server: the read
            // returns 0 bytes. An accepted peer stays open.
            let mut b = [0u8; 1];
            match tokio::time::timeout(Duration::from_secs(2), s.read(&mut b)).await {
                Ok(Ok(0)) | Ok(Err(_)) => {
                    println!("CLIENT CLOSED BY SERVER");
                    ExitCode::from(1)
                }
                Ok(Ok(_)) | Err(_) => {
                    println!("CLIENT CONNECTED");
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
        _ => usage(),
    }
}
