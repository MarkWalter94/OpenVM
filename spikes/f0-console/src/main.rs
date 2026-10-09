//! `f0-console`: binario di prova per F0-04 (non è codice di prodotto).
//!
//! Sottocomandi:
//! - `probe-unix <socket> [socket2|uds]`: collegamento AF_UNIX e lettura del banner;
//! - `rfb-snapshot <trasporto> --out <png>`: handshake RFB, primo aggiornamento completo, PNG;
//! - `rfb-fps <trasporto> --secs N [--rate R]`: frequenza degli aggiornamenti;
//! - `qmp --qmp-socket <s> [--geom WxH] <azione> ...`: status, mice, accel, move X Y,
//!   click X Y [left|right|middle], key COMBO, type TESTO, wheel up|down N,
//!   screendump <png> --tmp <dir>;
//! - `bench-screendump --qmp-socket <s> --tmp <dir> --n N [--save-last <png>]`;
//! - `wait-desktop --qmp-socket <s> --tmp <dir> --timeout S`.
//!
//! Trasporto RFB: `--vnc-socket <s> [--impl socket2|uds]` oppure
//! `--vnc-tls 127.0.0.1:PORTA --tls-dir <dir>` (ca-cert.pem, client-cert.pem, client-key.pem).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use f0_console::image::{RgbImage, encode_png, parse_ppm};
use f0_console::qmp::{Button, InputEvent, Qmp, scale_abs};
use f0_console::stats::summarize;
use f0_console::typing::{combo_key, us_char};
use rfb_client::{Connection, TlsConfig, Transport, UnixImpl};

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("probe-unix") => probe_unix(&args[1..]),
        Some("rfb-snapshot") => rfb_snapshot(&args[1..]),
        Some("rfb-fps") => rfb_fps(&args[1..]),
        Some("qmp") => qmp_cmd(&args[1..]),
        Some("bench-screendump") => bench_screendump(&args[1..]),
        Some("wait-desktop") => wait_desktop(&args[1..]),
        Some("diff") => diff_cmd(&args[1..]),
        Some("session-stop") => session_stop(&args[1..]),
        Some("rfb-idle") => rfb_idle(&args[1..]),
        _ => Err("uso: vedere la documentazione del modulo (sottocomando mancante)".into()),
    };
    match res {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            println!("ERRORE {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn transport(args: &[String]) -> R<Transport> {
    if let Some(p) = opt(args, "--vnc-socket") {
        let imp = match opt(args, "--impl").unwrap_or("socket2") {
            "socket2" => UnixImpl::Socket2,
            "uds" => UnixImpl::UdsWindows,
            o => return Err(format!("implementazione sconosciuta: {o}").into()),
        };
        return Ok(Transport::Unix {
            path: PathBuf::from(p),
            imp,
        });
    }
    if let Some(a) = opt(args, "--vnc-tls") {
        let dir = PathBuf::from(opt(args, "--tls-dir").ok_or("--tls-dir mancante")?);
        return Ok(Transport::VencryptTls {
            addr: a.parse()?,
            tls: TlsConfig {
                ca_cert: dir.join("ca-cert.pem"),
                client_cert: dir.join("client-cert.pem"),
                client_key: dir.join("client-key.pem"),
                server_name: opt(args, "--server-name").unwrap_or("localhost").to_owned(),
            },
        });
    }
    Err("specificare --vnc-socket oppure --vnc-tls".into())
}

fn probe_unix(args: &[String]) -> R<()> {
    let path = Path::new(args.first().ok_or("percorso mancante")?);
    let imp = args.get(1).map_or("socket2", String::as_str);
    let t0 = Instant::now();
    let mut buf = vec![0u8; 512];
    let n = match imp {
        "socket2" => {
            let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
            s.connect(&socket2::SockAddr::unix(path)?)?;
            s.set_read_timeout(Some(Duration::from_secs(3)))?;
            let n = (&s).read(&mut buf)?;
            second_read(|b| (&s).read(b), "originale");
            let c = s.try_clone()?;
            c.set_read_timeout(Some(Duration::from_millis(500)))?;
            s.set_read_timeout(None)?;
            println!(
                "PROBE timeout impostato solo sul duplicato (500 ms), lettura sull'originale in un thread:"
            );
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                second_read(|b| (&s).read(b), "originale-dopo-timeout-sul-duplicato")
                    == tx.send(()).is_ok()
            });
            if rx.recv_timeout(Duration::from_secs(3)).is_err() {
                println!(
                    "PROBE lettura ancora bloccata dopo 3 s: il timeout del duplicato non vale per l'originale"
                );
            }
            n
        }
        #[cfg(windows)]
        "uds" => {
            let mut s = uds_windows::UnixStream::connect(path)?;
            s.set_read_timeout(Some(Duration::from_secs(3)))?;
            s.read(&mut buf)?
        }
        o => return Err(format!("implementazione sconosciuta: {o}").into()),
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    let first = text.lines().next().unwrap_or("");
    println!(
        "PROBE impl={imp} connesso=si byte={n} ms={} prima_riga={first:?}",
        t0.elapsed().as_millis()
    );
    Ok(())
}

