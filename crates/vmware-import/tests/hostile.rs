//! Hostile `.vmx` files and VMDK descriptors (ADR-0030, required evidence):
//! traversal, absolute paths outside the folder, UNC and device paths,
//! links and junctions, oversized files, encrypted, suspended, locked and
//! not cleanly shut down VMs, unknown guest types, looping and overlong
//! chains, inconsistent snapshots. The VM folders are written by the tests;
//! no `qemu-img` is needed.

mod common;

use common::{VmDir, flat_descriptor, junction, temp_dir, write_flat_disk};
use vmware_import::{ImportError, inspect};

const MIB_SECTORS: u64 = 2048;

fn ok_vm(prefix: &str) -> VmDir {
    let vm = VmDir::new(prefix);
    write_flat_disk(&vm.folder, "disk.vmdk", 64 * MIB_SECTORS);
    vm
}

fn err(vm: &VmDir) -> ImportError {
    inspect(&vm.vmx_str()).expect_err("must be refused")
}

#[test]
fn baseline_flat_vm_is_accepted() {
    let vm = ok_vm("ok");
    vm.write_vmx(
        "winvista",
        "scsi0:0",
        "disk.vmdk",
        "scsi0.virtualDev = \"pvscsi\"\nethernet0.present = \"TRUE\"\nsound.present = \"TRUE\"\n",
    );
    let i = inspect(&vm.vmx_str()).unwrap();
    assert_eq!(i.vmx_file, "Test VM.vmx");
    assert_eq!(i.disk.chain.len(), 1);
    assert_eq!(i.disk.files.len(), 2, "descriptor and flat extent");
    assert_eq!(
        i.disk.controller,
        lightvm_proto::ipc::VmwareController::Pvscsi
    );
    assert_eq!(i.guest.os, lightvm_proto::GuestOs::WindowsVista);
    // The guard opens every file and keeps them open.
    let g = vmware_import::open_guard(&i.disk).unwrap();
    drop(g);
}

#[test]
fn disk_paths_outside_the_folder_are_refused() {
    let elsewhere = temp_dir("outside");
    write_flat_disk(&elsewhere, "other.vmdk", MIB_SECTORS);
    let outside_abs = elsewhere.join("other.vmdk").to_string_lossy().into_owned();
    let cases: Vec<(String, &str)> = vec![
        (r"..\outside.vmdk".into(), "traversal"),
        ("../outside.vmdk".into(), "traversal with slashes"),
        (r"sub\..\disk.vmdk".into(), "traversal that stays inside"),
        (outside_abs.clone(), "absolute outside"),
        (r"\\server\share\disk.vmdk".into(), "UNC"),
        (r"\\?\C:\disk.vmdk".into(), "device prefix"),
        (r"\\.\PhysicalDrive0".into(), "raw device"),
        (
            r"\\.\GLOBALROOT\Device\Harddisk0\x.vmdk".into(),
            "device namespace",
        ),
        ("NUL.vmdk".into(), "device name"),
        (r"C:disk.vmdk".into(), "drive relative"),
        ("disk.vmdk:stream".into(), "alternate data stream"),
    ];
    for (name, what) in cases {
        let vm = ok_vm("trav");
        vm.write_vmx("windows7-64", "sata0:0", &name, "");
        let e = err(&vm);
        if cfg!(windows) || !name.contains('\\') {
            assert!(
                matches!(
                    e,
                    ImportError::OutsideFolder(_) | ImportError::NotFound(_) | ImportError::NoDisk
                ),
                "{what}: {e:?}"
            );
        }
        assert!(
            !e.to_public()
                .message
                .contains(&*elsewhere.to_string_lossy()),
            "no paths in messages"
        );
    }
    let _ = common::remove_tree(&elsewhere);
}

#[test]
fn absolute_path_inside_the_folder_is_accepted() {
    let vm = ok_vm("absin");
    let abs = vm.path("disk.vmdk").to_string_lossy().into_owned();
    vm.write_vmx("windows7-64", "sata0:0", &abs, "");
    inspect(&vm.vmx_str()).expect("absolute inside the folder");
}

