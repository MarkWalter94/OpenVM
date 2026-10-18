//! Handle-relative directory traversal. Symlinks, junctions, reparse points,
//! alternate streams and device paths are never followed. Opening the root
//! also checks every ancestor; canonicalize-and-prefix-check is not used.

use std::fs::{File, Metadata};
use std::io;
use std::path::{Component, Path};

use crate::protocol::{RelativePath, invalid};

/// Prefix of the host-owned staging names used by unpublished transfers.
pub(crate) const STAGING_PREFIX: &str = ".lightvm-transfer-";

/// Names reserved for host staging files. The guest learns a transfer id
/// from `Begin`, so it must never address these names itself. NTFS compares
/// names case-insensitively and may also expose an 8.3 alias of them.
pub(crate) fn reserved_name(name: &str) -> bool {
    let staging = name
        .get(..STAGING_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(STAGING_PREFIX));
    #[cfg(windows)]
    let staging = staging || short_name_alias(name);
    staging
}

/// An 8.3 generated alias (`BASE~N.EXT`) can name any long file on volumes
/// with short names enabled, so it is never accepted as guest input.
#[cfg(windows)]
fn short_name_alias(name: &str) -> bool {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let Some((stem, digits)) = base.rsplit_once('~') else {
        return false;
    };
    base.len() <= 8
        && !stem.is_empty()
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && ext.len() <= 3
        && !ext.contains('.')
}

#[derive(Debug)]
pub struct Directory {
    file: File,
    #[cfg(windows)]
    path: std::path::PathBuf,
    #[cfg(windows)]
    ancestors: Vec<File>,
}