fn save_fb_png(fb: &rfb_client::Framebuffer, out: &Path) -> R<RgbImage> {
    let img = RgbImage {
        width: u32::from(fb.width()),
        height: u32::from(fb.height()),
        data: fb.to_rgb(),
    };
    std::fs::write(out, encode_png(&img)?)?;
    Ok(img)
}

fn rfb_snapshot(args: &[String]) -> R<()> {
    let t = transport(args)?;
    let out = PathBuf::from(opt(args, "--out").ok_or("--out mancante")?);
    let t0 = Instant::now();
    let mut c = Connection::connect(&t)?;
    let hs = t0.elapsed();
    let init = c.server_init().clone();
    println!(
        "RFB connesso canale={:?} sicurezza=\"{}\" handshake_ms={} server_init={}x{} bpp={} depth={} nome={:?}",
        c.channel(),
        c.security(),
        hs.as_millis(),
        init.width,
        init.height,
        init.pixel_format.bits_per_pixel,
        init.pixel_format.depth,
        init.name
    );
    c.request_update()?;
    let u = c.read_update()?;
    let first = t0.elapsed();
    let img = save_fb_png(c.framebuffer(), &out)?;
    println!(
        "RFB primo_aggiornamento_ms={} rettangoli={} geometria={}x{} non_neri={:.2}% stats={:?}",
        first.as_millis(),
        u.rects.len(),
        u.width,
        u.height,
        img.non_black_percent(),
        c.stats()
    );
    println!("RFB png={}", out.display());
    Ok(())
}

fn rfb_fps(args: &[String]) -> R<()> {
    let t = transport(args)?;
    let secs: f64 = opt(args, "--secs").unwrap_or("30").parse()?;
    let rate: u32 = opt(args, "--rate").unwrap_or("60").parse()?;
    let mut c = Connection::connect(&t)?;
    c.request_update()?;
    c.read_update()?; // aggiornamento completo iniziale, escluso dalla misura
    let min_interval = Duration::from_secs(1) / rate.max(1);
    let start = Instant::now();
    let mut per_sec: Vec<(u64, u64)> = vec![(0, 0); secs.ceil() as usize + 1];
    let mut intervals = Vec::new();
    let mut last: Option<Instant> = None;
    let stats0 = c.stats().clone();
    while start.elapsed().as_secs_f64() < secs {
        let req = Instant::now();
        c.request_update()?;
        let u = c.read_update()?;
        let now = Instant::now();
        let s = now.duration_since(start).as_secs() as usize;
        if s < per_sec.len() {
            per_sec[s].0 += 1;
            per_sec[s].1 += u.rects.iter().map(|r| r.area()).sum::<u64>();
        }
        if let Some(l) = last.replace(now) {
            intervals.push(now.duration_since(l).as_secs_f64() * 1000.0);
        }
        let el = req.elapsed();
        if el < min_interval {
            std::thread::sleep(min_interval - el);
        }
    }
    let total = start.elapsed().as_secs_f64();
    let n = c.stats().updates - stats0.updates;
    println!(
        "FPS durata_s={total:.1} aggiornamenti={n} media_al_s={:.2} limite_richieste_al_s={rate}",
        n as f64 / total
    );
    let full: Vec<String> = per_sec
        .iter()
        .take(secs as usize)
        .map(|(u, px)| format!("{u}/{}k", px / 1000))
        .collect();
    println!("FPS per_secondo(aggiornamenti/kpixel)={}", full.join(" "));
    if !intervals.is_empty() {
        let s = summarize(&intervals);
        println!(
            "FPS intervallo_ms n={} p50={:.1} p95={:.1} max={:.1}",
            s.n, s.p50, s.p95, s.max
        );
    }
    let st = c.stats();
    println!(
        "FPS rettangoli raw={} copyrect={} hextile={} zrle={} desktopsize={} pixel={} resize={}",
        st.rects_by_encoding[0] - stats0.rects_by_encoding[0],
        st.rects_by_encoding[1] - stats0.rects_by_encoding[1],
        st.rects_by_encoding[2] - stats0.rects_by_encoding[2],
        st.rects_by_encoding[3] - stats0.rects_by_encoding[3],
        st.rects_by_encoding[4] - stats0.rects_by_encoding[4],
        st.pixels - stats0.pixels,
        st.resizes - stats0.resizes
    );
    Ok(())
}