#[test]
fn extent_and_parent_paths_follow_the_same_rules() {
    let elsewhere = temp_dir("ext");
    let f = std::fs::File::create(elsewhere.join("x-flat.vmdk")).unwrap();
    f.set_len(MIB_SECTORS * 512).unwrap();
    let abs = elsewhere.join("x-flat.vmdk").to_string_lossy().into_owned();
    for (extent, what) in [
        (r"..\x-flat.vmdk".to_owned(), "extent traversal"),
        (abs.clone(), "extent absolute outside"),
        (r"\\server\share\x-flat.vmdk".to_owned(), "extent UNC"),
        (r"\\.\PhysicalDrive0".to_owned(), "extent device"),
    ] {
        let vm = VmDir::new("ext");
        std::fs::write(vm.path("disk.vmdk"), flat_descriptor(&extent, MIB_SECTORS)).unwrap();
        vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
        let e = err(&vm);
        if cfg!(windows) || !extent.contains('\\') {
            assert!(matches!(e, ImportError::OutsideFolder(_)), "{what}: {e:?}");
        }
    }
    // Parent hint outside the folder.
    let vm = VmDir::new("parent");
    write_flat_disk(&vm.folder, "base.vmdk", MIB_SECTORS);
    let child = format!(
        "# Disk DescriptorFile\nversion=1\nCID=11111111\nparentCID=fd32bc87\ncreateType=\"monolithicFlat\"\n\
         parentFileNameHint=\"{}\"\nRW {MIB_SECTORS} FLAT \"child-flat.vmdk\" 0\n",
        abs.replace("x-flat.vmdk", "base.vmdk")
    );
    std::fs::write(vm.path("child.vmdk"), child).unwrap();
    std::fs::File::create(vm.path("child-flat.vmdk"))
        .unwrap()
        .set_len(MIB_SECTORS * 512)
        .unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "child.vmdk", "");
    assert!(matches!(err(&vm), ImportError::OutsideFolder(_)));
    let _ = common::remove_tree(&elsewhere);
}

#[test]
fn links_and_junctions_are_refused() {
    // Junction inside the VM folder towards another folder.
    let elsewhere = temp_dir("jt");
    write_flat_disk(&elsewhere, "j.vmdk", MIB_SECTORS);
    let vm = VmDir::new("junction");
    if !junction(&vm.path("sub"), &elsewhere) {
        eprintln!("junction not available: case skipped");
        return;
    }
    vm.write_vmx("windows7-64", "sata0:0", r"sub\j.vmdk", "");
    assert!(
        matches!(err(&vm), ImportError::Link(_)),
        "junction on the way"
    );
    // The .vmx itself reached through a junction: not where the user saw it.
    let vm2 = ok_vm("vmxjunction");
    vm2.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    let link = vm2.root.join("via");
    assert!(junction(&link, &vm2.folder));
    let through = link.join("Test VM.vmx").to_string_lossy().into_owned();
    assert!(
        matches!(inspect(&through), Err(ImportError::NotLocal)),
        "vmx via junction"
    );
    // Symbolic link to a file (needs the privilege or developer mode).
    #[cfg(windows)]
    {
        let vm3 = ok_vm("symlink");
        if std::os::windows::fs::symlink_file(elsewhere.join("j.vmdk"), vm3.path("l.vmdk")).is_ok()
        {
            vm3.write_vmx("windows7-64", "sata0:0", "l.vmdk", "");
            assert!(matches!(err(&vm3), ImportError::Link(_)));
        } else {
            eprintln!("file symlinks not available: case skipped");
        }
    }
    let _ = common::remove_tree(&elsewhere);
}

#[test]
fn oversized_files_are_refused() {
    let vm = ok_vm("big");
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    let mut s = std::fs::read_to_string(&vm.vmx).unwrap();
    s.push_str(&"# padding\n".repeat(120_000));
    std::fs::write(&vm.vmx, s).unwrap();
    assert!(matches!(err(&vm), ImportError::TooLarge(_)));
    // A large text file named as the disk: not a descriptor.
    let vm = VmDir::new("bigdesc");
    let mut d = flat_descriptor("x-flat.vmdk", MIB_SECTORS);
    d.push_str(&"#".repeat(70 << 10));
    std::fs::write(vm.path("disk.vmdk"), d).unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    assert!(matches!(err(&vm), ImportError::InvalidDescriptor(..)));
    // A sparse header whose embedded descriptor is too large.
    let vm = VmDir::new("bigembedded");
    let mut h = vec![0u8; 512 * 4];
    h[..4].copy_from_slice(b"KDMV");
    h[28..36].copy_from_slice(&1u64.to_le_bytes());
    h[36..44].copy_from_slice(&100_000u64.to_le_bytes());
    std::fs::write(vm.path("disk.vmdk"), h).unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    assert!(matches!(err(&vm), ImportError::TooLarge(_)));
    // Descriptor offset past the end of the file.
    let vm = VmDir::new("pastend");
    let mut h = vec![0u8; 512];
    h[..4].copy_from_slice(b"KDMV");
    h[28..36].copy_from_slice(&1000u64.to_le_bytes());
    h[36..44].copy_from_slice(&1u64.to_le_bytes());
    std::fs::write(vm.path("disk.vmdk"), h).unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    assert!(matches!(err(&vm), ImportError::InvalidDescriptor(..)));
}

