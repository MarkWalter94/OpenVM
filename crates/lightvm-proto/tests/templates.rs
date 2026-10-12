//! Standard resource templates per guest (ADR-0027).

use lightvm_proto::{GuestArch, GuestOs};

const ARCHES: [Option<GuestArch>; 4] = [
    None,
    Some(GuestArch::X86),
    Some(GuestArch::X86_64),
    Some(GuestArch::Aarch64),
];

#[test]
fn templates_meet_minimums_and_caps() {
    for os in GuestOs::ALL {
        for arch in ARCHES {
            let t = os.template(arch);
            assert!(t.vcpus >= 1, "{os:?} {arch:?}");
            assert!(t.memory_mib >= os.min_memory_mib(), "{os:?} {arch:?}");
            assert!(t.disk_gib >= os.min_disk_gib(), "{os:?} {arch:?}");
            if let Some(max) = os.max_memory_mib(arch) {
                assert!(t.memory_mib <= max, "{os:?} {arch:?}");
            }
        }
    }
}

#[test]
fn unknown_arch_defaults() {
    assert_eq!(
        GuestOs::WindowsXp.template(None),
        GuestOs::WindowsXp.template(Some(GuestArch::X86))
    );
    assert_eq!(
        GuestOs::Windows10.template(None),
        GuestOs::Windows10.template(Some(GuestArch::X86_64))
    );
    let t = GuestOs::Windows7.template(None);
    assert_eq!(
        GuestOs::Windows7.suggested_resources(),
        (t.vcpus, t.memory_mib, t.disk_gib)
    );
}

#[test]
fn x86_windows_memory_cap() {
    assert_eq!(
        GuestOs::Windows10.max_memory_mib(Some(GuestArch::X86)),
        Some(4096)
    );
    assert_eq!(
        GuestOs::Windows10.max_memory_mib(Some(GuestArch::X86_64)),
        None
    );
    assert_eq!(
        GuestOs::UbuntuLtsX86_64.max_memory_mib(Some(GuestArch::X86)),
        None
    );
}

#[test]
fn windows_11_arm64_template_and_profile() {
    let os = GuestOs::Windows11Arm64;
    let t = os.template(Some(GuestArch::Aarch64));
    assert_eq!((t.vcpus, t.memory_mib, t.disk_gib), (4, 6144, 64));
    assert_eq!(os.template(None), t);
    assert_eq!(os.suggested_resources(), (4, 6144, 64));
    assert_eq!((os.min_memory_mib(), os.min_disk_gib()), (4096, 64));
    assert_eq!(os.max_memory_mib(Some(GuestArch::Aarch64)), None);
    assert!(os.is_windows() && os.is_aarch64() && os.needs_tpm());
    for other in GuestOs::ALL.into_iter().filter(|o| *o != os) {
        assert!(!other.is_aarch64() && !other.needs_tpm(), "{other:?}");
        // Exactly one architecture family runs on any host.
        assert_ne!(other.runs_on_this_host(), os.runs_on_this_host());
    }
    assert_eq!(
        os.runs_on_this_host(),
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    );
}