fn geom(args: &[String]) -> R<(u32, u32)> {
    let g = opt(args, "--geom").unwrap_or("1280x800");
    let (w, h) = g.split_once('x').ok_or("--geom WxH")?;
    Ok((w.parse()?, h.parse()?))
}

fn screendump_png(q: &mut Qmp, tmp: &Path, out: &Path) -> R<RgbImage> {
    let ppm = tmp.join("f0-04-shot.ppm");
    q.screendump_ppm(&ppm)?;
    let img = parse_ppm(&std::fs::read(&ppm)?)?;
    std::fs::write(out, encode_png(&img)?)?;
    let _ = std::fs::remove_file(&ppm);
    Ok(img)
}

fn qmp_cmd(args: &[String]) -> R<()> {
    let sock = PathBuf::from(opt(args, "--qmp-socket").ok_or("--qmp-socket mancante")?);
    let (gw, gh) = geom(args)?;
    // azione = primo argomento posizionale dopo le opzioni con valore
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            i += 2;
        } else {
            pos.push(args[i].as_str());
            i += 1;
        }
    }
    let mut q = Qmp::connect(&sock)?;
    let t0 = Instant::now();
    match pos.as_slice() {
        ["status"] => println!("QMP status={}", q.query_status()?),
        ["mice"] => println!("QMP mice={}", q.query_mice()?),
        ["accel"] => println!("QMP accel={}", q.query_accelerators()?),
        ["move", x, y] => {
            let (x, y): (u32, u32) = (x.parse()?, y.parse()?);
            q.send_input(&[InputEvent::Abs {
                x: scale_abs(x, gw),
                y: scale_abs(y, gh),
            }])?;
            println!(
                "QMP move {x},{y} su {gw}x{gh} -> abs {},{}",
                scale_abs(x, gw),
                scale_abs(y, gh)
            );
        }
        ["click", x, y, rest @ ..] => {
            let (x, y): (u32, u32) = (x.parse()?, y.parse()?);
            let button = match rest.first().copied().unwrap_or("left") {
                "left" => Button::Left,
                "right" => Button::Right,
                "middle" => Button::Middle,
                o => return Err(format!("pulsante {o}").into()),
            };
            let abs = InputEvent::Abs {
                x: scale_abs(x, gw),
                y: scale_abs(y, gh),
            };
            q.send_input(&[abs, InputEvent::Btn { button, down: true }])?;
            std::thread::sleep(Duration::from_millis(60));
            q.send_input(&[InputEvent::Btn {
                button,
                down: false,
            }])?;
            println!("QMP click {button:?} {x},{y} su {gw}x{gh}");
        }
        ["key", combo] => {
            let keys: Vec<&'static str> = combo
                .split('-')
                .map(|k| combo_key(k).ok_or(format!("tasto {k} sconosciuto")))
                .collect::<Result<_, _>>()?;
            let mut ev: Vec<InputEvent> = keys
                .iter()
                .map(|k| InputEvent::Key {
                    qcode: k,
                    down: true,
                })
                .collect();
            q.send_input(&ev)?;
            std::thread::sleep(Duration::from_millis(60));
            ev = keys
                .iter()
                .rev()
                .map(|k| InputEvent::Key {
                    qcode: k,
                    down: false,
                })
                .collect();
            q.send_input(&ev)?;
            println!("QMP key {keys:?}");
        }
        ["type", text] => {
            let mut sent = 0;
            for ch in text.chars() {
                let (k, shift) = us_char(ch).ok_or(format!("carattere {ch:?} non coperto (US)"))?;
                let mut down = Vec::new();
                if shift {
                    down.push(InputEvent::Key {
                        qcode: "shift",
                        down: true,
                    });
                }
                down.push(InputEvent::Key {
                    qcode: k,
                    down: true,
                });
                q.send_input(&down)?;
                std::thread::sleep(Duration::from_millis(15));
                let mut up = vec![InputEvent::Key {
                    qcode: k,
                    down: false,
                }];
                if shift {
                    up.push(InputEvent::Key {
                        qcode: "shift",
                        down: false,
                    });
                }
                q.send_input(&up)?;
                std::thread::sleep(Duration::from_millis(15));
                sent += 1;
            }
            println!("QMP type caratteri={sent} testo={text:?}");
        }
        ["taps", list] => {
            let mut n = 0;
            for k in list.split(',') {
                let code = combo_key(k)
                    .or_else(|| EXTRA_QCODES.iter().find(|e| **e == k).copied())
                    .ok_or(format!("qcode {k} non ammesso"))?;
                q_tap(&mut q, code)?;
                n += 1;
            }
            println!("QMP taps n={n} sequenza={list}");
        }
        ["wheel", dir, n] => {
            let button = if *dir == "up" {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            for _ in 0..n.parse::<u32>()? {
                q.send_input(&[InputEvent::Btn { button, down: true }])?;
                q.send_input(&[InputEvent::Btn {
                    button,
                    down: false,
                }])?;
            }
            println!("QMP wheel {dir} {n}");
        }
        ["screendump", out] => {
            let tmp = PathBuf::from(opt(args, "--tmp").ok_or("--tmp mancante")?);
            let img = screendump_png(&mut q, &tmp, Path::new(out))?;
            println!(
                "QMP screendump {}x{} non_neri={:.2}% png={out}",
                img.width,
                img.height,
                img.non_black_percent()
            );
        }
        ["quit"] => {
            q.quit()?;
            println!("QMP quit");
        }
        other => return Err(format!("azione QMP non valida: {other:?}").into()),
    }
    println!("QMP durata_ms={}", t0.elapsed().as_millis());
    for e in &q.events {
        println!("QMP evento {e}");
    }
    Ok(())
}

