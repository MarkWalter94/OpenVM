//! Automatic answer to the "Press any key to boot from CD or DVD..." prompt
//! of the Windows UEFI installation media.
//!
//! The UEFI CD boot loader of the Windows media (`cdboot.efi` in
//! `efisys.bin`) waits about 5 seconds for a key before booting the CD; with
//! nobody at the console it returns to the firmware, which tries the next
//! boot entry (the empty disk) and ends in the UEFI Shell. The legacy BIOS
//! boot sector only asks when a bootable hard disk exists, so BIOS profiles
//! are not affected.
//!
//! When a Windows guest boots with UEFI firmware from its installation media
//! onto a disk without an installed system ([`boot_prompt_key_needed`]), the
//! backend presses [`BOOT_PROMPT_KEY`] once every [`BOOT_PROMPT_INTERVAL`]
//! for [`BOOT_PROMPT_WINDOW`] after the vCPUs start ([`press_boot_prompt_key`],
//! started through [`crate::QemuRun::boot_prompt_keys`]), then stops. The
//! presses never fail the start: an error is counted in the report only.

use std::future::Future;
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use lightvm_proto::GuestOs;
use tokio::sync::watch;

use crate::cmdline::{FirmwareKind, VmLaunchSpec};
use crate::error::Result;
use crate::run::ProcessState;

/// QEMU key code pressed at the prompt.
///
/// `ret` produces a keystroke on the firmware console (the prompt accepts any
/// keystroke; modifier keys alone produce none) and has no side effect on
/// the way to Setup:
/// - it is not a firmware hotkey: OVMF and the ArmVirtQemu firmware open
///   their setup menu with Esc (and F2), the boot menu with F12; Enter
///   during the logo or in the "Press ESC to skip startup.nsh" countdown of
///   the Shell only continues;
/// - in a Windows Boot Manager menu (media with several entries) it starts
///   the default entry, where any other key would stop the countdown and
///   leave the menu waiting; F8 (advanced options) is avoided;
/// - while the boot manager loads Windows PE it is ignored.
///
/// The window ([`BOOT_PROMPT_WINDOW`]) ends well before the Setup window
/// appears, where Enter would activate the default button.
pub const BOOT_PROMPT_KEY: &str = "ret";

/// Time after the vCPUs start during which the key is pressed. It covers
/// the firmware start (logo, device enumeration) and the 5 s prompt under
/// WHPX and HVF with a margin, and ends before Windows PE shows Setup (which
/// reads the whole `boot.wim` from the CD first).
pub const BOOT_PROMPT_WINDOW: Duration = Duration::from_secs(25);

/// Interval between two presses: several presses fall inside the 5 s of
/// the prompt whenever it appears in the window.
pub const BOOT_PROMPT_INTERVAL: Duration = Duration::from_secs(1);

/// Largest image file of a disk still considered without an installed
/// system (see [`DiskContent`]).
pub const BLANK_DISK_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// What the VM's disk holds, as far as the backend can tell before the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskContent {
    /// No installed system: a QCOW2 image without a backing file whose image
    /// file is at most [`BLANK_DISK_MAX_BYTES`].
    Blank,
    /// The image file holds more data than a blank disk.
    Used,
    /// Overlay of a baseline (backing file, or explicit backing node of
    /// the run): the base holds the system, whatever the overlay size.
    Backed,
    /// Not readable or not a QCOW2 image: treated as installed.
    Unknown,
}

/// Magic number of a QCOW2 header (`QFI\xfb`).
const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";

/// Classifies the QCOW2 disk at `path`; `backing_node` is `true` when the
/// run opens it with an explicit backing node (overlay of a baseline).
///
/// Criterion for "no installed system": the size of the image file. QCOW2
/// (created by the backend without preallocation) allocates clusters by
/// appending them to the file, so the file length bounds the data the guest
/// has written: a newly created disk is a few hundred KiB (header, L1 and
/// refcount tables) at any virtual size, and growing it only reallocates
/// the small L1 table. Partitioning and formatting by Setup write a few
/// MiB; an installed Windows (boot manager, kernel, registry hives,
/// drivers) needs several GiB, far above [`BLANK_DISK_MAX_BYTES`]. A disk
/// with an installed Windows therefore never counts as blank, so with the
/// media still attached it keeps booting from the disk after every restart
/// (the prompt times out). An overlay (backing file in the header or backing
/// node) is never blank: its system is in the base. Every read error or
/// unexpected format gives [`DiskContent::Unknown`] (no key).
pub fn inspect_disk(path: &Path, backing_node: bool) -> DiskContent {
    if backing_node {
        return DiskContent::Backed;
    }
    let read = || -> std::io::Result<(u64, [u8; 16])> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        let mut header = [0u8; 16];
        f.read_exact(&mut header)?;
        Ok((len, header))
    };
    match read() {
        Ok((len, header)) => classify_qcow2(len, &header),
        Err(_) => DiskContent::Unknown,
    }
}