#[test]
fn vm_state_refusals() {
    type Case = (&'static str, fn(&VmDir), fn(&ImportError) -> bool);
    let cases: [Case; 7] = [
        (
            "encrypted",
            |vm| {
                let mut s = std::fs::read_to_string(&vm.vmx).unwrap();
                s.push_str("encryption.keySafe = \"vmware:key/list/(pair/(phrase/x))\"\nencryption.data = \"abc\"\n");
                std::fs::write(&vm.vmx, s).unwrap();
            },
            |e| matches!(e, ImportError::Encrypted),
        ),
        (
            "suspended vmState",
            |vm| {
                let mut s = std::fs::read_to_string(&vm.vmx).unwrap();
                s.push_str("checkpoint.vmState = \"Test VM-Snapshot1.vmss\"\n");
                std::fs::write(&vm.vmx, s).unwrap();
            },
            |e| matches!(e, ImportError::Suspended),
        ),
        (
            "suspended vmss file",
            |vm| std::fs::write(vm.path("Test VM-1234.vmss"), b"x").unwrap(),
            |e| matches!(e, ImportError::Suspended),
        ),
        (
            "locked",
            |vm| std::fs::create_dir(vm.path("Test VM.vmx.lck")).unwrap(),
            |e| matches!(e, ImportError::Locked),
        ),
        (
            "disk locked",
            |vm| std::fs::create_dir(vm.path("disk.vmdk.lck")).unwrap(),
            |e| matches!(e, ImportError::Locked),
        ),
        (
            "unclean shutdown",
            |vm| {
                let s = std::fs::read_to_string(&vm.vmx)
                    .unwrap()
                    .replace("cleanShutdown = \"TRUE\"", "cleanShutdown = \"FALSE\"");
                std::fs::write(&vm.vmx, s).unwrap();
            },
            |e| matches!(e, ImportError::NotCleanShutdown),
        ),
        (
            "unknown guest",
            |vm| {
                let s = std::fs::read_to_string(&vm.vmx)
                    .unwrap()
                    .replace("guestOS = \"windows7-64\"", "guestOS = \"darwin19-64\"");
                std::fs::write(&vm.vmx, s).unwrap();
            },
            |e| matches!(e, ImportError::UnknownGuest(_)),
        ),
    ];
    for (what, setup, check) in cases {
        let vm = ok_vm("state");
        vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
        setup(&vm);
        let e = err(&vm);
        assert!(check(&e), "{what}: {e:?}");
        let p = e.to_public();
        assert!(!p.message.is_empty(), "{what}");
    }
}

#[test]
fn chain_loops_depth_and_consistency() {
    let delta = |cid: &str, parent_cid: &str, parent: &str, own: &str| {
        format!(
            "# Disk DescriptorFile\nversion=1\nCID={cid}\nparentCID={parent_cid}\n\
             createType=\"monolithicFlat\"\nparentFileNameHint=\"{parent}\"\n\
             RW {MIB_SECTORS} FLAT \"{own}\" 0\n"
        )
    };
    let extent = |vm: &VmDir, name: &str| {
        std::fs::File::create(vm.path(name))
            .unwrap()
            .set_len(MIB_SECTORS * 512)
            .unwrap();
    };
    // a → b → a.
    let vm = VmDir::new("loop");
    std::fs::write(
        vm.path("a.vmdk"),
        delta("0000000a", "0000000b", "b.vmdk", "a-f.vmdk"),
    )
    .unwrap();
    std::fs::write(
        vm.path("b.vmdk"),
        delta("0000000b", "0000000a", "a.vmdk", "b-f.vmdk"),
    )
    .unwrap();
    extent(&vm, "a-f.vmdk");
    extent(&vm, "b-f.vmdk");
    vm.write_vmx("windows7-64", "sata0:0", "a.vmdk", "");
    assert!(matches!(err(&vm), ImportError::ChainTooDeep));
    // A chain one longer than the limit.
    let vm = VmDir::new("deep");
    let n = vmware_import::MAX_CHAIN_DEPTH + 1;
    for i in 0..n {
        let own = format!("d{i}-f.vmdk");
        extent(&vm, &own);
        let text = if i + 1 == n {
            format!(
                "# Disk DescriptorFile\nversion=1\nCID={:08x}\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW {MIB_SECTORS} FLAT \"{own}\" 0\n",
                i + 1
            )
        } else {
            delta(
                &format!("{:08x}", i + 1),
                &format!("{:08x}", i + 2),
                &format!("d{}.vmdk", i + 1),
                &own,
            )
        };
        std::fs::write(vm.path(&format!("d{i}.vmdk")), text).unwrap();
    }
    vm.write_vmx("windows7-64", "sata0:0", "d0.vmdk", "");
    assert!(matches!(err(&vm), ImportError::ChainTooDeep));
    // Parent CID that does not match the parent: inconsistent snapshot.
    let vm = VmDir::new("cid");
    write_flat_disk(&vm.folder, "base.vmdk", MIB_SECTORS);
    std::fs::write(
        vm.path("child.vmdk"),
        delta("00000002", "12345678", "base.vmdk", "c-f.vmdk"),
    )
    .unwrap();
    extent(&vm, "c-f.vmdk");
    vm.write_vmx("windows7-64", "sata0:0", "child.vmdk", "");
    assert!(matches!(err(&vm), ImportError::InvalidDescriptor(..)));
    // An extent that is a disk of the chain.
    let vm = VmDir::new("selfref");
    write_flat_disk(&vm.folder, "base.vmdk", MIB_SECTORS);
    std::fs::write(
        vm.path("child.vmdk"),
        delta("00000002", "fd32bc87", "base.vmdk", "child.vmdk"),
    )
    .unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "child.vmdk", "");
    assert!(matches!(err(&vm), ImportError::InvalidDescriptor(..)));
}

