//! Every VMDK create type `qemu-img` produces, and a snapshot chain, built
//! at test time with the `qemu-img` of the approved QEMU build
//! (`LIGHTVM_QEMU_DIR`; skipped with a message when unset): the examination
//! accepts them, the files it validates are exactly the ones `qemu-img info`
//! opens, and the plan maps them (ADR-0030, required evidence). No real
//! VMware VM is used.

mod common;

use common::{VmDir, qemu_img, run_qemu_img};
use lightvm_proto::{DiskController, Firmware, GuestOs, VmName};
use vmware_import::{HostLimits, check_chain_info, inspect, plan};

const LIMITS: HostLimits = HostLimits {
    max_vcpus: 8,
    max_memory_mib: 16 * 1024,
    max_disk_gib: 256,
};

fn info(q: &std::path::Path, top: &std::path::Path) -> String {
    run_qemu_img(
        q,
        &[
            "info",
            "--backing-chain",
            "--output=json",
            "-f",
            "vmdk",
            &top.to_string_lossy(),
        ],
    )
}

#[test]
fn every_create_type_of_qemu_img() {
    let Some(q) = qemu_img() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: qemu-img fixtures skipped");
        return;
    };
    // 2.5 GiB for the split types (two extents), 64 MiB otherwise; the
    // files are sparse or unallocated.
    for (sub, size, files) in [
        ("monolithicSparse", "64M", 1),
        ("monolithicFlat", "64M", 2),
        ("twoGbMaxExtentSparse", "2560M", 3),
        ("twoGbMaxExtentFlat", "2560M", 3),
        ("streamOptimized", "64M", 1),
    ] {
        let vm = VmDir::new("fx");
        let top = vm.path("Test VM.vmdk");
        run_qemu_img(
            &q,
            &[
                "create",
                "-q",
                "-f",
                "vmdk",
                "-o",
                &format!("subformat={sub}"),
                &top.to_string_lossy(),
                size,
            ],
        );
        vm.write_vmx(
            "windows9-64",
            "nvme0:0",
            "Test VM.vmdk",
            "firmware = \"efi\"\ncpuid.coresPerSocket = \"2\"\nnumvcpus = \"4\"\n",
        );
        let i = inspect(&vm.vmx_str()).unwrap_or_else(|e| panic!("{sub}: {e:?}"));
        assert_eq!(i.disk.files.len(), files, "{sub}: {:?}", i.disk.files);
        assert_eq!(i.disk.create_type(), sub);
        let facts = check_chain_info(&info(&q, &i.disk.top), &i.disk)
            .unwrap_or_else(|e| panic!("{sub}: {e:?}"));
        let expect = if size == "64M" { 64 << 20 } else { 2560 << 20 };
        assert_eq!(facts.virtual_size, expect, "{sub}");
        assert_eq!(facts.depth, 1);
        let p = plan(&i, &facts, &LIMITS, VmName::parse("x").unwrap()).unwrap();
        assert_eq!(p.config.guest_os, GuestOs::Windows10);
        assert_eq!(p.config.firmware, Firmware::Uefi);
        let hw = p.config.imported.unwrap();
        assert_eq!(hw.disk_controller, DiskController::Nvme);
        assert_eq!(hw.cores_per_socket, Some(2));
        assert_eq!(p.config.vcpus, 4);
        assert_eq!(p.config.disk_gib, if size == "64M" { 1 } else { 3 });
        // The guard holds every file; qemu-img can still read them.
        let g = vmware_import::open_guard(&i.disk).unwrap();
        info(&q, &i.disk.top);
        drop(g);
    }
}

