//! Import of VMware VMs in the core (ADR-0030): user only, the creation
//! checks plus the imported hardware, names unique (also against imports in
//! progress), the hardware fixed after the import, and every outcome
//! audited as `vm_import`.

mod common;

use common::*;
use vm_core::ports::CatalogStore as _;
use vm_core::proto::*;

fn imported(h: &Harness, name: &str, os: GuestOs, fw: Firmware, arch: GuestArch) -> VmConfig {
    let mut c = h.vm_config(name, None);
    c.guest_os = os;
    c.firmware = fw;
    c.vcpus = 4;
    c.memory_mib = 2048;
    // Smaller than the guest's creation minimum: the disk is the source's.
    c.disk_gib = 8;
    c.imported = Some(ImportedHardware {
        source: ImportSource::Vmware,
        arch,
        disk_controller: DiskController::Pvscsi,
        display: DisplayAdapter::VmwareSvga,
        cores_per_socket: Some(2),
        storage_driver_preparation: false,
    });
    c
}

fn last_import(h: &Harness) -> AuditEvent {
    h.audit_events()
        .into_iter()
        .rev()
        .find(|e| e.action == AuditAction::VmImport)
        .expect("vm_import audited")
}

fn begin(h: &mut Harness, c: &VmConfig) -> Result<VmId, PublicError> {
    h.core
        .begin_import(&Caller::User, c, |_, _| Ok(()))
        .map(|(id, ())| id)
}

/// Completes an import as the service does: the storage writes the row.
fn finish(h: &mut Harness, id: VmId, c: &VmConfig) -> VmRecord {
    h.core
        .finish_import(&Caller::User, id, c.clone(), |store, rec| {
            store.insert_vm(rec)
        })
        .expect("finished")
}

#[test]
fn import_registers_the_vm_and_audits() {
    let mut h = Harness::new();
    let c = imported(&h, "Win7", GuestOs::Windows7, Firmware::Bios, GuestArch::X86_64);
    let id = begin(&mut h, &c).expect("checks pass");
    let rec = finish(&mut h, id, &c);
    assert_eq!(rec.id, id);
    assert_eq!(rec.config, c);
    let vms = h.core.list_vms(&Caller::User).unwrap();
    assert!(vms.iter().any(|v| v.vm_id == id));
    let ev = last_import(&h);
    assert_eq!(ev.outcome, AuditOutcome::Succeeded);
    assert_eq!(ev.vm_id, Some(id));
}