#[test]
fn raw_device_and_unknown_formats_are_refused() {
    for (desc, what) in [
        (
            "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"fullDevice\"\nRW 2048 FLAT \"\\\\.\\PhysicalDrive0\" 0\n",
            "full device",
        ),
        (
            "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"vmfsRawDeviceMap\"\nRW 2048 VMFSRDM \"x-rdm.vmdk\"\n",
            "rdm",
        ),
    ] {
        let vm = VmDir::new("raw");
        std::fs::write(vm.path("disk.vmdk"), desc).unwrap();
        vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
        let e = err(&vm);
        assert!(
            matches!(
                e,
                ImportError::UnsupportedDisk(..) | ImportError::OutsideFolder(_)
            ),
            "{what}: {e:?}"
        );
    }
    let vm = VmDir::new("cowd");
    let mut h = vec![0u8; 512];
    h[..4].copy_from_slice(b"COWD");
    std::fs::write(vm.path("disk.vmdk"), h).unwrap();
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    assert!(matches!(err(&vm), ImportError::UnsupportedDisk(..)));
}

#[test]
fn vmx_path_refusals() {
    assert!(matches!(
        inspect("relative\\x.vmx"),
        Err(ImportError::InvalidPath(_))
    ));
    assert!(matches!(
        inspect(r"\\server\share\x.vmx"),
        Err(ImportError::InvalidPath(_))
    ));
    let vm = ok_vm("ext");
    vm.write_vmx("windows7-64", "sata0:0", "disk.vmdk", "");
    let other = vm.path("disk.vmdk").to_string_lossy().into_owned();
    assert!(matches!(inspect(&other), Err(ImportError::InvalidPath(_))));
    let missing = vm.path("missing.vmx").to_string_lossy().into_owned();
    assert!(matches!(inspect(&missing), Err(ImportError::NotFound(_))));
    // A directory named like a .vmx.
    std::fs::create_dir(vm.path("dir.vmx")).unwrap();
    let dir = vm.path("dir.vmx").to_string_lossy().into_owned();
    assert!(matches!(inspect(&dir), Err(ImportError::NotAFile(_))));
}

#[test]
fn no_disk_and_device_selection() {
    let vm = ok_vm("nodisk");
    vm.write_vmx("windows7-64", "sata0:0", "disk.iso", "");
    assert!(matches!(err(&vm), ImportError::NoDisk));
    // Two disks: bios.hddOrder decides, the other is listed.
    let vm = ok_vm("order");
    write_flat_disk(&vm.folder, "second.vmdk", MIB_SECTORS);
    vm.write_vmx(
        "windows7-64",
        "sata0:0",
        "disk.vmdk",
        "scsi0.present = \"TRUE\"\nscsi0.virtualDev = \"lsisas1068\"\nscsi0:1.present = \"TRUE\"\n\
         scsi0:1.fileName = \"second.vmdk\"\nbios.hddOrder = \"scsi0:1\"\n\
         ide1:0.present = \"TRUE\"\nide1:0.deviceType = \"cdrom-image\"\nide1:0.fileName = \"C:\\x\\setup.iso\"\n",
    );
    let i = inspect(&vm.vmx_str()).unwrap();
    assert_eq!(i.disk.device.slot(), "scsi0:1");
    assert_eq!(i.other_disks, vec!["sata0:0 disk.vmdk".to_owned()]);
    assert_eq!(i.cdroms, vec!["ide1:0 setup.iso".to_owned()]);
}
