//! Controlled DNS replies through Windows' actual resolver, without changing
//! the machine's DNS/hosts settings. It qualifies the resolver/policy boundary.
use super::*;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::windows::process::CommandExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct DnsFixture {
    stop: Arc<AtomicBool>,
    private: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DnsFixture {
    fn start() -> Self {
        let socket =
            UdpSocket::bind("127.0.0.54:53").expect("dedicated loopback DNS port must be free");
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let private = Arc::new(AtomicBool::new(false));
        let (ending, phase) = (stop.clone(), private.clone());
        let thread = std::thread::spawn(move || {
            let mut query = [0; 512];
            while !ending.load(Ordering::SeqCst) {
                let Ok((length, peer)) = socket.recv_from(&mut query) else {
                    continue;
                };
                if length < 17 {
                    continue;
                }
                let mut end = 12;
                while end < length && query[end] != 0 {
                    let size = usize::from(query[end]);
                    if size > 63 {
                        end = length;
                        break;
                    }
                    end += size + 1;
                }
                if end + 5 > length || query[end + 1..end + 5] != [0, 1, 0, 1] {
                    continue;
                }
                let mut response = query[..end + 5].to_vec();
                response[2..12].copy_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
                response.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4]);
                response.extend_from_slice(if phase.load(Ordering::SeqCst) {
                    &[127, 0, 0, 54]
                } else {
                    &[1, 1, 1, 1]
                });
                socket.send_to(&response, peer).unwrap();
            }
        });
        Self {
            stop,
            private,
            thread: Some(thread),
        }
    }
}

impl Drop for DnsFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn windows_query(name: &str) -> Vec<SocketAddr> {
    // The test constructs this name exclusively from an integer PID.
    let script = format!(
        "$ErrorActionPreference='Stop'; Resolve-DnsName -Name '{name}' -Server 127.0.0.54 -Type A -DnsOnly -NoHostsFile -QuickTimeout | Where-Object Type -eq A | ForEach-Object {{$_.IPAddress}}"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Windows resolver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| SocketAddr::from((line.trim().parse::<Ipv4Addr>().unwrap(), 443)))
        .collect()
}

#[test]
#[ignore = "Windows DNS client and free private loopback UDP port 53 required"]
fn real_windows_dns_rebinding_answer_is_rejected_by_public_policy() {
    let fixture = DnsFixture::start();
    let name = format!("rebind-{}.lightvm.test", std::process::id());
    let initial = windows_query(&name);
    assert_eq!(initial, vec!["1.1.1.1:443".parse().unwrap()]);
    let pinned = public_address(&initial).unwrap();
    fixture.private.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(1200));
    let rebound = windows_query(&name);
    assert_eq!(rebound, vec!["127.0.0.54:443".parse().unwrap()]);
    let denied = public_address(&rebound).unwrap_err();
    println!(
        "name={name}\ninitial={initial:?}\npinned={pinned}\nrebound={rebound:?}\nrefused={denied}"
    );
}
