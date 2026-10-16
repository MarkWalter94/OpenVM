//! Windows guests (ADR-0027): firmware allowed per guest, per-guest
//! minimum resources at creation, hardware profile fixed after creation.

mod common;

use common::*;
use vm_core::proto::*;

fn windows(h: &mut Harness, name: &str, os: GuestOs, firmware: Firmware) -> VmConfig {
    let m = h.install_media();
    let mut c = h.vm_config(name, Some(m));
    c.guest_os = os;
    c.firmware = firmware;
    let (vcpus, memory, disk) = os.suggested_resources();
    c.vcpus = vcpus;
    c.memory_mib = memory;
    c.disk_gib = disk;
    c
}

fn invalid(r: Result<VmRecord, PublicError>) -> PublicError {
    let e = r.expect_err("refused");
    assert_eq!(e.code, ErrorCode::InvalidArgument, "{e:?}");
    e
}

fn detail(e: &PublicError, k: &str) -> Option<String> {
    e.details.as_ref().and_then(|d| d.get(k)).cloned()
}

#[test]
fn every_allowed_guest_and_firmware_is_created() {
    let mut h = Harness::new();
    for (i, os) in GuestOs::ALL.into_iter().enumerate() {
        for fw in os.allowed_firmware() {
            let c = windows(&mut h, &format!("vm-{i}-{fw:?}"), os, *fw);
            let vm = h.core.create_vm(&Caller::User, c).expect("created");
            assert_eq!((vm.config.guest_os, vm.config.firmware), (os, *fw));
        }
    }
}

#[test]
fn firmware_outside_the_allowed_set_is_refused() {
    let mut h = Harness::new();
    for (os, fw) in [
        (GuestOs::Windows7, Firmware::Uefi),
        (GuestOs::WindowsVista, Firmware::Uefi),
        (GuestOs::WindowsXp, Firmware::Uefi),
        (GuestOs::UbuntuLtsX86_64, Firmware::Bios),
        (GuestOs::OtherLinuxX86_64, Firmware::Bios),
    ] {
        let c = windows(&mut h, "bad-fw", os, fw);
        let e = invalid(h.core.create_vm(&Caller::User, c));
        assert_eq!(detail(&e, "field").as_deref(), Some("firmware"), "{e:?}");
        assert!(e.message.contains("cannot boot"), "{}", e.message);
    }
    assert!(h.store.0.borrow().vms.is_empty(), "no VM created");
    // Refusals are audited.
    assert!(
        h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::VmCreate && e.outcome == AuditOutcome::Denied)
    );
}

#[test]
fn per_guest_minimum_resources_at_creation() {
    let mut h = Harness::new();
    // Windows 10: 2048 MiB and 32 GiB.
    let mut c = windows(&mut h, "w10", GuestOs::Windows10, Firmware::Uefi);
    c.memory_mib = 2047;
    let e = invalid(h.core.create_vm(&Caller::User, c));
    assert_eq!(detail(&e, "min").as_deref(), Some("2048"));
    let mut c = windows(&mut h, "w10", GuestOs::Windows10, Firmware::Uefi);
    c.disk_gib = 31;
    let e = invalid(h.core.create_vm(&Caller::User, c));
    assert_eq!(detail(&e, "min").as_deref(), Some("32"));
    // Windows 7: 1024 MiB and 20 GiB.
    let mut c = windows(&mut h, "w7", GuestOs::Windows7, Firmware::Bios);
    c.memory_mib = 1023;
    invalid(h.core.create_vm(&Caller::User, c));
    let mut c = windows(&mut h, "w7", GuestOs::Windows7, Firmware::Bios);
    c.memory_mib = 1024;
    c.disk_gib = 20;
    h.core.create_vm(&Caller::User, c).expect("at the minimum");
    // Windows XP goes below the Linux floor: 256 MiB.
    let mut c = windows(&mut h, "xp", GuestOs::WindowsXp, Firmware::Bios);
    c.memory_mib = 256;
    c.disk_gib = 8;
    h.core
        .create_vm(&Caller::User, c)
        .expect("XP at its minimum");
    // Linux unchanged: 512 MiB and 4 GiB.
    let m = h.install_media();
    let mut c = h.vm_config("linux", Some(m));
    c.memory_mib = 511;
    invalid(h.core.create_vm(&Caller::User, c));
    let mut c = h.vm_config("linux", Some(m));
    c.memory_mib = 512;
    c.disk_gib = 4;
    h.core
        .create_vm(&Caller::User, c)
        .expect("Linux at its minimum");
}

