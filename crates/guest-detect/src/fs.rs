//! The common read-only view of the ISO 9660 and UDF file systems.

use std::io::{Read, Seek};

use crate::DetectError;
use crate::disk::{Disk, FileData};

/// Deepest path looked up.
pub(crate) const MAX_PATH_DEPTH: usize = 8;

/// One entry of a directory.
#[derive(Clone, Debug)]
pub(crate) struct DirEntry<N> {
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) node: N,
}

/// A read-only file system on the image.
pub(crate) trait Fs {
    type Node: Clone;

    fn root(&self) -> Self::Node;

    fn list<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        dir: &Self::Node,
    ) -> Result<Vec<DirEntry<Self::Node>>, DetectError>;

    fn open<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        node: &Self::Node,
    ) -> Result<FileData, DetectError>;
}

/// Case-insensitive name comparison (Unicode simple lower case).
pub(crate) fn name_eq(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// Looks up `path` (components, case-insensitive) from the root. The last
/// component must be a directory when `want_dir`, a file otherwise.
pub(crate) fn find<F: Fs, R: Read + Seek>(
    fs: &F,
    disk: &mut Disk<'_, R>,
    path: &[&str],
    want_dir: bool,
) -> Result<Option<F::Node>, DetectError> {
    if path.len() > MAX_PATH_DEPTH {
        return Err(DetectError::LimitExceeded("path depth"));
    }
    let mut node = fs.root();
    for (i, comp) in path.iter().enumerate() {
        let last = i + 1 == path.len();
        let entries = fs.list(disk, &node)?;
        let Some(e) = entries
            .into_iter()
            .find(|e| e.is_dir == (!last || want_dir) && name_eq(&e.name, comp))
        else {
            return Ok(None);
        };
        node = e.node;
    }
    Ok(Some(node))
}