/// Classification from the file length and the first 16 bytes of the
/// header (magic, version, backing file offset).
fn classify_qcow2(len: u64, header: &[u8; 16]) -> DiskContent {
    if header[..4] != QCOW2_MAGIC {
        return DiskContent::Unknown;
    }
    let mut offset = [0u8; 8];
    offset.copy_from_slice(&header[8..16]);
    if u64::from_be_bytes(offset) != 0 {
        DiskContent::Backed
    } else if len <= BLANK_DISK_MAX_BYTES {
        DiskContent::Blank
    } else {
        DiskContent::Used
    }
}

/// `true` if the installation media boots first: the command line gives the
/// install CD `bootindex=0` and the disk the next index whenever the media
/// is attached ([`VmLaunchSpec::build`]).
pub fn install_media_boots_first(spec: &VmLaunchSpec) -> bool {
    spec.install_media.is_some()
}

/// Decision: press the key at the CD prompt only for a Windows guest, with
/// UEFI firmware (x86 OVMF or AArch64 edk2), booting first from the
/// installation media, onto a disk without an installed system.
pub fn boot_prompt_key_needed(
    guest: GuestOs,
    firmware: FirmwareKind,
    install_media_first: bool,
    disk: DiskContent,
) -> bool {
    let uefi = match firmware {
        FirmwareKind::Uefi | FirmwareKind::UefiAarch64 => true,
        FirmwareKind::SeaBios => false,
    };
    guest.is_windows() && uefi && install_media_first && disk == DiskContent::Blank
}

/// Why the presses ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootKeyEnd {
    /// The window elapsed.
    WindowElapsed,
    /// The VM left the running state (stop requested or process exited).
    VmStopped,
    /// Nothing to do (vCPUs left paused, key not available).
    Skipped,
}

/// Outcome of [`press_boot_prompt_key`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootKeyReport {
    /// Presses accepted by QEMU.
    pub sent: u32,
    /// Presses rejected or timed out.
    pub failed: u32,
    /// Why the presses ended.
    pub end: BootKeyEnd,
}