#[test]
fn hardware_profile_cannot_change_after_creation() {
    let mut h = Harness::new();
    let c = windows(&mut h, "w10", GuestOs::Windows10, Firmware::Uefi);
    let vm = h.core.create_vm(&Caller::User, c.clone()).unwrap();
    // Firmware change (even an allowed one).
    let mut n = c.clone();
    n.firmware = Firmware::Bios;
    let e = invalid(h.core.update_vm_config(&Caller::User, vm.id, 1, n));
    assert_eq!(detail(&e, "field").as_deref(), Some("firmware"));
    // Guest change between Windows releases with the same firmware set.
    let mut n = c.clone();
    n.guest_os = GuestOs::Windows81;
    let e = invalid(h.core.update_vm_config(&Caller::User, vm.id, 1, n));
    assert_eq!(detail(&e, "field").as_deref(), Some("guest_os"));
    // Windows to Linux (same firmware).
    let mut n = c.clone();
    n.guest_os = GuestOs::UbuntuLtsX86_64;
    invalid(h.core.update_vm_config(&Caller::User, vm.id, 1, n));
    // Other changes are still accepted.
    let mut n = c;
    n.memory_mib = 8192;
    let r = h.core.update_vm_config(&Caller::User, vm.id, 1, n).unwrap();
    assert_eq!(r.config_revision, 2);

    // Linux to Linux is allowed; Linux to Windows is not.
    let lin = h.create_vm("linux");
    let cur = h.core.vm_status(&Caller::User, lin).unwrap();
    let rev = cur.config_revision;
    let m = h.install_media();
    let mut n = h.vm_config("linux", Some(m));
    n.guest_os = GuestOs::OtherLinuxX86_64;
    let r = h
        .core
        .update_vm_config(&Caller::User, lin, rev, n.clone())
        .unwrap();
    let mut w = n;
    w.guest_os = GuestOs::Windows10;
    invalid(
        h.core
            .update_vm_config(&Caller::User, lin, r.config_revision, w),
    );
}

#[test]
fn windows_11_arm64_profile_cannot_change_after_creation() {
    let mut h = Harness::new();
    let c = windows(&mut h, "w11", GuestOs::Windows11Arm64, Firmware::Uefi);
    let vm = h.core.create_vm(&Caller::User, c.clone()).unwrap();
    for os in [
        GuestOs::Windows10,
        GuestOs::UbuntuLtsX86_64,
        GuestOs::OtherLinuxX86_64,
    ] {
        let mut n = c.clone();
        n.guest_os = os;
        let e = invalid(h.core.update_vm_config(&Caller::User, vm.id, 1, n));
        assert_eq!(detail(&e, "field").as_deref(), Some("guest_os"));
    }
    let mut n = c.clone();
    n.firmware = Firmware::Bios;
    let e = invalid(h.core.update_vm_config(&Caller::User, vm.id, 1, n));
    assert_eq!(detail(&e, "field").as_deref(), Some("firmware"));
    // No x86 Windows becomes Windows 11 ARM64 either.
    let c10 = windows(&mut h, "w10", GuestOs::Windows10, Firmware::Uefi);
    let w10 = h.core.create_vm(&Caller::User, c10.clone()).unwrap();
    let mut n = c10;
    n.guest_os = GuestOs::Windows11Arm64;
    invalid(h.core.update_vm_config(&Caller::User, w10.id, 1, n));
    // Minimums: 4096 MiB and 32 GiB; BIOS refused at creation.
    let mut n = windows(&mut h, "w11b", GuestOs::Windows11Arm64, Firmware::Uefi);
    n.memory_mib = 4095;
    invalid(h.core.create_vm(&Caller::User, n));
    let mut n = windows(&mut h, "w11b", GuestOs::Windows11Arm64, Firmware::Uefi);
    n.disk_gib = 31;
    invalid(h.core.create_vm(&Caller::User, n));
    let n = windows(&mut h, "w11b", GuestOs::Windows11Arm64, Firmware::Bios);
    invalid(h.core.create_vm(&Caller::User, n));
}