#[test]
fn snapshot_chain_is_followed_and_cross_checked() {
    let Some(q) = qemu_img() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: qemu-img fixtures skipped");
        return;
    };
    let vm = VmDir::new("chain");
    let base = vm.path("Test VM.vmdk");
    let s1 = vm.path("Test VM-000001.vmdk");
    let s2 = vm.path("Test VM-000002.vmdk");
    run_qemu_img(
        &q,
        &[
            "create",
            "-q",
            "-f",
            "vmdk",
            &base.to_string_lossy(),
            "128M",
        ],
    );
    // Children name their parent by file name, as VMware writes them.
    std::fs::create_dir_all(vm.path("x")).unwrap();
    for (child, parent) in [(&s1, "Test VM.vmdk"), (&s2, "Test VM-000001.vmdk")] {
        let out = std::process::Command::new(&q)
            .current_dir(&vm.folder)
            .args(["create", "-q", "-f", "vmdk", "-b", parent, "-F", "vmdk"])
            .arg(child.file_name().unwrap())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    vm.write_vmx(
        "winvista",
        "scsi0:0",
        "Test VM-000002.vmdk",
        "scsi0.virtualDev = \"lsilogic\"\nethernet0.present = \"TRUE\"\nusb.present = \"TRUE\"\n\
         sharedFolder0.present = \"TRUE\"\nsound.present = \"TRUE\"\nmks.enable3d = \"TRUE\"\n\
         toolsInstallManager.lastInstallError = \"0\"\nmemsize = \"512\"\n",
    );
    let i = inspect(&vm.vmx_str()).unwrap();
    assert_eq!(i.disk.chain.len(), 3);
    assert_eq!(i.disk.snapshot_levels(), 2);
    let facts = check_chain_info(&info(&q, &i.disk.top), &i.disk).unwrap();
    assert_eq!(facts.depth, 3);
    assert_eq!(facts.virtual_size, 128 << 20);
    let p = plan(&i, &facts, &LIMITS, VmName::parse("Vista").unwrap()).unwrap();
    let hw = p.config.imported.unwrap();
    // LSI Logic on Windows: the profile controller and the preparation flag.
    assert_eq!(hw.disk_controller, DiskController::Ahci);
    assert!(hw.storage_driver_preparation);
    use lightvm_proto::ipc::{ImportWarningKind as W, NotImportedItem as N};
    let kinds: Vec<_> = p.warnings.iter().map(|w| w.kind).collect();
    assert!(kinds.contains(&W::StorageDriverPreparation), "{kinds:?}");
    assert!(kinds.contains(&W::MemoryRaised), "{kinds:?}");
    let items: Vec<_> = p.not_imported.iter().map(|n| n.item).collect();
    for want in [
        N::Network,
        N::Usb,
        N::SharedFolders,
        N::Sound,
        N::Accelerated3d,
        N::Snapshots,
        N::VmwareToolsFeatures,
    ] {
        assert!(items.contains(&want), "{want:?} in {items:?}");
    }
    assert_eq!(p.config.memory_mib, 1024, "raised to the Vista minimum");
    // A chain whose qemu-img output names a file that was not validated
    // (simulated) is refused.
    let json = info(&q, &i.disk.top).replace("Test VM.vmdk", "Other.vmdk");
    assert!(check_chain_info(&json, &i.disk).is_err());
    // Fewer images than descriptors: refused.
    let one = info(&q, &vm.path("Test VM.vmdk"));
    assert!(check_chain_info(&one, &i.disk).is_err());
}

#[test]
fn parent_changed_after_snapshot_is_refused() {
    let Some(q) = qemu_img() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: qemu-img fixtures skipped");
        return;
    };
    let vm = VmDir::new("cidx");
    let base = vm.path("b.vmdk");
    run_qemu_img(
        &q,
        &["create", "-q", "-f", "vmdk", &base.to_string_lossy(), "64M"],
    );
    let out = std::process::Command::new(&q)
        .current_dir(&vm.folder)
        .args([
            "create", "-q", "-f", "vmdk", "-b", "b.vmdk", "-F", "vmdk", "c.vmdk",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    // Recreate the parent: new CID, the child's parentCID no longer matches.
    std::fs::remove_file(&base).unwrap();
    run_qemu_img(
        &q,
        &["create", "-q", "-f", "vmdk", &base.to_string_lossy(), "64M"],
    );
    vm.write_vmx("windows7-64", "sata0:0", "c.vmdk", "");
    assert!(matches!(
        inspect(&vm.vmx_str()),
        Err(vmware_import::ImportError::InvalidDescriptor(..))
    ));
}