/// Calls `press` once every `interval` from `started` until
/// `started + window`, as long as the process state is
/// [`ProcessState::Running`]. A failed press is counted and does not stop
/// the loop; a press that does not complete before the window ends is
/// abandoned. Stops early as soon as the state changes away from running.
pub async fn press_boot_prompt_key<F, Fut>(
    started: Instant,
    window: Duration,
    interval: Duration,
    mut state: watch::Receiver<ProcessState>,
    mut press: F,
) -> BootKeyReport
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let deadline = started + window;
    let mut report = BootKeyReport {
        sent: 0,
        failed: 0,
        end: BootKeyEnd::WindowElapsed,
    };
    let mut next = started;
    loop {
        if !matches!(*state.borrow_and_update(), ProcessState::Running) {
            report.end = BootKeyEnd::VmStopped;
            return report;
        }
        let now = Instant::now();
        if now >= deadline {
            return report;
        }
        if now >= next {
            match tokio::time::timeout(deadline - now, press()).await {
                Ok(Ok(())) => report.sent += 1,
                Ok(Err(_)) | Err(_) => report.failed += 1,
            }
            next += interval;
            // A slow press does not cause a burst of catch-up presses.
            let now = Instant::now();
            while next <= now {
                next += interval;
            }
            continue;
        }
        let wait = next.min(deadline) - now;
        match tokio::time::timeout(wait, state.changed()).await {
            // State changed: checked at the top of the loop.
            Ok(Ok(())) | Err(_) => {}
            // The run is gone.
            Ok(Err(_)) => {
                report.end = BootKeyEnd::VmStopped;
                return report;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BackendError;
    use std::sync::Arc;
    use std::sync::Mutex;

    fn header(backing_offset: u64) -> [u8; 16] {
        let mut h = [0u8; 16];
        h[..4].copy_from_slice(&QCOW2_MAGIC);
        h[4..8].copy_from_slice(&3u32.to_be_bytes());
        h[8..16].copy_from_slice(&backing_offset.to_be_bytes());
        h
    }

    #[test]
    fn disk_classification() {
        assert_eq!(classify_qcow2(196_608, &header(0)), DiskContent::Blank);
        assert_eq!(
            classify_qcow2(BLANK_DISK_MAX_BYTES, &header(0)),
            DiskContent::Blank
        );
        assert_eq!(
            classify_qcow2(BLANK_DISK_MAX_BYTES + 1, &header(0)),
            DiskContent::Used
        );
        assert_eq!(classify_qcow2(9 << 30, &header(0)), DiskContent::Used);
        // An overlay is never blank, however small.
        assert_eq!(classify_qcow2(196_608, &header(0x200)), DiskContent::Backed);
        let mut raw = header(0);
        raw[..4].copy_from_slice(b"\0\0\0\0");
        assert_eq!(classify_qcow2(1024, &raw), DiskContent::Unknown);
    }

    #[test]
    fn disk_inspection_on_files() {
        let dir = std::env::temp_dir().join(format!("lightvm-boot-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blank = dir.join("blank.qcow2");
        let mut bytes = header(0).to_vec();
        bytes.resize(196_608, 0);
        std::fs::write(&blank, &bytes).unwrap();
        assert_eq!(inspect_disk(&blank, false), DiskContent::Blank);
        // Explicit backing node of the run (baseline overlay).
        assert_eq!(inspect_disk(&blank, true), DiskContent::Backed);
        let backed = dir.join("backed.qcow2");
        let mut bytes = header(0x1_0000).to_vec();
        bytes.resize(196_608, 0);
        std::fs::write(&backed, &bytes).unwrap();
        assert_eq!(inspect_disk(&backed, false), DiskContent::Backed);
        let short = dir.join("short.qcow2");
        std::fs::write(&short, b"QFI").unwrap();
        assert_eq!(inspect_disk(&short, false), DiskContent::Unknown);
        assert_eq!(
            inspect_disk(&dir.join("missing.qcow2"), false),
            DiskContent::Unknown
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decision_for_every_combination() {
        let guests = &GuestOs::ALL;
        let firmwares = [
            FirmwareKind::Uefi,
            FirmwareKind::UefiAarch64,
            FirmwareKind::SeaBios,
        ];
        let disks = [
            DiskContent::Blank,
            DiskContent::Used,
            DiskContent::Backed,
            DiskContent::Unknown,
        ];
        for &guest in guests {
            for firmware in firmwares {
                for media in [true, false] {
                    for disk in disks {
                        let expected = guest.is_windows()
                            && firmware != FirmwareKind::SeaBios
                            && media
                            && disk == DiskContent::Blank;
                        assert_eq!(
                            boot_prompt_key_needed(guest, firmware, media, disk),
                            expected,
                            "{guest:?} {firmware:?} media={media} {disk:?}"
                        );
                    }
                }
            }
        }
        // The cases of the field.
        assert!(boot_prompt_key_needed(
            GuestOs::Windows10,
            FirmwareKind::Uefi,
            true,
            DiskContent::Blank
        ));
        assert!(boot_prompt_key_needed(
            GuestOs::Windows11Arm64,
            FirmwareKind::UefiAarch64,
            true,
            DiskContent::Blank
        ));
        assert!(!boot_prompt_key_needed(
            GuestOs::Windows10,
            FirmwareKind::SeaBios,
            true,
            DiskContent::Blank
        ));
        assert!(!boot_prompt_key_needed(
            GuestOs::UbuntuLtsX86_64,
            FirmwareKind::Uefi,
            true,
            DiskContent::Blank
        ));
        assert!(!boot_prompt_key_needed(
            GuestOs::Windows10,
            FirmwareKind::Uefi,
            true,
            DiskContent::Used
        ));
        assert!(!boot_prompt_key_needed(
            GuestOs::Windows10,
            FirmwareKind::Uefi,
            true,
            DiskContent::Backed
        ));
        assert!(!boot_prompt_key_needed(
            GuestOs::Windows10,
            FirmwareKind::Uefi,
            false,
            DiskContent::Blank
        ));
    }

    #[test]
    fn key_is_allowed_and_constants_cover_the_prompt() {
        let q = lightvm_proto::QCode::parse(BOOT_PROMPT_KEY).unwrap();
        assert!(crate::input::is_supported_qcode(&q));
        // At least a few presses inside the 5 s of the prompt.
        assert!(BOOT_PROMPT_INTERVAL * 3 <= Duration::from_secs(5));
        assert!(BOOT_PROMPT_WINDOW >= Duration::from_secs(20));
        assert!(BOOT_PROMPT_WINDOW <= Duration::from_secs(30));
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    /// Press times relative to the start, recorded by the fake sender.
    type Presses = Arc<Mutex<Vec<Duration>>>;

    fn recorder(
        started: Instant,
        fail: bool,
    ) -> (Presses, impl FnMut() -> std::future::Ready<Result<()>>) {
        let presses: Presses = Arc::default();
        let p = presses.clone();
        let press = move || {
            p.lock().unwrap().push(started.elapsed());
            std::future::ready(if fail {
                Err(BackendError::Timeout("input-send-event"))
            } else {
                Ok(())
            })
        };
        (presses, press)
    }

    #[test]
    fn presses_within_the_window_and_not_after() {
        let window = Duration::from_millis(600);
        let interval = Duration::from_millis(100);
        let (tx, rx) = watch::channel(ProcessState::Running);
        let started = Instant::now();
        let (presses, press) = recorder(started, false);
        let report = rt().block_on(press_boot_prompt_key(started, window, interval, rx, press));
        assert_eq!(report.end, BootKeyEnd::WindowElapsed);
        assert!(started.elapsed() >= window);
        let presses = presses.lock().unwrap().clone();
        assert_eq!(report.sent as usize, presses.len());
        assert_eq!(report.failed, 0);
        // One at the start, then one per interval: 6 in 600 ms (timer
        // granularity may drop the last one).
        assert!((4..=6).contains(&presses.len()), "{presses:?}");
        assert!(presses[0] < interval);
        assert!(presses.iter().all(|t| *t < window), "{presses:?}");
        assert!(presses.windows(2).all(|w| w[1] >= w[0] + interval / 2));
        drop(tx);
    }

    #[test]
    fn failures_do_not_stop_the_presses() {
        let (_tx, rx) = watch::channel(ProcessState::Running);
        let started = Instant::now();
        let (presses, press) = recorder(started, true);
        let report = rt().block_on(press_boot_prompt_key(
            started,
            Duration::from_millis(350),
            Duration::from_millis(100),
            rx,
            press,
        ));
        assert_eq!(report.end, BootKeyEnd::WindowElapsed);
        assert_eq!(report.sent, 0);
        assert!(report.failed >= 3, "{report:?}");
        assert_eq!(report.failed as usize, presses.lock().unwrap().len());
    }

    #[test]
    fn stops_early_when_the_vm_stops() {
        let (tx, rx) = watch::channel(ProcessState::Running);
        let started = Instant::now();
        let (presses, press) = recorder(started, false);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            tx.send_replace(ProcessState::Stopping);
            tx
        });
        let report = rt().block_on(press_boot_prompt_key(
            started,
            Duration::from_secs(30),
            Duration::from_millis(100),
            rx,
            press,
        ));
        let elapsed = started.elapsed();
        let _tx = stopper.join().unwrap();
        assert_eq!(report.end, BootKeyEnd::VmStopped);
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        let presses = presses.lock().unwrap();
        assert!(!presses.is_empty() && presses.len() <= 4, "{presses:?}");
        assert!(presses.iter().all(|t| *t < Duration::from_millis(260)));
    }

    #[test]
    fn nothing_when_not_running_or_run_gone() {
        let (_tx, rx) = watch::channel(ProcessState::Exited {
            code: Some(0),
            cause: crate::run::ExitCause::GuestShutdown,
        });
        let started = Instant::now();
        let (presses, press) = recorder(started, false);
        let report = rt().block_on(press_boot_prompt_key(
            started,
            Duration::from_secs(30),
            Duration::from_millis(100),
            rx,
            press,
        ));
        assert_eq!(report.end, BootKeyEnd::VmStopped);
        assert!(presses.lock().unwrap().is_empty());

        // Sender dropped (run released) while waiting for the next press.
        let (tx, rx) = watch::channel(ProcessState::Running);
        let (presses, press) = recorder(started, false);
        let dropper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(tx);
        });
        let report = rt().block_on(press_boot_prompt_key(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(1),
            rx,
            press,
        ));
        dropper.join().unwrap();
        assert_eq!(report.end, BootKeyEnd::VmStopped);
        assert_eq!(presses.lock().unwrap().len(), 1);
    }
}
