//! Copy one prepared Outbox file through the host's native drag protocol.
use std::path::PathBuf;

pub fn supported() -> bool {
    cfg!(windows)
}

pub fn start(_frame: &eframe::Frame, path: PathBuf) -> Result<(), String> {
    #[cfg(windows)]
    {
        native::start(&path).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Native file drag is unavailable on this host; use Receive…".into())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod native {
    use std::{os::windows::ffi::OsStrExt, path::Path};
    use windows::{
        Win32::{
            Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK},
            System::{
                Com::{IBindCtx, IDataObject},
                Ole::{
                    DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl,
                    OleInitialize, OleUninitialize,
                },
                SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
            },
            UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
        },
        core::{BOOL, HRESULT, PCWSTR, Result, implement},
    };

    #[implement(IDropSource)]
    struct Source;

    #[allow(non_snake_case)]
    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                DRAGDROP_S_CANCEL
            } else if keys & MK_LBUTTON == MODIFIERKEYS_FLAGS(0) {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }
        fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    struct Ole;
    impl Drop for Ole {
        fn drop(&mut self) {
            // SAFETY: constructed only after successful OleInitialize on this
            // same UI thread; balances that call after all COM objects drop.
            unsafe { OleUninitialize() };
        }
    }

    pub(super) fn start(path: &Path) -> Result<()> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: synchronous UI-thread call; COM wrappers retain their objects,
        // name is NUL-terminated and lives through parsing, and the effect is a
        // valid mutable output. Only the prepared cache file is offered, COPY
        // exclusively. No clipboard access or shell command execution occurs.
        unsafe {
            OleInitialize(None)?;
            let _ole = Ole;
            let item: IShellItem =
                SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&IBindCtx>)?;
            let data: IDataObject = item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)?;
            let source: IDropSource = Source.into();
            let mut effect = DROPEFFECT::default();
            DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect).ok()
        }
    }
}