#[test]
fn agents_cannot_import() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let (_sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let c = imported(&h, "x", GuestOs::Windows7, Firmware::Bios, GuestArch::X86_64);
    let e = h
        .core
        .begin_import(&agent, &c, |_, _| Ok(()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let ev = last_import(&h);
    assert_eq!(ev.outcome, AuditOutcome::Denied);
    assert_eq!(ev.error, Some(ErrorCode::PermissionDenied));
}

#[test]
fn import_checks() {
    let mut h = Harness::new();
    h.create_vm("Taken");
    // Same name, any case.
    let c = imported(&h, "taken", GuestOs::Windows7, Firmware::Bios, GuestArch::X86_64);
    assert_eq!(begin(&mut h, &c).unwrap_err().code, ErrorCode::StateConflict);
    // Without the imported hardware.
    let mut c = imported(&h, "plain", GuestOs::Windows7, Firmware::Bios, GuestArch::X86_64);
    c.imported = None;
    assert!(begin(&mut h, &c).is_err());
    // Topology that does not divide the vCPUs.
    let mut c = imported(&h, "topo", GuestOs::Windows7, Firmware::Bios, GuestArch::X86_64);
    c.vcpus = 3;
    assert_eq!(begin(&mut h, &c).unwrap_err().code, ErrorCode::InvalidArgument);
    // VMware SVGA on Linux: refused.
    let mut c = imported(&h, "svga", GuestOs::OtherLinuxX86_64, Firmware::Bios, GuestArch::X86_64);
    assert!(begin(&mut h, &c).is_err());
    if let Some(hw) = c.imported.as_mut() {
        hw.display = DisplayAdapter::Standard;
    }
    // Linux with legacy BIOS is allowed for an imported VM only.
    begin(&mut h, &c).expect("imported Linux BIOS");
    let mut created = h.vm_config("linux-bios", None);
    created.firmware = Firmware::Bios;
    assert!(h.core.create_vm(&Caller::User, created).is_err());
    // Over the host limits.
    let mut c = imported(&h, "huge", GuestOs::Windows10, Firmware::Uefi, GuestArch::X86_64);
    c.disk_gib = 100_000;
    assert_eq!(begin(&mut h, &c).unwrap_err().code, ErrorCode::ResourceLimit);
    // EFI Windows 7 stays refused.
    let c = imported(&h, "w7efi", GuestOs::Windows7, Firmware::Uefi, GuestArch::X86_64);
    assert!(begin(&mut h, &c).is_err());
    // Nothing was registered by the refusals.
    assert_eq!(h.core.list_vms(&Caller::User).unwrap().len(), 1);
}

#[test]
fn name_is_reserved_while_importing_and_released_on_abort() {
    let mut h = Harness::new();
    let c = imported(&h, "Busy", GuestOs::Windows10, Firmware::Uefi, GuestArch::X86_64);
    let id = begin(&mut h, &c).unwrap();
    // Another import or a creation with the same name is refused meanwhile.
    let again = imported(&h, "busy", GuestOs::Windows10, Firmware::Uefi, GuestArch::X86_64);
    assert_eq!(begin(&mut h, &again).unwrap_err().code, ErrorCode::StateConflict);
    let created = h.vm_config("BUSY", None);
    assert_eq!(
        h.core.create_vm(&Caller::User, created.clone()).unwrap_err().code,
        ErrorCode::StateConflict
    );
    let mut rolled_back = false;
    let e = h.core.abort_import(
        &Caller::User,
        id,
        |_| rolled_back = true,
        PublicError::state_conflict("import cancelled"),
    );
    assert!(rolled_back);
    assert_eq!(e.code, ErrorCode::StateConflict);
    let ev = last_import(&h);
    assert_eq!((ev.outcome, ev.error), (AuditOutcome::Failed, Some(ErrorCode::StateConflict)));
    // Free again.
    h.core.create_vm(&Caller::User, created).expect("name released");
}

#[test]
fn storage_failures_are_audited_and_register_nothing() {
    let mut h = Harness::new();
    let c = imported(&h, "nospace", GuestOs::Windows10, Firmware::Uefi, GuestArch::X86_64);
    let e = h
        .core
        .begin_import(&Caller::User, &c, |_, _| {
            Err::<(), _>(PublicError::resource_limit("not enough space"))
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ResourceLimit);
    assert_eq!(last_import(&h).outcome, AuditOutcome::Failed);
    let id = begin(&mut h, &c).unwrap();
    let e = h
        .core
        .finish_import(&Caller::User, id, c.clone(), |_, _| {
            Err(PublicError::infrastructure("commit failed"))
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InfrastructureError);
    assert!(h.core.list_vms(&Caller::User).unwrap().is_empty());
    assert_eq!(last_import(&h).outcome, AuditOutcome::Failed);
}

#[test]
fn vista_x64_import_follows_the_host_rule() {
    let mut h = Harness::new();
    h.core.set_guest_support(GuestOs::runs_on_this_host_with);
    let x64 = imported(&h, "v64", GuestOs::WindowsVista, Firmware::Bios, GuestArch::X86_64);
    let r = begin(&mut h, &x64);
    if cfg!(windows) {
        let e = r.unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("cannot be imported"), "{}", e.message);
    } else {
        r.unwrap();
    }
    let x86 = imported(&h, "v32", GuestOs::WindowsVista, Firmware::Bios, GuestArch::X86);
    begin(&mut h, &x86).expect("Vista x86");
}

#[test]
fn imported_hardware_is_fixed_after_the_import() {
    let mut h = Harness::new();
    let c = imported(&h, "fixed", GuestOs::Windows10, Firmware::Uefi, GuestArch::X86_64);
    let id = begin(&mut h, &c).unwrap();
    let rec = finish(&mut h, id, &c);
    // Memory can change; the imported hardware cannot.
    let mut more = c.clone();
    more.memory_mib = 4096;
    let rec = h
        .core
        .update_vm_config(&Caller::User, id, rec.config_revision, more)
        .expect("memory change");
    let mut other = rec.config.clone();
    if let Some(hw) = other.imported.as_mut() {
        hw.disk_controller = DiskController::Ahci;
    }
    let e = h
        .core
        .update_vm_config(&Caller::User, id, rec.config_revision, other)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let mut dropped = rec.config.clone();
    dropped.imported = None;
    assert!(
        h.core
            .update_vm_config(&Caller::User, id, rec.config_revision, dropped)
            .is_err()
    );
    let mut odd = rec.config.clone();
    odd.vcpus = 3;
    assert!(
        h.core
            .update_vm_config(&Caller::User, id, rec.config_revision, odd)
            .is_err(),
        "topology must keep dividing the vCPUs"
    );
}

#[test]
fn refusals_before_the_configuration_are_audited() {
    let mut h = Harness::new();
    h.core
        .record_import_denied(&Caller::User, &PublicError::state_conflict("locked"));
    let ev = last_import(&h);
    assert_eq!((ev.outcome, ev.error), (AuditOutcome::Denied, Some(ErrorCode::StateConflict)));
}