fn bench_screendump(args: &[String]) -> R<()> {
    let sock = PathBuf::from(opt(args, "--qmp-socket").ok_or("--qmp-socket mancante")?);
    let tmp = PathBuf::from(opt(args, "--tmp").ok_or("--tmp mancante")?);
    let n: usize = opt(args, "--n").unwrap_or("100").parse()?;
    let mut q = Qmp::connect(&sock)?;
    let ppm = tmp.join("f0-04-bench.ppm");
    let (mut tot, mut t_qmp, mut t_parse, mut t_png) = (vec![], vec![], vec![], vec![]);
    let mut errors = Vec::new();
    let mut last: Option<(RgbImage, Vec<u8>)> = None;
    for i in 0..n {
        let t0 = Instant::now();
        let r: R<(RgbImage, Vec<u8>, [f64; 3])> = (|| {
            q.screendump_ppm(&ppm)?;
            let t1 = Instant::now();
            let img = parse_ppm(&std::fs::read(&ppm)?)?;
            let t2 = Instant::now();
            let png = encode_png(&img)?;
            let t3 = Instant::now();
            Ok((
                img,
                png,
                [
                    t1.duration_since(t0).as_secs_f64() * 1000.0,
                    t2.duration_since(t1).as_secs_f64() * 1000.0,
                    t3.duration_since(t2).as_secs_f64() * 1000.0,
                ],
            ))
        })();
        let el = t0.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok((img, png, ph)) => {
                tot.push(el);
                t_qmp.push(ph[0]);
                t_parse.push(ph[1]);
                t_png.push(ph[2]);
                last = Some((img, png));
            }
            Err(e) => errors.push(format!("campione {i}: {e}")),
        }
    }
    let _ = std::fs::remove_file(&ppm);
    for (name, v) in [
        ("totale", &tot),
        ("qmp_screendump", &t_qmp),
        ("lettura_parse_ppm", &t_parse),
        ("png_in_memoria", &t_png),
    ] {
        let s = summarize(v);
        println!(
            "BENCH {name} n={} min={:.1} p50={:.1} p95={:.1} max={:.1} media={:.1} ms",
            s.n, s.min, s.p50, s.p95, s.max, s.mean
        );
    }
    println!("BENCH errori={} timeout=0", errors.len());
    for e in &errors {
        println!("BENCH errore {e}");
    }
    if let Some((img, png)) = last {
        println!(
            "BENCH ultima_immagine={}x{} png_byte={} non_neri={:.2}%",
            img.width,
            img.height,
            png.len(),
            img.non_black_percent()
        );
        if let Some(out) = opt(args, "--save-last") {
            std::fs::write(out, &png)?;
        }
    }
    Ok(())
}

