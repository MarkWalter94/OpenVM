//! Property tests of the parsers on arbitrary and mutated input: they
//! return a result, never panic, and what they accept respects their limits.

use proptest::prelude::*;
use vmware_import::vmdk::parse_descriptor;
use vmware_import::vmx::parse_vmx;

const DESC: &str = "# Disk DescriptorFile\nversion=1\nCID=53c96037\nparentCID=a2cda9cd\n\
    createType=\"monolithicSparse\"\nparentFileNameHint=\"base.vmdk\"\n\
    RW 6291456 SPARSE \"child.vmdk\"\nRDONLY 10 ZERO\nRW 20 FLAT \"x.vmdk\" 7\n";

const VMX: &str = "guestOS = \"winvista\"\nscsi0.present = \"TRUE\"\nscsi0.virtualDev = \"lsilogic\"\n\
    scsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"a|22b.vmdk\"\nmemsize = \"2048\"\n";

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn random_bytes(b in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = parse_vmx(&b);
        let _ = parse_descriptor(&b, "x");
    }

    #[test]
    fn mutated_descriptor(pos in 0usize..DESC.len(), byte in any::<u8>(), cut in 0usize..DESC.len()) {
        let mut b = DESC.as_bytes().to_vec();
        b[pos] = byte;
        if let Ok(d) = parse_descriptor(&b, "x") {
            prop_assert!(!d.extents.is_empty());
            prop_assert!(d.extents.len() <= vmware_import::MAX_EXTENTS);
        }
        let _ = parse_descriptor(&DESC.as_bytes()[..cut], "x");
    }

    #[test]
    fn mutated_vmx(pos in 0usize..VMX.len(), byte in any::<u8>()) {
        let mut b = VMX.as_bytes().to_vec();
        b[pos] = byte;
        if let Ok(v) = parse_vmx(&b) {
            for k in v.keys() {
                prop_assert!(vmware_import::vmx::is_known_key(k));
            }
            let _ = v.devices();
        }
    }
}
