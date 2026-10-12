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
