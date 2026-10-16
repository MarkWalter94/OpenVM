//! Cross-check of `qemu-img info --backing-chain --output=json -f vmdk`.
//!
//! `qemu-img` runs only after [`crate::inspect`] has validated every file of
//! the chain. Its output is then compared with what was validated: one
//! image per descriptor of the chain, in the same order, each of format
//! `vmdk`, with the same file names; every extent it reports is one of the
//! validated files; the same positive virtual size everywhere. The bytes
//! allocated on the host are summed from the files it reports.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::paths::same_path;
use crate::{DiskSource, ImportError};

/// Longest `qemu-img info` output read (one image is about 2 KiB; a
/// 1024-extent disk about 200 KiB).
pub const MAX_INFO_BYTES: usize = 1 << 20;

/// Facts of the chain read by `qemu-img`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainFacts {
    /// Virtual size of the disk.
    pub virtual_size: u64,
    /// Bytes allocated on the host by every file of the chain.
    pub allocated_bytes: u64,
    /// Images in the chain.
    pub depth: u32,
    /// An image of the chain is marked dirty.
    pub dirty: bool,
}

/// Checks the JSON output of `qemu-img info --backing-chain` against
/// `disk` and returns the chain facts.
pub fn check_chain_info(json: &str, disk: &DiskSource) -> Result<ChainFacts, ImportError> {
    let mismatch = ImportError::ChainMismatch;
    if json.len() > MAX_INFO_BYTES {
        return Err(mismatch("output too large"));
    }
    let v: Value = serde_json::from_str(json).map_err(|_| mismatch("output not readable"))?;
    let images = match &v {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) => std::slice::from_ref(&v),
        _ => return Err(mismatch("output not readable")),
    };
    if images.len() != disk.chain.len() {
        return Err(mismatch("number of images in the chain"));
    }
    let base_dir = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut virtual_size = None;
    let mut allocated: u64 = 0;
    let mut dirty = false;
    for (img, expected) in images.iter().zip(&disk.chain) {
        if img.get("format").and_then(Value::as_str) != Some("vmdk") {
            return Err(mismatch("format is not vmdk"));
        }
        let name = img
            .get("filename")
            .and_then(Value::as_str)
            .ok_or(mismatch("file name missing"))?;
        let dir = base_dir(expected);
        if !same_path(&absolute(name, &dir), expected) {
            return Err(mismatch("file of the chain"));
        }
        if let Some(f) = img.get("backing-filename-format").and_then(Value::as_str)
            && f != "vmdk"
        {
            return Err(mismatch("backing format is not vmdk"));
        }
        let size = img
            .get("virtual-size")
            .and_then(Value::as_u64)
            .filter(|s| *s > 0)
            .ok_or(mismatch("virtual size"))?;
        if *virtual_size.get_or_insert(size) != size {
            return Err(mismatch("virtual size differs along the chain"));
        }
        dirty |= img.get("dirty-flag").and_then(Value::as_bool) == Some(true);
        // Files the image opened: its own file and its extents.
        let mut opened: Vec<PathBuf> = Vec::new();
        if let Some(children) = img.get("children").and_then(Value::as_array) {
            for c in children {
                let info = c.get("info").ok_or(mismatch("child image"))?;
                let f = info
                    .get("filename")
                    .and_then(Value::as_str)
                    .ok_or(mismatch("child file"))?;
                opened.push(absolute(f, &dir));
                allocated = allocated
                    .saturating_add(info.get("actual-size").and_then(Value::as_u64).unwrap_or(0));
            }
        }
        if let Some(exts) = img
            .pointer("/format-specific/data/extents")
            .and_then(Value::as_array)
        {
            for e in exts {
                if let Some(f) = e.get("filename").and_then(Value::as_str) {
                    opened.push(absolute(f, &dir));
                }
            }
        }
        if opened.is_empty() {
            allocated = allocated
                .saturating_add(img.get("actual-size").and_then(Value::as_u64).unwrap_or(0));
        }
        for p in &opened {
            if !disk.files.iter().any(|f| same_path(f, p)) {
                return Err(mismatch("a file outside the checked chain"));
            }
        }
    }
    Ok(ChainFacts {
        virtual_size: virtual_size.ok_or(mismatch("virtual size"))?,
        allocated_bytes: allocated,
        depth: u32::try_from(images.len()).unwrap_or(u32::MAX),
        dirty,
    })
}

/// `name` as reported by `qemu-img` (absolute, or relative to the folder
/// of the image that names it).
fn absolute(name: &str, dir: &Path) -> PathBuf {
    let p = PathBuf::from(if cfg!(windows) {
        name.replace('/', "\\")
    } else {
        name.to_owned()
    });
    if p.is_absolute() { p } else { dir.join(p) }
}
