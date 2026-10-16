//! Import of a VMware VM by conversion (ADR-0030), journaled.
//!
//! 1. [`Catalog::begin_vm_import`] records an `import_vm` intent for the VM
//!    row to create, with the final path `vms/<vm_id>/primary.qcow2`; the
//!    caller writes the converted disk into [`VmImport::temp_path`] (a
//!    journal temporary under `tmp/`), outside any transaction;
//! 2. [`Catalog::finish_vm_import`] publishes the disk without overwriting
//!    and inserts the VM row in the same transaction that commits the
//!    intent: the VM exists only with its complete disk;
//! 3. [`Catalog::abort_vm_import`] (cancellation, failed conversion)
//!    removes the temporary and the VM directory; nothing is left.
//!
//! A crash at any point before the commit is rolled back by reconciliation
//! when the catalog is opened (temporary, published disk without a row and
//! the empty VM directory removed, see `recover.rs`).

use std::path::Path;

use crate::catalog::{Catalog, Intent, IntentSpec};
use crate::enums::{IntentKind, Lifecycle, TargetKind};
use crate::error::{Result, StorageError};
use crate::ids::{OperationId, VmId};
use crate::layout::CatalogLayout;
use crate::records::NewVm;

/// Import in progress (journal intent `import_vm`).
#[derive(Debug, Clone)]
pub struct VmImport {
    intent: Intent,
    vm_id: VmId,
}

impl VmImport {
    /// VM being created.
    #[must_use]
    pub fn vm_id(&self) -> VmId {
        self.vm_id
    }

    /// Journal temporary the converted disk is written to (must not exist
    /// before the conversion).
    #[must_use]
    pub fn temp_path(&self) -> &Path {
        &self.intent.temp_path
    }

    /// Final path of the disk once published.
    #[must_use]
    pub fn final_path(&self) -> Option<&Path> {
        self.intent.final_path.as_deref()
    }

    /// Journal intent identifier.
    #[must_use]
    pub fn intent_id(&self) -> i64 {
        self.intent.id
    }
}

impl Catalog {
    /// First step of an import: intent `import_vm` for VM `vm` (which must
    /// not exist), final path `vms/<vm>/primary.qcow2` (which must not
    /// exist either).
    pub fn begin_vm_import(
        &mut self,
        vm: &VmId,
        operation: Option<OperationId>,
    ) -> Result<VmImport> {
        let intent = self.begin_intent(IntentSpec {
            kind: IntentKind::ImportVm,
            vm_id: Some(*vm),
            operation_id: operation,
            target_kind: TargetKind::Vm,
            target_id: Some(vm.to_string()),
            final_path: Some(CatalogLayout::primary_disk(vm)),
            read_only: false,
        })?;
        Ok(VmImport { intent, vm_id: *vm })
    }

    /// Last step: the converted disk (a regular file at
    /// [`VmImport::temp_path`]) is published as the VM's primary disk and
    /// the VM row is inserted with `lifecycle`, in the transaction that
    /// commits the intent. On failure the import is aborted (nothing left)
    /// and the error returned.
    pub fn finish_vm_import(
        &mut self,
        imp: VmImport,
        vm: &NewVm,
        lifecycle: Lifecycle,
    ) -> Result<()> {
        let published = self.mark_applied(&imp.intent);
        let committed = published.and_then(|()| {
            let id = imp.vm_id;
            self.commit(&imp.intent, |tx| {
                tx.insert_vm_with_id(&id, vm)?;
                tx.set_vm_lifecycle(&id, lifecycle)?;
                Ok(())
            })
        });
        if let Err(e) = committed {
            if let Err(a) = self.abort(&imp.intent) {
                return Err(StorageError::Constraint(format!(
                    "import not completed ({e}) and not rolled back ({a}): reconciliation will finish it"
                )));
            }
            return Err(e);
        }
        Ok(())
    }

    /// Cancels an import that will not complete: the temporary (partial
    /// disk) and the VM directory are removed, the intent is aborted.
    pub fn abort_vm_import(&mut self, imp: VmImport) -> Result<()> {
        self.abort(&imp.intent)
    }
}
