//! Import of VMware VMs by conversion (ADR-0030), service side.
//!
//! The examination of the `.vmx` and of the disk chain ([`analyze`]) and
//! the conversion ([`convert`]) run on worker threads; the catalog and core
//! steps run on the supervisor thread ([`crate::supervisor`]):
//!
//! 1. supervisor: caller checked (user only), context taken
//!    ([`crate::engine::VmEngine::vmware_context`]);
//! 2. worker: [`analyze`]: `vmware_import::inspect` on the untrusted files,
//!    the files of the chain held read-only ([`vmware_import::open_guard`]),
//!    the examination repeated under the guard (nothing changed in between),
//!    `qemu-img info --backing-chain` cross-checked, the configuration
//!    planned within the host limits;
//! 3. supervisor: core checks of the configuration and storage budget, then
//!    the journal intent `import_vm`
//!    ([`crate::engine::VmEngine::vmware_begin`]);
//! 4. worker: [`convert`]: `qemu-img convert -p -f vmdk -O qcow2` into the
//!    journal temporary with the guard still held, progress, cancellation;
//!    then `qemu-img check` and the virtual size of the result;
//! 5. supervisor: the disk is published and the VM row written in one
//!    commit ([`crate::engine::VmEngine::vmware_finish`]), or everything is
//!    rolled back ([`crate::engine::VmEngine::vmware_abort`]).
//!
//! The VMware files are opened read-only, never written; messages name
//! files, never their folders.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightvm_proto::ipc::VmwareImportPreview;
use lightvm_proto::{DisplayAdapter, PublicError, VmConfig, VmName};
use vmware_import::inspect::Guard;
use vmware_import::{ChainFacts, HostLimits, Inspection, Plan};

use crate::engine::ImageTool;

/// What the workers need, taken on the supervisor thread.
#[derive(Clone)]
pub struct VmwareContext {
    /// `qemu-img` of the approved build.
    pub tool: Arc<dyn ImageTool>,
    /// Limits a new VM must fit.
    pub limits: HostLimits,
    /// Catalog root (free space).
    pub catalog_root: PathBuf,
}

impl std::fmt::Debug for VmwareContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmwareContext")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Outcome of [`analyze`]: the examined VM, its chain held read-only.
#[derive(Debug)]
pub struct Analysis {
    /// Examination of the VM folder.
    pub inspection: Inspection,
    /// Chain facts read by `qemu-img`.
    pub facts: ChainFacts,
    /// Planned configuration and lists.
    pub plan: Plan,
    /// Bytes needed on the catalog volume.
    pub required_bytes: u64,
    /// Free bytes on the catalog volume, if measured.
    pub free_bytes: Option<u64>,
    /// Files of the chain held read-only until the conversion ends.
    guard: Guard,
    /// `qemu-img` that examined the chain, used for the conversion.
    tool: Arc<dyn ImageTool>,
}

impl Analysis {
    /// Preview for the UI.
    #[must_use]
    pub fn preview(&self) -> VmwareImportPreview {
        vmware_import::plan::preview(
            &self.inspection,
            &self.facts,
            &self.plan,
            self.required_bytes,
            self.free_bytes,
        )
    }

    /// Configuration to create with the user's display choice (VMware
    /// SVGA only where offered).
    pub fn config(&self, display: DisplayAdapter) -> Result<VmConfig, PublicError> {
        let mut c = self.plan.config.clone();
        if display == DisplayAdapter::VmwareSvga && !self.inspection.guest.os.is_windows() {
            return Err(PublicError::invalid_argument(
                "the VMware SVGA display is offered to Windows guests only",
            )
            .with_detail("field", "display"));
        }
        if let Some(hw) = c.imported.as_mut() {
            hw.display = display;
        }
        Ok(c)
    }
}

fn cancelled() -> PublicError {
    PublicError::state_conflict("VMware import cancelled")
}

/// Path QEMU can receive (`qemu_backend::validate::qemu_path_value`): a
/// VMware folder whose name has a comma, an equals sign, a quote or a
/// colon must be renamed first.
fn qemu_path(p: &Path) -> Result<(), PublicError> {
    let text = p
        .to_str()
        .ok_or_else(|| PublicError::invalid_argument("disk path not valid text"))?;
    qemu_backend::validate::qemu_path_value("vmdk", text).map(|_| ()).map_err(|_| {
        PublicError::invalid_argument(
            "the location of the VMware disk contains a character QEMU cannot accept (for example a comma, an equals sign or a quote): rename the folder of the VM (with VMware closed) and try again",
        )
    })
}