/// An x86 host (the rule of every build except Apple Silicon macOS).
fn x86_host(os: GuestOs, _: Option<GuestArch>) -> bool {
    !os.is_aarch64()
}

/// An Apple Silicon host.
fn arm_host(os: GuestOs, _: Option<GuestArch>) -> bool {
    os.is_aarch64()
}

fn unsupported(r: Result<VmRecord, PublicError>) -> PublicError {
    let e = r.expect_err("refused");
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "field").as_deref(), Some("guest_os"));
    e
}

#[test]
fn guests_the_host_cannot_run_are_refused() {
    let mut h = Harness::new();
    // Created while every guest was accepted (a catalog from another host).
    let c11 = windows(&mut h, "w11", GuestOs::Windows11Arm64, Firmware::Uefi);
    let w11 = h.core.create_vm(&Caller::User, c11.clone()).unwrap();
    let c10 = windows(&mut h, "w10", GuestOs::Windows10, Firmware::Uefi);
    let w10 = h.core.create_vm(&Caller::User, c10.clone()).unwrap();

    // x86 host: Windows 11 ARM64 cannot be created, modified or started.
    h.core.set_guest_support(x86_host);
    let n = windows(&mut h, "w11-new", GuestOs::Windows11Arm64, Firmware::Uefi);
    let e = unsupported(h.core.create_vm(&Caller::User, n));
    assert!(e.message.contains("Windows 11 ARM64"), "{}", e.message);
    let mut n = c11.clone();
    n.memory_mib = 8192;
    unsupported(h.core.update_vm_config(&Caller::User, w11.id, 1, n));
    let r = h.req(w11.id, None, VmAction::Start);
    let e = h.core.submit(&Caller::User, r).expect_err("start refused");
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    // x86 guests still work.
    let n = windows(&mut h, "w10-new", GuestOs::Windows10, Firmware::Uefi);
    h.core
        .create_vm(&Caller::User, n)
        .expect("x86 guest on x86 host");
    h.start(w10.id);
    h.stop(w10.id);

    // Apple Silicon host: the opposite.
    h.core.set_guest_support(arm_host);
    for os in [GuestOs::UbuntuLtsX86_64, GuestOs::Windows10] {
        let n = windows(&mut h, "x86-new", os, os.allowed_firmware()[0]);
        unsupported(h.core.create_vm(&Caller::User, n));
    }
    let r = h.req(w10.id, None, VmAction::Start);
    let e = h.core.submit(&Caller::User, r).expect_err("start refused");
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    let n = windows(&mut h, "w11-arm", GuestOs::Windows11Arm64, Firmware::Uefi);
    h.core
        .create_vm(&Caller::User, n)
        .expect("ARM64 guest on Apple Silicon");
    h.start(w11.id);
    // Refusals are audited.
    assert!(
        h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::VmCreate
                && e.outcome == AuditOutcome::Denied
                && e.error == Some(ErrorCode::Unsupported))
    );
}

#[test]
fn runs_on_this_host_rule() {
    let mut h = Harness::new();
    h.core.set_guest_support(GuestOs::runs_on_this_host_with);
    for os in GuestOs::ALL {
        let c = windows(&mut h, "vm", os, os.allowed_firmware()[0]);
        let r = h.core.create_vm(&Caller::User, c);
        if os.runs_on_this_host() {
            r.expect("native guest");
        } else {
            unsupported(r);
        }
    }
}

/// A Windows host (WHPX): x86 guests, but not Windows Vista x64 (ADR-0027).
fn windows_host(os: GuestOs, arch: Option<GuestArch>) -> bool {
    !os.is_aarch64() && !(os == GuestOs::WindowsVista && arch == Some(GuestArch::X86_64))
}

