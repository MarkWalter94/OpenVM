//! Supervisor reale avviato dalla UI come processo figlio (F1-11): riga
//! `ready`, due connessioni IPC autenticate, iscrizione agli eventi, elenco VM
//! e arresto controllato tramite la pipe di vita. Nessun QEMU: senza
//! `--qemu-dir` il servizio usa il backend "non configurato".
//!
//! L'eseguibile `lightvm-service` è cercato in `LIGHTVM_SERVICE_EXE` oppure
//! nella directory di build accanto ai test (presente dopo
//! `cargo build --workspace` o `cargo test --workspace`). Se manca, il test lo
//! dichiara su stderr e non esegue nulla.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_desktop::servizio::{Comando, ConfigServizio, ErroreAvvio, Servizio};
use lightvm_proto::ipc::ParentLossPolicy;

fn eseguibile_servizio() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIGHTVM_SERVICE_EXE") {
        return Some(PathBuf::from(p));
    }
    // target/<profilo>/deps/<test>.exe → target/<profilo>/lightvm-service.exe
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .parent()?
        .join(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX));
    p.exists().then_some(p)
}

fn catalogo_temporaneo(nome: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lightvm-desktop-{nome}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn attendi(s: &Servizio, cosa: &str, ok: impl Fn(&Servizio) -> bool) {
    let t0 = Instant::now();
    while !ok(s) {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{cosa}: stato {:?}",
            s.stato()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ui_avvia_il_supervisor_e_lo_arresta() {
    let Some(exe) = eseguibile_servizio() else {
        eprintln!("lightvm-service non compilato: prova saltata (cargo build --workspace)");
        return;
    };
    let dir = catalogo_temporaneo("figlio");
    let cfg = ConfigServizio {
        eseguibile: exe,
        catalogo: dir.join("catalogo"),
        qemu_dir: None,
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        politica: ParentLossPolicy::default(),
    };
    let mut s = Servizio::avvia(&cfg, Arc::new(|| {})).expect("avvio del supervisor");
    assert!(s.pid() > 0);
    attendi(&s, "connessione", |s| s.stato().connesso);
    attendi(&s, "capacità", |s| s.stato().backend.is_some());
    let st = s.stato();
    assert_eq!(st.connessione, "connesso");
    assert!(st.vms.is_empty(), "catalogo nuovo: {:?}", st.vms);
    assert!(s.vivo());

    // Emergency Stop senza VM: completato, nessun processo.
    assert!(s.invia(Comando::EmergencyStop));
    attendi(&s, "esito dell'Emergency Stop", |s| {
        s.stato().ultimo_esito.is_some()
    });
    let esito = s.stato().ultimo_esito.unwrap();
    assert!(esito.starts_with("Emergency Stop: 0 processi"), "{esito}");

    // Seconda istanza sullo stesso catalogo: rifiutata con il codice 3.
    match Servizio::avvia(&cfg, Arc::new(|| {})) {
        Err(ErroreAvvio::UscitoPrimaDelPronto(Some(3))) => {}
        other => panic!("seconda istanza: {other:?}"),
    }

    let (codice, forzato) = s.arresta(Duration::from_secs(15));
    assert_eq!(codice, Some(0), "uscita regolare");
    assert!(!forzato, "arresto controllato senza terminazione forzata");
    assert!(!s.vivo());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Impostazioni della VM e apertura da riga di comando attraverso un
/// supervisor reale senza QEMU: modifica salvata (revisione 2), `ApriVm`
/// applica le modifiche e prova l'avvio, rifiutato perché QEMU non è
/// configurato; VM inesistente segnalata.
#[test]
fn impostazioni_vm_e_apri_vm_senza_qemu() {
    use lightvm_desktop::impostazioni_vm::ModificheVm;
    use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmName};
    let Some(exe) = eseguibile_servizio() else {
        eprintln!("lightvm-service non compilato: prova saltata (cargo build --workspace)");
        return;
    };
    let dir = catalogo_temporaneo("impostazioni");
    let cfg = ConfigServizio {
        eseguibile: exe,
        catalogo: dir.join("catalogo"),
        qemu_dir: None,
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        politica: ParentLossPolicy::default(),
    };
    let mut s = Servizio::avvia(&cfg, Arc::new(|| {})).expect("avvio del supervisor");
    attendi(&s, "connessione", |s| s.stato().connesso);
    let vm = VmConfig {
        name: VmName::parse("da-riga-di-comando").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: Default::default(),
    };
    assert!(s.invia(Comando::Crea(Box::new(vm))));
    attendi(&s, "creazione", |s| s.stato().vms.len() == 1);
    let v = s.stato().vms[0].clone();
    let mut nuova = v.config.clone();
    nuova.limits.cpu_max_percent = 30;
    assert!(s.invia(Comando::AggiornaVm {
        vm_id: v.vm_id,
        revisione: v.config_revision,
        config: Box::new(nuova),
    }));
    attendi(&s, "revisione 2", |s| {
        s.stato()
            .vms
            .first()
            .is_some_and(|v| v.config_revision == 2)
    });
    assert_eq!(s.stato().vms[0].config.limits.cpu_max_percent, 30);

    // Riga di comando: modifiche applicate, avvio rifiutato senza QEMU.
    assert!(s.invia(Comando::ApriVm {
        nome: "da-riga-di-comando".into(),
        modifiche: ModificheVm {
            vcpus: Some(1),
            disk_gib: Some(20),
            storage_budget_gib: Some(Some(10)),
            ..ModificheVm::default()
        },
        crea: false,
    }));
    attendi(&s, "revisione 3", |s| {
        s.stato()
            .vms
            .first()
            .is_some_and(|v| v.config_revision == 3)
    });
    let c = s.stato().vms[0].config.clone();
    assert_eq!(
        (c.vcpus, c.disk_gib, c.limits.storage_budget_gib),
        (1, 20, Some(10))
    );
    attendi(&s, "esito dell'avvio", |s| {
        s.stato()
            .ultimo_esito
            .is_some_and(|e| e.starts_with("avvio") || e.contains("unsupported"))
    });
    eprintln!("esito: {:?}", s.stato().ultimo_esito);

    // VM inesistente.
    assert!(s.invia(Comando::ApriVm {
        nome: "non-esiste".into(),
        modifiche: ModificheVm::default(),
        crea: false,
    }));
    attendi(&s, "VM non trovata", |s| {
        s.stato()
            .ultimo_esito
            .is_some_and(|e| e.contains("non trovata"))
    });
    // `--crea`: VM creata con i valori predefiniti più le modifiche.
    assert!(s.invia(Comando::ApriVm {
        nome: "creata".into(),
        modifiche: ModificheVm {
            memory_mib: Some(1024),
            cpu_max_percent: Some(20),
            ..ModificheVm::default()
        },
        crea: true,
    }));
    attendi(&s, "VM creata", |s| s.stato().vms.len() == 2);
    let creata = s
        .stato()
        .vms
        .into_iter()
        .find(|v| v.name.as_str() == "creata")
        .unwrap();
    assert_eq!(creata.config.memory_mib, 1024);
    assert_eq!(creata.config.limits.cpu_max_percent, 20);
    let (codice, forzato) = s.arresta(Duration::from_secs(15));
    assert_eq!((codice, forzato), (Some(0), false));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eseguibile_assente_e_un_errore_di_avvio() {
    let dir = catalogo_temporaneo("assente");
    let cfg = ConfigServizio {
        eseguibile: dir.join("non-esiste.exe"),
        catalogo: dir.join("catalogo"),
        qemu_dir: None,
        runtime_dir: None,
        audit_log: None,
        politica: ParentLossPolicy::default(),
    };
    match Servizio::avvia(&cfg, Arc::new(|| {})) {
        Err(ErroreAvvio::Spawn(_)) => {}
        other => panic!("atteso errore di spawn: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Variabile che fa eseguire a [`crash_ui_con_vm_reale`] la parte della "UI".
const RUOLO_UI: &str = "LIGHTVM_P2_RUOLO_UI";

fn pid_vivo(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.contains(&format!("\"{pid}\""))
}

/// Figli diretti di un processo: (PID, nome eseguibile).
fn figli(pid: u32) -> Vec<(u32, String)> {
    let cmd = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}' | \
         ForEach-Object {{ \"$($_.ProcessId) $($_.Name)\" }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let (p, n) = l.trim().split_once(' ')?;
            Some((p.parse().ok()?, n.to_owned()))
        })
        .collect()
}

/// Parte "UI" della prova P-2: avvia il supervisor con QEMU, crea e avvia una
/// VM, stampa `PRONTO <pid servizio>` e resta in attesa di essere terminata.
fn ruolo_ui(qemu_dir: PathBuf) -> ! {
    use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmName, VmState};
    let exe = eseguibile_servizio().expect("lightvm-service compilato");
    let dir = PathBuf::from(std::env::var_os("LIGHTVM_P2_DIR").unwrap());
    let cfg = ConfigServizio {
        eseguibile: exe,
        catalogo: dir.join("catalogo"),
        qemu_dir: Some(qemu_dir),
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        politica: politica_p2(),
    };
    let s = Servizio::avvia(&cfg, Arc::new(|| {})).expect("avvio del supervisor");
    attendi(&s, "connessione", |s| s.stato().connesso);
    let vm = VmConfig {
        name: VmName::parse("p2-crash-ui").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 1,
        memory_mib: 1024,
        disk_gib: 8,
        network: NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: Default::default(),
    };
    assert!(s.invia(Comando::Crea(Box::new(vm))));
    attendi(&s, "creazione", |s| s.stato().vms.len() == 1);
    let vm_id = s.stato().vms[0].vm_id;
    assert!(s.invia(Comando::Avvia(vm_id)));
    attendi(&s, "VM in esecuzione", |s| {
        s.stato().vms.first().map(|v| v.state) == Some(VmState::Running)
    });
    println!("PRONTO {}", s.pid());
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    // `Servizio` non viene mai distrutto: la UI "muore" senza arresto controllato.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Politica della prova P-2 passata alla UI di prova (`kill` o `shutdown:N`).
const POLITICA_P2: &str = "LIGHTVM_P2_POLITICA";

fn politica_p2() -> ParentLossPolicy {
    match std::env::var(POLITICA_P2).ok().as_deref() {
        Some(v) if v.starts_with("shutdown:") => ParentLossPolicy::ShutdownVms {
            powerdown_s: v["shutdown:".len()..].parse().unwrap(),
        },
        _ => ParentLossPolicy::KillVms,
    }
}

/// P-2 (`docs/STATUS.md`): crash della UI che ha avviato il supervisor, con una
/// VM reale in esecuzione. La UI è terminata con `TerminateProcess`; il
/// supervisor deve rilevare la chiusura della pipe di vita, applicare la
/// politica scelta e uscire, senza processi QEMU orfani. Restituisce i tempi
/// di fine di QEMU e del servizio e le righe di audit rilevanti.
fn prova_crash_ui(nome_test: &str, politica: &str) -> Option<(Duration, Duration, String)> {
    let qemu_dir = std::env::var_os("LIGHTVM_QEMU_DIR").map(PathBuf::from)?;
    if std::env::var_os(RUOLO_UI).is_some() {
        ruolo_ui(qemu_dir);
    }
    assert!(
        eseguibile_servizio().is_some(),
        "lightvm-service non compilato (cargo build --workspace)"
    );
    let dir = catalogo_temporaneo("p2");
    let mut ui = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            nome_test,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(RUOLO_UI, "1")
        .env(POLITICA_P2, politica)
        .env("LIGHTVM_P2_DIR", &dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = ui.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for l in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            // libtest stampa `test <nome> ... ` senza a capo prima dell'output.
            if let Some((_, p)) = l.split_once("PRONTO ") {
                let _ = tx.send(p.trim().parse::<u32>().unwrap());
            }
        }
    });
    let svc_pid = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(p) => p,
        Err(e) => {
            // Nessun processo lasciato indietro se la UI di prova non è pronta.
            let _ = ui.kill();
            let _ = ui.wait();
            panic!("la UI di prova non ha avviato la VM: {e}");
        }
    };
    let qemu: Vec<u32> = figli(svc_pid)
        .into_iter()
        .filter(|(_, n)| n.eq_ignore_ascii_case("qemu-system-x86_64.exe"))
        .map(|(p, _)| p)
        .collect();
    eprintln!(
        "politica {politica}: UI pid {} → servizio pid {svc_pid} → QEMU {qemu:?}",
        ui.id()
    );
    assert_eq!(qemu.len(), 1, "un processo QEMU figlio del servizio");

    let t0 = Instant::now();
    ui.kill().unwrap();
    let _ = ui.wait();
    let mut fine_servizio = None;
    let mut fine_qemu = None;
    while t0.elapsed() < Duration::from_secs(120) {
        if fine_qemu.is_none() && !pid_vivo(qemu[0]) {
            fine_qemu = Some(t0.elapsed());
        }
        if fine_servizio.is_none() && !pid_vivo(svc_pid) {
            fine_servizio = Some(t0.elapsed());
        }
        if fine_qemu.is_some() && fine_servizio.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!(
        "dopo il kill della UI: QEMU terminato in {fine_qemu:?}, servizio in {fine_servizio:?}"
    );
    let audit = default_audit(&dir).unwrap_or_default();
    eprintln!("audit:\n{audit}");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        fine_qemu.expect("QEMU orfano dopo il crash della UI"),
        fine_servizio.expect("servizio ancora vivo dopo il crash della UI"),
        audit,
    ))
}