/// Examines the VMware VM of `vmx` (see the module documentation). `name`
/// is the name of the new VM; `None` = suggested from the VMware display
/// name (preview).
pub fn analyze(
    ctx: &VmwareContext,
    vmx: &str,
    name: Option<VmName>,
    cancel: &dyn Fn() -> bool,
) -> Result<Analysis, PublicError> {
    let first = vmware_import::inspect(vmx).map_err(|e| e.to_public())?;
    qemu_path(&first.disk.top)?;
    let guard = vmware_import::open_guard(&first.disk).map_err(|e| e.to_public())?;
    // Under the guard nobody can write, rename or delete the chain: a second
    // examination must find exactly the same files.
    let inspection = vmware_import::inspect(vmx).map_err(|e| e.to_public())?;
    if inspection.disk != first.disk || inspection.vmx != first.vmx {
        return Err(PublicError::state_conflict(
            "the VMware VM changed while it was examined: close VMware and try again",
        ));
    }
    if cancel() {
        return Err(cancelled());
    }
    let json = ctx.tool.vmdk_chain_info(&inspection.disk.top, cancel)?;
    let facts =
        vmware_import::check_chain_info(&json, &inspection.disk).map_err(|e| e.to_public())?;
    let name = name.unwrap_or_else(|| {
        let stem = Path::new(&inspection.vmx_file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        vmware_import::plan::suggested_name(inspection.vmx.get("displayname"), &stem)
    });
    let plan =
        vmware_import::plan(&inspection, &facts, &ctx.limits, name).map_err(|e| e.to_public())?;
    let required_bytes = vmware_import::plan::required_bytes(&facts);
    let free_bytes = ctx.tool.free_bytes(&ctx.catalog_root);
    Ok(Analysis {
        inspection,
        facts,
        plan,
        required_bytes,
        free_bytes,
        guard,
        tool: ctx.tool.clone(),
    })
}

/// Storage budget of a conversion: the free space of the catalog volume
/// minus the host reserve (ADR-0017) must hold the estimate.
pub fn check_space(required: u64, free: Option<u64>, reserve: u64) -> Result<(), PublicError> {
    let Some(free) = free else {
        return Err(PublicError::infrastructure(
            "free space of the catalog volume not measurable",
        ));
    };
    let available = free.saturating_sub(reserve);
    if available < required {
        return Err(PublicError::resource_limit(format!(
            "not enough free space on the catalog volume: the converted disk needs up to {} MiB, {} MiB are available (host reserve excluded)",
            required >> 20,
            available >> 20
        ))
        .with_detail("required_bytes", required.to_string())
        .with_detail("available_bytes", available.to_string()));
    }
    Ok(())
}

/// Converts the chain of `a` into `dst` (a journal temporary that does not
/// exist yet), then checks the result. The guard of the analysis is held
/// until the end; `progress` receives the percentage.
pub fn convert(
    a: Analysis,
    dst: &Path,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u8),
) -> Result<(), PublicError> {
    if cancel() {
        return Err(cancelled());
    }
    let tool = a.tool.clone();
    tool.convert_vmdk(&a.inspection.disk.top, dst, cancel, progress)?;
    if cancel() {
        return Err(cancelled());
    }
    tool.check(dst, cancel)?;
    let size = tool.virtual_size(dst)?;
    if size != a.facts.virtual_size {
        return Err(PublicError::infrastructure(
            "the converted disk does not have the size of the VMware disk",
        ));
    }
    drop(a.guard);
    if let Some(hw) = &a.plan.config.imported {
        let outcome = prepare_storage_driver(dst, hw);
        if outcome == DriverPreparation::ManualPreparationNeeded {
            crate::diag!(
                "lightvm-service: VMware import: the guest needs its storage driver enabled for boot (manual preparation, ADR-0030)"
            );
        }
    }
    Ok(())
}

/// Outcome of [`prepare_storage_driver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverPreparation {
    /// The guest keeps a controller it has a boot driver for.
    NotNeeded,
    /// Windows was on a controller QEMU does not emulate (VMware
    /// `lsilogic`/`buslogic`): the driver of the new controller must be
    /// enabled for boot; the user prepares the guest in VMware
    /// (`docs/USER_GUIDE.md`, "LSI Logic disks").
    ManualPreparationNeeded,
}

/// Hook of the offline storage driver enablement (ADR-0030): it will edit
/// the guest `SYSTEM` hive of the **converted copy** (`converted`, a
/// journal temporary, never the VMware files) so that the inbox driver of
/// [`ImportedHardware::disk_controller`] starts at boot. The method (hive
/// editing on the image, the host privileges it needs, the Windows
/// versions it covers) is decided by a separate spike: until then nothing
/// is changed and the outcome says whether the manual preparation applies.
#[must_use]
pub fn prepare_storage_driver(
    converted: &Path,
    hw: &lightvm_proto::ImportedHardware,
) -> DriverPreparation {
    let _ = converted;
    if hw.storage_driver_preparation {
        DriverPreparation::ManualPreparationNeeded
    } else {
        DriverPreparation::NotNeeded
    }
}