fn wait_desktop(args: &[String]) -> R<()> {
    let sock = PathBuf::from(opt(args, "--qmp-socket").ok_or("--qmp-socket mancante")?);
    let tmp = PathBuf::from(opt(args, "--tmp").ok_or("--tmp mancante")?);
    let timeout: u64 = opt(args, "--timeout").unwrap_or("240").parse()?;
    let mut q = Qmp::connect(&sock)?;
    let ppm = tmp.join("f0-04-wait.ppm");
    let start = Instant::now();
    loop {
        q.screendump_ppm(&ppm)?;
        let img = parse_ppm(&std::fs::read(&ppm)?)?;
        let nb = img.non_black_percent();
        println!(
            "WAIT t={}s {}x{} non_neri={nb:.2}%",
            start.elapsed().as_secs(),
            img.width,
            img.height
        );
        if nb > 50.0 {
            let _ = std::fs::remove_file(&ppm);
            println!("WAIT desktop_probabile t={}s", start.elapsed().as_secs());
            return Ok(());
        }
        if start.elapsed().as_secs() > timeout {
            return Err("timeout in attesa del desktop".into());
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Qcode aggiuntivi ammessi da `taps` per le prove del layout IT.
const EXTRA_QCODES: [&str; 12] = [
    "minus",
    "equal",
    "bracket_left",
    "bracket_right",
    "backslash",
    "semicolon",
    "apostrophe",
    "grave_accent",
    "comma",
    "dot",
    "slash",
    "less",
];

fn q_tap(q: &mut Qmp, k: &'static str) -> R<()> {
    q.send_input(&[InputEvent::Key {
        qcode: k,
        down: true,
    }])?;
    std::thread::sleep(Duration::from_millis(15));
    q.send_input(&[InputEvent::Key {
        qcode: k,
        down: false,
    }])?;
    std::thread::sleep(Duration::from_millis(15));
    Ok(())
}

fn load_png(path: &str) -> R<RgbImage> {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG troppo grande")?];
    let info = reader.next_frame(&mut buf)?;
    if info.color_type != png::ColorType::Rgb || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "PNG {:?} {:?} non supportato",
            info.color_type, info.bit_depth
        )
        .into());
    }
    buf.truncate(info.buffer_size());
    Ok(RgbImage {
        width: info.width,
        height: info.height,
        data: buf,
    })
}

/// `diff a.png b.png`: pixel diversi e rettangolo che li contiene.
fn diff_cmd(args: &[String]) -> R<()> {
    let a = load_png(args.first().ok_or("primo PNG mancante")?)?;
    let b = load_png(args.get(1).ok_or("secondo PNG mancante")?)?;
    match a.diff_pixels(&b) {
        None => println!(
            "DIFF dimensioni diverse {}x{} / {}x{}",
            a.width, a.height, b.width, b.height
        ),
        Some(n) => println!(
            "DIFF {}x{} pixel_diversi={n} ({:.3}%) bbox={:?}",
            a.width,
            a.height,
            n as f64 * 100.0 / f64::from(a.width * a.height),
            a.diff_bbox(&b)
        ),
    }
    Ok(())
}