/// Politica predefinita (`kill_vms`): Emergency Stop subito dopo la perdita
/// della UI, poi arresto del servizio.
#[test]
#[ignore = "richiede QEMU reale con acceleratore: impostare LIGHTVM_QEMU_DIR"]
fn crash_ui_con_vm_reale() {
    let Some((qemu, servizio, audit)) = prova_crash_ui("crash_ui_con_vm_reale", "kill") else {
        eprintln!("LIGHTVM_QEMU_DIR non impostata: prova saltata");
        return;
    };
    assert!(qemu < Duration::from_secs(5), "QEMU terminato in {qemu:?}");
    assert!(
        servizio < Duration::from_secs(10),
        "servizio terminato in {servizio:?}"
    );
    assert!(
        audit.contains(r#""action":"emergency_stop","outcome":"succeeded""#),
        "{audit}"
    );
}

/// `shutdown_vms` con 3 s di attesa: il firmware non risponde all'ACPI, quindi
/// `quit` dopo 3 s; arresto registrato come riuscito, nessun timeout.
#[test]
#[ignore = "richiede QEMU reale con acceleratore: impostare LIGHTVM_QEMU_DIR"]
fn crash_ui_con_vm_reale_spegnimento_ordinato() {
    let Some((qemu, servizio, audit)) =
        prova_crash_ui("crash_ui_con_vm_reale_spegnimento_ordinato", "shutdown:3")
    else {
        eprintln!("LIGHTVM_QEMU_DIR non impostata: prova saltata");
        return;
    };
    assert!(qemu < Duration::from_secs(15), "QEMU terminato in {qemu:?}");
    assert!(
        servizio < Duration::from_secs(20),
        "servizio terminato in {servizio:?}"
    );
    assert!(
        audit.contains(r#""action":"vm_stop","outcome":"succeeded""#),
        "{audit}"
    );
}

/// Righe di audit di arresto, se il file è nel catalogo di prova.
fn default_audit(dir: &std::path::Path) -> Option<String> {
    let mut righe = Vec::new();
    for e in walk(dir) {
        if e.extension().is_some_and(|x| x == "jsonl") {
            let t = std::fs::read_to_string(&e).ok()?;
            righe.extend(
                t.lines()
                    .filter(|l| {
                        l.contains("vm_stop")
                            || l.contains("emergency_stop")
                            || l.contains("parent")
                    })
                    .map(str::to_owned),
            );
        }
    }
    Some(righe.join("\n"))
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