/// Records in the registry that `m` was detected as `os` of `arch`.
fn detect(h: &Harness, m: MediaId, os: GuestOs, arch: Option<GuestArch>) {
    let mut store = h.store.0.borrow_mut();
    let entry = store.media.get_mut(&m).expect("registered media");
    entry.record.detected_guest = Some(DetectedGuest {
        os,
        arch,
        build: Some(6003),
        edition: None,
    });
}

/// Windows Vista configuration with installation media detected as `arch`.
fn vista(h: &mut Harness, name: &str, arch: Option<GuestArch>) -> VmConfig {
    let c = windows(h, name, GuestOs::WindowsVista, Firmware::Bios);
    let m = c.install_media.expect("install media");
    detect(h, m, GuestOs::WindowsVista, arch);
    c
}

fn unsupported_media(r: Result<VmRecord, PublicError>) -> PublicError {
    let e = r.expect_err("refused");
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "field").as_deref(), Some("install_media"));
    assert_eq!(detail(&e, "arch").as_deref(), Some("x86_64"));
    assert!(
        e.message.contains("Windows Vista 64-bit") && e.message.contains("32-bit"),
        "{}",
        e.message
    );
    e
}

#[test]
fn vista_x64_media_is_refused_where_the_host_cannot_run_it() {
    let mut h = Harness::new();
    // Created while every guest was accepted (a catalog from another host).
    let old = vista(&mut h, "vista-old", Some(GuestArch::X86_64));
    let old = h.core.create_vm(&Caller::User, old).unwrap();

    h.core.set_guest_support(windows_host);
    // Creation with Vista x64 media is refused.
    let c = vista(&mut h, "vista-x64", Some(GuestArch::X86_64));
    unsupported_media(h.core.create_vm(&Caller::User, c));
    // Vista 32-bit, media of unknown architecture and no media are not.
    let c32 = vista(&mut h, "vista-x86", Some(GuestArch::X86));
    let v32 = h
        .core
        .create_vm(&Caller::User, c32.clone())
        .expect("32-bit");
    let c = vista(&mut h, "vista-unknown", None);
    h.core.create_vm(&Caller::User, c).expect("unknown arch");
    let mut c = windows(
        &mut h,
        "vista-no-media",
        GuestOs::WindowsVista,
        Firmware::Bios,
    );
    c.install_media = None;
    h.core.create_vm(&Caller::User, c).expect("no media");
    // Other guests from 64-bit media are not affected.
    let c = windows(&mut h, "w7-x64", GuestOs::Windows7, Firmware::Bios);
    detect(
        &h,
        c.install_media.unwrap(),
        GuestOs::Windows7,
        Some(GuestArch::X86_64),
    );
    h.core.create_vm(&Caller::User, c).expect("Windows 7 x64");

    // Switching the 32-bit VM to Vista x64 media is refused.
    let mut n = c32;
    let x64 = vista(&mut h, "unused", Some(GuestArch::X86_64));
    n.install_media = x64.install_media;
    unsupported_media(h.core.update_vm_config(&Caller::User, v32.id, 1, n));

    // A Vista x64 VM from another host does not start.
    let r = h.req(old.id, None, VmAction::Start);
    let e = h.core.submit(&Caller::User, r).expect_err("start refused");
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "field").as_deref(), Some("install_media"));
    h.start(v32.id);
    assert!(
        h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::VmConfigure
                && e.outcome == AuditOutcome::Denied
                && e.error == Some(ErrorCode::Unsupported))
    );
}

#[test]
fn vista_x64_follows_the_rule_of_this_host() {
    let mut h = Harness::new();
    h.core.set_guest_support(GuestOs::runs_on_this_host_with);
    for (i, arch) in [None, Some(GuestArch::X86), Some(GuestArch::X86_64)]
        .into_iter()
        .enumerate()
    {
        let c = vista(&mut h, &format!("vista-{i}"), arch);
        let r = h.core.create_vm(&Caller::User, c);
        if GuestOs::WindowsVista.runs_on_this_host_with(arch) {
            r.expect("runs on this host");
        } else {
            let e = r.expect_err("refused on this host");
            assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
        }
    }
}