/// `session-stop <trasporto>`: avvia una `Session`, attende il primo
/// aggiornamento e misura quanto impiega `stop()` a chiudere il thread.
fn session_stop(args: &[String]) -> R<()> {
    use std::sync::mpsc::channel;
    let t = transport(args)?;
    let (tx, rx) = channel();
    let mut s = rfb_client::Session::spawn(
        rfb_client::SessionConfig {
            transport: t,
            max_updates_per_second: 30,
        },
        move |e| {
            let _ = tx.send(format!("{e:?}").chars().take(120).collect::<String>());
        },
    );
    let first = rx.recv_timeout(Duration::from_secs(10))?;
    println!("SESSION primo_evento={first}");
    let _ = rx.recv_timeout(Duration::from_secs(10))?;
    std::thread::sleep(Duration::from_secs(1));
    let t0 = Instant::now();
    let (dtx, drx) = channel();
    std::thread::spawn(move || {
        s.stop();
        let _ = dtx.send(());
    });
    match drx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => println!("SESSION stop_ms={}", t0.elapsed().as_millis()),
        Err(_) => println!("SESSION stop BLOCCATO oltre 5 s"),
    }
    if let Ok(e) = rx.recv_timeout(Duration::from_secs(15)) {
        println!(
            "SESSION evento_tardivo dopo {} ms: {e}",
            t0.elapsed().as_millis()
        );
    } else {
        println!("SESSION nessun evento entro 15 s dallo stop");
    }
    while let Ok(e) = rx.try_recv() {
        println!("SESSION evento={e}");
    }
    Ok(())
}

/// Seconda lettura su un socket senza dati in arrivo: deve scadere per timeout.
fn second_read(mut f: impl FnMut(&mut [u8]) -> std::io::Result<usize>, label: &str) -> bool {
    let mut b = [0u8; 16];
    let t0 = Instant::now();
    let r = f(&mut b);
    println!(
        "PROBE seconda_lettura[{label}] esito={:?} ms={}",
        r.map_err(|e| e.kind()),
        t0.elapsed().as_millis()
    );
    true
}

/// `rfb-idle <trasporto> --qmp-socket S --idle N`: connessione interrompibile
/// (timeout di lettura brevi), attesa di N secondi senza aggiornamenti, poi un
/// tasto via QMP (`x` + backspace) per cambiare lo schermo: verifica che
/// l'aggiornamento arrivi dopo che i timeout di `recv` sono scattati.
fn rfb_idle(args: &[String]) -> R<()> {
    let t = transport(args)?;
    let sock = PathBuf::from(opt(args, "--qmp-socket").ok_or("--qmp-socket mancante")?);
    let idle: u64 = opt(args, "--idle").unwrap_or("10").parse()?;
    let keys: Vec<&'static str> = opt(args, "--keys")
        .unwrap_or("x,backspace")
        .split(',')
        .map(|k| combo_key(k).ok_or(format!("tasto {k}")))
        .collect::<Result<_, _>>()?;
    let mut c = Connection::connect(&t)?;
    c.request_update()?;
    c.read_update()?;
    c.request_update()?;
    let start = Instant::now();
    let h = std::thread::spawn(move || -> Result<(), String> {
        std::thread::sleep(Duration::from_secs(idle));
        let mut q = Qmp::connect(&sock).map_err(|e| e.to_string())?;
        for k in keys {
            q.send_input(&[InputEvent::Key {
                qcode: k,
                down: true,
            }])
            .map_err(|e| e.to_string())?;
            q.send_input(&[InputEvent::Key {
                qcode: k,
                down: false,
            }])
            .map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(())
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let r = c.read_update().map(|u| u.rects.len());
            let ok = r.is_ok();
            if tx.send(r.map_err(|e| e.to_string())).is_err() || !ok || c.request_update().is_err()
            {
                break;
            }
        }
    });
    let mode = "bloccante";
    let key_ms = u128::from(idle) * 1000;
    let mut before = Vec::new();
    let mut after = Vec::new();
    while start.elapsed() < Duration::from_secs(idle + 4) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(_)) => {
                let ms = start.elapsed().as_millis();
                if ms < key_ms {
                    before.push(ms)
                } else {
                    after.push(ms)
                }
            }
            Ok(Err(e)) => {
                println!("IDLE errore {e}");
                break;
            }
            Err(_) => {}
        }
    }
    let longest_gap = before
        .windows(2)
        .map(|w| w[1] - w[0])
        .chain(before.first().copied())
        .max()
        .unwrap_or(key_ms);
    println!(
        "IDLE modo={mode} tasto_a_ms={key_ms} aggiornamenti_prima={} pausa_max_senza_aggiornamenti_prima_ms={longest_gap} aggiornamenti_dopo={} primo_dopo_ms={:?}",
        before.len(),
        after.len(),
        after.first()
    );
    println!("IDLE thread_qmp={:?}", h.join().map_err(|_| "panic")?);
    Ok(())
}
