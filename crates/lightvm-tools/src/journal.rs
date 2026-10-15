//! Private supervisor journal for recovering unpublished host share writes.
//! Only the user-selected host root is recorded. Recovery never removes a final
//! destination and never follows links; corrupt records stop recovery.
use crate::{
    protocol::{RelativePath, invalid},
    sandbox::Directory,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub struct Journal {
    root: Arc<Directory>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u32,
    root: PathBuf,
    path: RelativePath,
    staging: String,
}
#[derive(Debug)]
pub struct Cleanup {
    root: Arc<Directory>,
    name: String,
}
impl Cleanup {
    pub fn finish(self) -> io::Result<()> {
        self.root.remove(&self.name)?;
        self.root.sync()
    }
}
impl Journal {
    /// The caller creates this directory with the supervisor's private ACL.
    pub fn open(private: &Path) -> io::Result<Self> {
        Ok(Self {
            root: Arc::new(Directory::open(private)?),
        })
    }
    pub fn recover(&self) -> io::Result<usize> {
        let mut recovered = 0;
        for entry in self.root.entries()? {
            let Some(id) = entry.name.strip_suffix(".json") else {
                return Err(invalid("unknown Tools journal entry"));
            };
            if !valid_id(id) || entry.directory || entry.size > 16384 {
                return Err(invalid("invalid Tools journal record"));
            }
            let mut bytes = Vec::new();
            self.root
                .read_file(&entry.name)?
                .take(16385)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 16384 {
                return Err(invalid("Tools journal record too large"));
            }
            let record: Entry =
                serde_json::from_slice(&bytes).map_err(|_| invalid("corrupt Tools journal"))?;
            if record.version != 1
                || !record.root.is_absolute()
                || record.staging != format!(".lightvm-transfer-{id}")
            {
                return Err(invalid("invalid Tools recovery path"));
            }
            record.path.validate()?;
            let root = Directory::open(&record.root)?;
            let (parent, _) = root.parent(&record.path)?;
            match parent.read_file(&record.staging) {
                Ok(file) => {
                    drop(file);
                    parent.remove(&record.staging)?;
                    parent.sync()?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e),
            }
            self.root.remove(&entry.name)?;
            recovered += 1;
        }
        self.root.sync()?;
        Ok(recovered)
    }
    pub fn prepare(&self, root: &Path, path: &RelativePath, id: &str) -> io::Result<Cleanup> {
        if !root.is_absolute() || !valid_id(id) {
            return Err(invalid("invalid journal admission"));
        }
        path.validate()?;
        let name = format!("{id}.json");
        let entry = Entry {
            version: 1,
            root: root.to_owned(),
            path: path.clone(),
            staging: format!(".lightvm-transfer-{id}"),
        };
        let bytes =
            serde_json::to_vec(&entry).map_err(|_| invalid("journal serialization failed"))?;
        if bytes.len() > 16384 {
            return Err(invalid("journal admission too large"));
        }
        let mut file = self.root.create_file(&name)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.root.sync()?;
        Ok(Cleanup {
            root: self.root.clone(),
            name,
        })
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