impl Directory {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("share root must be absolute"));
        }
        #[cfg(unix)]
        {
            let mut d = Self {
                file: File::open("/")?,
            };
            for c in path.components() {
                match c {
                    Component::RootDir => {}
                    Component::Normal(n) => d = d.child_dir(n)?,
                    _ => return Err(invalid("invalid share root")),
                }
            }
            Ok(d)
        }
        #[cfg(windows)]
        {
            use std::path::Prefix;
            let mut components = path.components();
            if !matches!(components.next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            {
                return Err(invalid("share must be on a local drive"));
            }
            if !matches!(components.next(), Some(Component::RootDir)) {
                return Err(invalid("invalid drive root"));
            }
            let root: std::path::PathBuf = path.components().take(2).collect();
            let mut d = Self {
                file: win_open(&root, true, false, false)?,
                path: root,
                ancestors: Vec::new(),
            };
            for c in components {
                match c {
                    Component::Normal(n) => d = d.child_dir(n)?,
                    _ => return Err(invalid("invalid share root")),
                }
            }
            Ok(d)
        }
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }

    pub fn descend(&self, path: &RelativePath) -> io::Result<Self> {
        path.validate()?;
        let mut d = self.try_clone()?;
        for c in path.components() {
            d = d.child_dir(c.as_ref())?;
        }
        Ok(d)
    }

    pub fn parent(&self, path: &RelativePath) -> io::Result<(Self, String)> {
        path.validate()?;
        let (last, rest) = path
            .components()
            .split_last()
            .ok_or_else(|| invalid("file name required"))?;
        Ok((
            self.descend(&RelativePath::new(rest.to_vec())?)?,
            last.clone(),
        ))
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            #[cfg(windows)]
            path: self.path.clone(),
            #[cfg(windows)]
            ancestors: self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
        })
    }

    fn child_dir(&self, name: &std::ffi::OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let fd = openat(
                &self.file,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(Self { file: fd.into() })
        }
        #[cfg(windows)]
        {
            let mut ancestors = self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<Vec<_>>>()?;
            ancestors.push(self.file.try_clone()?);
            let path = self.path.join(name);
            Ok(Self {
                file: win_open(&path, true, false, false)?,
                path,
                ancestors,
            })
        }
    }

    pub fn read_file(&self, name: &str) -> io::Result<File> {
        RelativePath::new(vec![name.into()])?;
        #[cfg(unix)]
        let f: File = {
            use rustix::fs::{Mode, OFlags, openat};
            openat(
                &self.file,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into()
        };
        #[cfg(windows)]
        let f = win_open(&self.path.join(name), false, false, false)?;
        if !f.metadata()?.is_file() {
            return Err(invalid("regular file required"));
        }
        Ok(f)
    }

    pub fn create_file(&self, name: &str) -> io::Result<File> {
        RelativePath::new(vec![name.into()])?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            Ok(openat(
                &self.file,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?
            .into())
        }
        #[cfg(windows)]
        {
            win_open(&self.path.join(name), false, true, true)
        }
    }

    /// Publishes without overwriting anything, including an attacker-created
    /// symlink at the destination. The staging file remains on failure.
    pub fn publish(&self, staging: &str, target: &str) -> io::Result<()> {
        RelativePath::new(vec![staging.into()])?;
        RelativePath::new(vec![target.into()])?;
        #[cfg(unix)]
        {
            rustix::fs::linkat(
                &self.file,
                staging,
                &self.file,
                target,
                rustix::fs::AtFlags::empty(),
            )?;
            self.remove(staging)?;
            self.file.sync_all()
        }
        #[cfg(windows)]
        {
            std::fs::hard_link(self.path.join(staging), self.path.join(target))?;
            self.remove(staging)
        }
    }

    pub fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.file.sync_all()
        }
        #[cfg(windows)]
        {
            Ok(())
        }
    }

    pub fn remove(&self, name: &str) -> io::Result<()> {
        RelativePath::new(vec![name.into()])?;
        #[cfg(unix)]
        {
            Ok(rustix::fs::unlinkat(
                &self.file,
                name,
                rustix::fs::AtFlags::empty(),
            )?)
        }
        #[cfg(windows)]
        {
            std::fs::remove_file(self.path.join(name))
        }
    }

    pub fn mkdir(&self, name: &str) -> io::Result<()> {
        RelativePath::new(vec![name.into()])?;
        #[cfg(unix)]
        {
            Ok(rustix::fs::mkdirat(
                &self.file,
                name,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
            )?)
        }
        #[cfg(windows)]
        {
            std::fs::create_dir(self.path.join(name))
        }
    }

    pub fn remove_entry(&self, name: &str) -> io::Result<()> {
        if self.read_file(name).is_ok() {
            return self.remove(name);
        }
        drop(self.child_dir(name.as_ref())?);
        #[cfg(unix)]
        {
            Ok(rustix::fs::unlinkat(
                &self.file,
                name,
                rustix::fs::AtFlags::REMOVEDIR,
            )?)
        }
        #[cfg(windows)]
        {
            std::fs::remove_dir(self.path.join(name))
        }
    }

    pub fn rename_to(
        &self,
        from: &str,
        destination: &Self,
        to: &str,
        replace: bool,
    ) -> io::Result<()> {
        RelativePath::new(vec![from.into()])?;
        RelativePath::new(vec![to.into()])?;
        let regular = self.read_file(from).is_ok();
        if !regular {
            drop(self.child_dir(from.as_ref())?);
        }
        // Refuse a symlink/reparse point already present at the target.
        if replace {
            match destination.read_file(to) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        #[cfg(unix)]
        {
            let flags = if replace {
                rustix::fs::RenameFlags::empty()
            } else {
                rustix::fs::RenameFlags::NOREPLACE
            };
            rustix::fs::renameat_with(&self.file, from, &destination.file, to, flags)?;
            self.file.sync_all()?;
            destination.file.sync_all()
        }
        #[cfg(windows)]
        {
            if !replace && regular {
                std::fs::hard_link(self.path.join(from), destination.path.join(to))?;
                self.remove(from)
            } else {
                // MoveFileEx cannot replace an existing destination directory.
                std::fs::rename(self.path.join(from), destination.path.join(to))
            }
        }
    }

    pub fn entries(&self) -> io::Result<Vec<crate::protocol::Entry>> {
        let mut entries = Vec::new();
        #[cfg(unix)]
        let names = {
            let mut dir = rustix::fs::Dir::read_from(&self.file)?;
            let mut names = Vec::new();
            for item in &mut dir {
                let item = item?;
                let name = item.file_name().to_string_lossy().into_owned();
                if name == "." || name == ".." {
                    continue;
                }
                if names.len() >= crate::protocol::MAX_ENTRIES {
                    return Err(invalid("too many directory entries"));
                }
                names.push(name);
            }
            names
        };
        #[cfg(windows)]
        let names = {
            let mut names = Vec::new();
            for item in std::fs::read_dir(&self.path)? {
                if names.len() >= crate::protocol::MAX_ENTRIES {
                    return Err(invalid("too many directory entries"));
                }
                names.push(item?.file_name().to_string_lossy().into_owned());
            }
            names
        };
        for name in names {
            if RelativePath::new(vec![name.clone()]).is_err() || reserved_name(&name) {
                continue;
            }
            let metadata = if let Ok(f) = self.read_file(&name) {
                f.metadata().ok()
            } else {
                self.child_dir(name.as_ref())
                    .and_then(|d| d.metadata())
                    .ok()
            };
            if let Some(m) = metadata {
                entries.push(crate::protocol::Entry {
                    name,
                    directory: m.is_dir(),
                    size: if m.is_dir() { 0 } else { m.len() },
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

#[cfg(windows)]
fn win_open(path: &Path, directory: bool, write: bool, create: bool) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    // OPEN_REPARSE_POINT, BACKUP_SEMANTICS; omit FILE_SHARE_DELETE so the
    // held directory chain cannot be renamed/replaced during traversal.
    let f = std::fs::OpenOptions::new()
        .read(!write)
        .write(write)
        .create_new(create)
        .share_mode(1 | 2)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)?;
    let m = f.metadata()?;
    if m.file_attributes() & 0x400 != 0 || m.is_dir() != directory {
        return Err(invalid("reparse point or unexpected file type"));
    }
    Ok(f)
}
