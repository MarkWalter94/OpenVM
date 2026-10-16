//! Console file transfers. Files are streamed on a worker; only bounded RPC
//! chunks cross the UI thread. Host paths never reach the guest.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use lightvm_proto::{
    RunId, ToolCall, ToolFileEntry, ToolFileReply, ToolFileRequest, ToolRelativePath, ToolReply,
    VmId, VmState,
};
use sha2::{Digest, Sha256};

use crate::service::{RequestStatus, ServiceCommand, ServiceState};

const CHUNK: usize = 16384;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 16;
type Result<T> = std::result::Result<T, String>;

fn path(name: &str) -> Result<ToolRelativePath> {
    ToolRelativePath::new(vec![name.into()]).map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 15) as usize] as char,
            ]
        })
        .collect()
}

fn decode(data: &str) -> Result<Vec<u8>> {
    if data.len() > CHUNK * 2 || !data.len().is_multiple_of(2) {
        return Err("Invalid guest file chunk".into());
    }
    data.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            let a = (p[0] as char)
                .to_digit(16)
                .ok_or("Invalid guest file chunk")?;
            let b = (p[1] as char)
                .to_digit(16)
                .ok_or("Invalid guest file chunk")?;
            Ok((a * 16 + b) as u8)
        })
        .collect()
}

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|e| e.to_string())
}

#[derive(Debug, Default)]
struct Progress {
    cancel: AtomicBool,
    bytes: AtomicU64,
    total: AtomicU64,
}
impl Progress {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err("Transfer cancelled".into())
        } else {
            Ok(())
        }
    }
}

fn ok(reply: ToolFileReply) -> Result<()> {
    match reply {
        ToolFileReply::Ok => Ok(()),
        ToolFileReply::Error { code } => Err(format!("Guest refused file operation: {code}")),
        _ => Err("Unexpected guest file reply".into()),
    }
}

fn upload(
    source: &Path,
    progress: &Progress,
    call: &mut impl FnMut(ToolFileRequest) -> Result<ToolFileReply>,
) -> Result<()> {
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid file name")?;
    let target = path(name)?;
    if !io(std::fs::metadata(source))?.is_file() {
        return Err("Choose regular files only".into());
    }
    let mut file = io(File::open(source))?;
    let metadata = io(file.metadata())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("Choose regular files of at most 4 GiB each".into());
    }
    let size = metadata.len();
    progress.total.store(size, Ordering::Relaxed);
    progress.bytes.store(0, Ordering::Relaxed);
    let mut buffer = vec![0; CHUNK];
    let mut hash = Sha256::new();
    let mut hashed = 0;
    loop {
        progress.check()?;
        let n = io(file.read(&mut buffer))?;
        if n == 0 {
            break;
        }
        hashed += n as u64;
        if hashed > size {
            return Err("Source file changed".into());
        }
        hash.update(&buffer[..n]);
    }
    if hashed != size {
        return Err("Source file changed".into());
    }
    progress.check()?;
    let transfer = match call(ToolFileRequest::Begin {
        replace: false,
        share: "inbox".into(),
        path: target,
        size,
        sha256: hex(&hash.finalize()),
    })? {
        ToolFileReply::Transfer { transfer } => transfer,
        _ => return Err("Guest did not start the transfer".into()),
    };
    let result = (|| {
        io(file.seek(SeekFrom::Start(0)))?;
        let mut sent = 0;
        loop {
            progress.check()?;
            let n = io(file.read(&mut buffer))?;
            if n == 0 {
                break;
            }
            if sent + n as u64 > size {
                return Err("Source file changed".into());
            }
            ok(call(ToolFileRequest::Write {
                transfer: transfer.clone(),
                offset: sent,
                data: hex(&buffer[..n]),
            })?)?;
            sent += n as u64;
            progress.bytes.store(sent, Ordering::Relaxed);
        }
        if sent != size {
            return Err("Source file changed".into());
        }
        progress.check()?;
        ok(call(ToolFileRequest::Commit {
            transfer: transfer.clone(),
        })?)
    })();
    if result.is_err() {
        let _ = call(ToolFileRequest::Cancel { transfer });
    }
    result
}

fn download(
    entry: &ToolFileEntry,
    destination: &Path,
    progress: &Progress,
    call: &mut impl FnMut(ToolFileRequest) -> Result<ToolFileReply>,
) -> Result<()> {
    let source = path(&entry.name)?;
    if entry.directory || entry.size > MAX_BYTES {
        return Err("Choose an Outbox file of at most 4 GiB".into());
    }
    let parent = destination.parent().ok_or("Choose a destination folder")?;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => return Err("Destination already exists; choose a new name".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    // Create-only, next to the destination so publication stays on one volume.
    let staging = parent.join(format!(".lightvm-transfer-{}", RunId::new_v7()));
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging))?;
    progress.total.store(entry.size, Ordering::Relaxed);
    progress.bytes.store(0, Ordering::Relaxed);
    let result = (|| {
        let mut received = 0;
        loop {
            progress.check()?;
            let (offset, data, eof) = match call(ToolFileRequest::Read {
                share: "outbox".into(),
                path: source.clone(),
                offset: received,
                length: CHUNK as u32,
            })? {
                ToolFileReply::Data { offset, data, eof } => (offset, decode(&data)?, eof),
                _ => return Err("Guest did not return file data".into()),
            };
            if offset != received
                || data.len() as u64 > entry.size.saturating_sub(received)
                || (data.is_empty() && !eof)
            {
                return Err("Invalid guest file offset or size".into());
            }
            io(file.write_all(&data))?;
            received += data.len() as u64;
            progress.bytes.store(received, Ordering::Relaxed);
            if eof {
                break;
            }
        }
        if received != entry.size {
            return Err("Guest file changed; refresh Outbox and retry".into());
        }
        progress.check()?;
        io(file.sync_all())
    })();
    drop(file);
    // hard_link is atomic and refuses an existing name, unlike rename on Unix.
    let result = result.and_then(|()| {
        progress.check()?;
        io(std::fs::hard_link(&staging, destination))
    });
    let cleanup = io(std::fs::remove_file(&staging));
    result.and(cleanup)
}

enum Event {
    Call(ToolFileRequest, mpsc::Sender<Result<ToolFileReply>>),
    Done(Result<Completed>),
}

enum Completed {
    Transfer,
    List(Vec<ToolFileEntry>),
    Image(Vec<u8>),
    Prepared(Prepared),
}

struct Prepared {
    target: (VmId, RunId),
    root: PathBuf,
    path: PathBuf,
}
impl Prepared {
    fn new(target: (VmId, RunId), name: &str) -> Result<Self> {
        path(name)?;
        let root = std::env::temp_dir().join(format!("lightvm-drag-{}", RunId::new_v7()));
        io(std::fs::create_dir(&root))?;
        let result = Self {
            target,
            path: root.join(name),
            root,
        };
        crate::pairing::private_dir(&result.root)?;
        Ok(result)
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        // Only our one cache file and its empty folder; never recursive deletion.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.root);
    }
}

struct Job {
    image: bool,
    target: (VmId, RunId),
    progress: Arc<Progress>,
    events: mpsc::Receiver<Event>,
    waiting: Option<mpsc::Sender<Result<ToolFileReply>>>,
}

enum Operation {
    Upload(Vec<PathBuf>),
    Download(ToolFileEntry, PathBuf),
    List,
    ImageSend,
    ImageReceive,
    Prepare(ToolFileEntry),
}

/// Console transfer controls, with one operation at a time across all VMs.
pub struct Transfers {
    prepared: Option<Prepared>,
    drag: Option<PathBuf>,
    clipboard: Option<arboard::Clipboard>,
    image_result: bool,
    job: Option<Job>,
    outbox: Vec<ToolFileEntry>,
    listed: Option<(VmId, RunId)>,
    message: Option<String>,
    pick: Option<crate::FilePicker>,
    save: Option<crate::SavePicker>,
}

impl Default for Transfers {
    fn default() -> Self {
        Self {
            prepared: None,
            drag: None,
            clipboard: None,
            image_result: false,
            job: None,
            outbox: Vec::new(),
            listed: None,
            message: None,
            pick: native_pick(),
            save: native_save(),
        }
    }
}

impl std::fmt::Debug for Transfers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transfers")
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

impl Drop for Transfers {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Transfers {
    pub fn take_drag(&mut self) -> Option<PathBuf> {
        self.drag.take()
    }
    /// Explicit console action. The worker never reads a clipboard by default.
    pub fn clipboard_image(&mut self, target: (VmId, RunId), receive: bool, ctx: egui::Context) {
        self.start(
            target,
            if receive {
                Operation::ImageReceive
            } else {
                Operation::ImageSend
            },
            ctx,
        );
    }
    /// Overrides native dialogs (also used by headless UI tests).
    pub fn set_pickers(
        &mut self,
        pick: Option<crate::FilePicker>,
        save: Option<crate::SavePicker>,
    ) {
        self.pick = pick;
        self.save = save;
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn start(&mut self, target: (VmId, RunId), operation: Operation, ctx: egui::Context) {
        if self.busy() {
            return;
        }
        if matches!(operation, Operation::List) {
            self.listed = None;
            self.outbox.clear();
        }
        let progress = Arc::new(Progress::default());
        let image = matches!(operation, Operation::ImageSend | Operation::ImageReceive);
        self.image_result = image;
        let worker_progress = progress.clone();
        let (tx, events) = mpsc::channel();
        self.message = None;
        self.job = Some(Job {
            image,
            target,
            progress,
            events,
            waiting: None,
        });
        std::thread::spawn(move || {
            let mut call = |request| {
                let (reply, rx) = mpsc::channel();
                tx.send(Event::Call(request, reply))
                    .map_err(|_| "Transfer view closed")?;
                ctx.request_repaint();
                rx.recv().map_err(|_| "Transfer view closed")?
            };
            let result = match operation {
                Operation::Upload(files) => {
                    if files.is_empty() || files.len() > MAX_FILES {
                        Err("Drop between 1 and 16 files".into())
                    } else {
                        files
                            .iter()
                            .try_for_each(|source| upload(source, &worker_progress, &mut call))
                            .map(|()| Completed::Transfer)
                    }
                }
                Operation::Download(entry, destination) => {
                    download(&entry, &destination, &worker_progress, &mut call)
                        .map(|()| Completed::Transfer)
                }
                Operation::List => call(ToolFileRequest::List {
                    share: "outbox".into(),
                    path: ToolRelativePath::new(vec![]).expect("root path"),
                })
                .and_then(|reply| {
                    worker_progress.check()?;
                    match reply {
                        ToolFileReply::Entries { entries } => Ok(Completed::List(entries)),
                        _ => Err("Guest did not return the Outbox".into()),
                    }
                }),
                Operation::ImageSend => (|| {
                    worker_progress.check()?;
                    let image = arboard::Clipboard::new()
                        .and_then(|mut c| c.get_image())
                        .map_err(|_| "Host clipboard image is unavailable")?;
                    let data =
                        crate::clipboard_images::pack(image.width, image.height, &image.bytes)?;
                    crate::clipboard_images::send(
                        &data,
                        &worker_progress.cancel,
                        |bytes, total| {
                            worker_progress.bytes.store(bytes, Ordering::Relaxed);
                            worker_progress.total.store(total, Ordering::Relaxed);
                        },
                        &mut call,
                    )?;
                    Ok(Completed::Transfer)
                })(),
                Operation::ImageReceive => crate::clipboard_images::receive(
                    &worker_progress.cancel,
                    |bytes, total| {
                        worker_progress.bytes.store(bytes, Ordering::Relaxed);
                        worker_progress.total.store(total, Ordering::Relaxed);
                    },
                    &mut call,
                )
                .map(Completed::Image),
                Operation::Prepare(entry) => (|| {
                    let prepared = Prepared::new(target, &entry.name)?;
                    download(&entry, &prepared.path, &worker_progress, &mut call)?;
                    worker_progress.check()?;
                    Ok(Completed::Prepared(prepared))
                })(),
            };
            let _ = tx.send(Event::Done(result));
            ctx.request_repaint();
        });
    }

    /// Called on every app frame, including while the console page is hidden.
    /// The shared Tools reply slot is reserved until the operation completes.
    pub fn poll(
        &mut self,
        state: Option<&ServiceState>,
        ctx: &egui::Context,
        image_active: Option<(VmId, RunId)>,
    ) -> Option<ServiceCommand> {
        let job = self.job.as_mut()?;
        ctx.request_repaint_after(Duration::from_millis(100));
        let permitted = state.is_some_and(|s| {
            if job.image {
                clipboard_eligible(s, job.target) && image_active == Some(job.target)
            } else {
                eligible(s, job.target)
            }
        });
        if !permitted {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(waiting) = job.waiting.take() {
            let status = state
                .and_then(|s| s.tools.as_ref())
                .filter(|(v, r, _)| (*v, *r) == job.target)
                .map(|(_, _, s)| s);
            let reply = match status {
                Some(RequestStatus::Done(ToolReply::File { reply })) if !job.image => Some(
                    reply
                        .validate()
                        .map(|()| reply.clone())
                        .map_err(|e| e.to_string()),
                ),
                Some(RequestStatus::Done(ToolReply::ClipboardImage { reply })) if job.image => {
                    Some(
                        reply
                            .validate()
                            .map(|()| reply.clone())
                            .map_err(|e| e.to_string()),
                    )
                }
                Some(RequestStatus::Done(_)) => Some(Err("Unexpected Tools reply".into())),
                Some(RequestStatus::Failed(f)) => Some(Err(f.message.clone())),
                _ if !state.is_some_and(|s| {
                    s.connected
                        && s.vm(job.target.0)
                            .is_some_and(|v| v.run_id == Some(job.target.1))
                }) =>
                {
                    Some(Err("VM run or Tools permissions changed".into()))
                }
                _ if !state.is_some_and(|s| s.connected) => {
                    Some(Err("Supervisor disconnected".into()))
                }
                _ => None,
            };
            if let Some(reply) = reply {
                let _ = waiting.send(reply);
            } else {
                job.waiting = Some(waiting);
                return None;
            }
        }
        match job.events.try_recv() {
            Ok(Event::Call(request, reply)) => {
                if job.progress.cancel.load(Ordering::Relaxed)
                    && !matches!(request, ToolFileRequest::Cancel { .. })
                {
                    let _ = reply.send(Err("Transfer cancelled".into()));
                    return None;
                }
                job.waiting = Some(reply);
                Some(ServiceCommand::GuestTools {
                    vm_id: job.target.0,
                    run_id: job.target.1,
                    call: if job.image {
                        ToolCall::ClipboardImage { request }
                    } else {
                        ToolCall::File { request }
                    },
                })
            }
            Ok(Event::Done(result)) => {
                match result {
                    Ok(Completed::List(entries)) => {
                        self.listed = Some(job.target);
                        self.outbox = entries;
                        self.message = None;
                    }
                    Ok(Completed::Transfer) => {
                        self.message = Some(
                            if job.image {
                                "Clipboard image copied"
                            } else {
                                "File transfer completed"
                            }
                            .into(),
                        )
                    }
                    Ok(Completed::Image(data)) => {
                        self.message = Some(
                            if permitted && !job.progress.cancel.load(Ordering::Relaxed) {
                                match crate::clipboard_images::dimensions(&data) {
                                    Ok((width, height)) => {
                                        match arboard::Clipboard::new().and_then(|mut c| {
                                            c.set_image(arboard::ImageData {
                                                width,
                                                height,
                                                bytes: std::borrow::Cow::Owned(data[12..].to_vec()),
                                            })?;
                                            Ok(c)
                                        }) {
                                            Ok(c) => {
                                                self.clipboard = Some(c);
                                                "Guest image copied to the host clipboard".into()
                                            }
                                            Err(_) => "Host image clipboard is unavailable".into(),
                                        }
                                    }
                                    Err(e) => e,
                                }
                            } else {
                                "Clipboard image transfer cancelled".into()
                            },
                        );
                    }
                    Ok(Completed::Prepared(prepared)) => {
                        if permitted && !job.progress.cancel.load(Ordering::Relaxed) {
                            self.prepared = Some(prepared);
                            self.message = Some("Ready to drag. Keep LightVM open until the target app finishes copying.".into());
                        } else {
                            self.message = Some("File preparation cancelled".into());
                        }
                    }
                    Err(error) => {
                        self.message = Some(if job.image {
                            error
                        } else {
                            format!(
                                "{error}. Check the destination before retrying; completed files are kept."
                            )
                        });
                    }
                }
                self.job = None;
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.message = Some("File transfer worker stopped".into());
                self.job = None;
                None
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        state: Option<&ServiceState>,
        vm: VmId,
        tools_busy: bool,
    ) {
        let target = state
            .and_then(|s| s.vm(vm))
            .and_then(|v| v.run_id)
            .map(|r| (vm, r));
        let enabled = target.is_some_and(|target| state.is_some_and(|s| eligible(s, target)));
        if !enabled && !self.busy() && !(self.image_result && self.message.is_some()) {
            return;
        }
        egui::CollapsingHeader::new(if self.image_result { "Clipboard image transfer" } else { "Files · Guest Inbox / Outbox" }).default_open(true).show(ui, |ui| {
            if enabled {
            ui.label("Send files to the guest Inbox. Put guest files in Outbox to receive them here. Up to 4 GiB per file; existing files are kept.");
            let busy = self.busy();
            ui.add_enabled_ui(enabled && !busy && !tools_busy, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.pick.is_some(), egui::Button::new("Send file…")).clicked()
                        && let Some(source) = self.pick.as_mut().and_then(|pick| pick("Send file to guest Inbox"))
                        && let Some(target) = target { self.start(target, Operation::Upload(vec![source]), ui.ctx().clone()); }
                    if ui.button("Refresh Outbox").clicked() && let Some(target) = target { self.start(target, Operation::List, ui.ctx().clone()); }
                });
                if self.listed == target {
                    if self.outbox.iter().all(|entry| entry.directory) {
                        ui.label("No files in the Outbox root");
                    }
                    let mut receive = None;
                    let mut prepare = None;
                    egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                        for entry in &self.outbox {
                            if !entry.directory {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{} · {} bytes", entry.name, entry.size));
                                    if ui.add_enabled(self.save.is_some() && entry.size <= MAX_BYTES, egui::Button::new("Receive…")).clicked() {
                                        receive = Some(entry.clone());
                                    }
                                    if crate::file_drag::supported() && ui.add_enabled(self.prepared.is_none() && entry.size <= MAX_BYTES, egui::Button::new("Prepare drag…")).clicked() {
                                        prepare = Some(entry.clone());
                                    }
                                });
                            }
                        }
                    });
                    if let Some(entry) = receive
                        && let Some(destination) = self.save.as_mut().and_then(|save| save(&entry.name))
                        && let Some(target) = target { self.start(target, Operation::Download(entry, destination), ui.ctx().clone()); }
                    if let Some(entry) = prepare && let Some(target) = target { self.start(target, Operation::Prepare(entry), ui.ctx().clone()); }
                }
            });
            if let Some(prepared) = &self.prepared {
                if Some(prepared.target) == target && enabled && !self.busy() {
                    let response = ui.add(egui::Label::new(format!("Drag {} to a host folder", prepared.path.file_name().unwrap_or_default().to_string_lossy())).sense(egui::Sense::drag()));
                    if response.drag_started() { self.drag = Some(prepared.path.clone()); }
                }
                if ui.button("Clear prepared copy").clicked() { self.prepared = None; }
            }
            }
            if let Some(job) = &self.job {
                if let Some(vm) = state.and_then(|s| s.vm(job.target.0)) {
                    ui.label(format!("Transfer · {}", vm.name));
                }
                let bytes = job.progress.bytes.load(Ordering::Relaxed);
                let total = job.progress.total.load(Ordering::Relaxed);
                ui.add(egui::ProgressBar::new(if total == 0 { 0.0 } else { bytes as f32 / total as f32 }).text(format!("{bytes} / {total} bytes · checking or transferring")));
                if ui.button("Cancel transfer").clicked() { job.progress.cancel.store(true, Ordering::Relaxed); }
            }
            if let Some(message) = &self.message { ui.label(message); }
        });
    }

    /// Only files dropped onto the active console are accepted, never paths
    /// supplied by the guest. Directories and unsupported names fail on worker.
    pub fn drops(
        &mut self,
        ctx: &egui::Context,
        rect: egui::Rect,
        state: Option<&ServiceState>,
        vm: VmId,
        tools_busy: bool,
    ) {
        if self.busy()
            || tools_busy
            || !ctx.input(|i| i.pointer.latest_pos().is_some_and(|p| rect.contains(p)))
        {
            return;
        }
        let Some(target) = state
            .and_then(|s| s.vm(vm))
            .and_then(|v| v.run_id)
            .map(|r| (vm, r))
        else {
            return;
        };
        if !state.is_some_and(|s| eligible(s, target)) {
            return;
        }
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        if files.is_empty() {
            return;
        }
        if files.len() > MAX_FILES || files.iter().any(|f| !f.path().is_absolute()) {
            self.message = Some("Drop at most 16 local files".into());
            return;
        }
        self.start(
            target,
            Operation::Upload(files.into_iter().map(|f| f.path().to_path_buf()).collect()),
            ctx.clone(),
        );
    }
}

fn eligible(s: &ServiceState, (vm, run): (VmId, RunId)) -> bool {
    s.connected
        && !s.agents.agent_has_input(vm)
        && s.vm(vm).is_some_and(|v| {
            v.run_id == Some(run)
                && v.state == VmState::Running
                && v.config.tools.enabled
                && v.config.tools.files_transfer
        })
}

fn clipboard_eligible(s: &ServiceState, (vm, run): (VmId, RunId)) -> bool {
    s.connected
        && !s.agents.agent_has_input(vm)
        && s.vm(vm).is_some_and(|v| {
            v.run_id == Some(run)
                && v.state == VmState::Running
                && v.config.tools.enabled
                && v.config.tools.clipboard
        })
}

fn native_pick() -> Option<crate::FilePicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|title| {
            rfd::FileDialog::new().set_title(title).pick_file()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}
fn native_save() -> Option<crate::SavePicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|name| {
            rfd::FileDialog::new()
                .set_title("Receive guest Outbox file")
                .set_file_name(name)
                .save_file()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("lightvm-desktop-transfer-{}", RunId::new_v7()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn assert_clean(&self) {
            assert!(std::fs::read_dir(&self.0).unwrap().all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".lightvm-transfer-")
            }));
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn entry(size: u64) -> ToolFileEntry {
        ToolFileEntry {
            name: "build.bin".into(),
            directory: false,
            size,
        }
    }

    #[test]
    fn upload_streams_bounded_chunks_and_hashes_the_held_file() {
        let root = Temp::new();
        let source = root.file("build.bin");
        let data = vec![0xab; 3 * CHUNK + 17];
        std::fs::write(&source, &data).unwrap();
        let mut received = Vec::new();
        let mut committed = false;
        upload(&source, &Progress::default(), &mut |request| {
            request.validate_guest().unwrap();
            match request {
                ToolFileRequest::Begin {
                    size, sha256, path, ..
                } => {
                    assert_eq!(size, data.len() as u64);
                    assert_eq!(sha256, hex(&Sha256::digest(&data)));
                    assert_eq!(path.components(), &["build.bin"]);
                    Ok(ToolFileReply::Transfer {
                        transfer: "ab".repeat(32),
                    })
                }
                ToolFileRequest::Write { offset, data, .. } => {
                    assert_eq!(offset, received.len() as u64);
                    received.extend(decode(&data).unwrap());
                    Ok(ToolFileReply::Ok)
                }
                ToolFileRequest::Commit { .. } => {
                    committed = true;
                    Ok(ToolFileReply::Ok)
                }
                _ => panic!("unexpected request"),
            }
        })
        .unwrap();
        assert!(committed);
        assert_eq!(received, data);
    }

    #[test]
    fn cancelling_upload_sends_cancel_without_committing() {
        let root = Temp::new();
        let source = root.file("build.bin");
        std::fs::write(&source, vec![1; CHUNK * 2]).unwrap();
        let progress = Progress::default();
        let mut cancelled = false;
        let result = upload(&source, &progress, &mut |request| match request {
            ToolFileRequest::Begin { .. } => Ok(ToolFileReply::Transfer {
                transfer: "ab".repeat(32),
            }),
            ToolFileRequest::Write { .. } => {
                progress.cancel.store(true, Ordering::Relaxed);
                Ok(ToolFileReply::Ok)
            }
            ToolFileRequest::Cancel { .. } => {
                cancelled = true;
                Ok(ToolFileReply::Ok)
            }
            _ => panic!("commit after cancellation"),
        });
        assert!(result.is_err());
        assert!(cancelled);
    }

    #[test]
    fn changed_source_cancels_the_guest_staging_file() {
        let root = Temp::new();
        let source = root.file("build.bin");
        std::fs::write(&source, [1; 10]).unwrap();
        let mut cancelled = false;
        assert!(
            upload(
                &source,
                &Progress::default(),
                &mut |request| match request {
                    ToolFileRequest::Begin { .. } => {
                        std::fs::write(&source, [2; 20]).unwrap();
                        Ok(ToolFileReply::Transfer {
                            transfer: "ab".repeat(32),
                        })
                    }
                    ToolFileRequest::Cancel { .. } => {
                        cancelled = true;
                        Ok(ToolFileReply::Ok)
                    }
                    _ => panic!("changed file must not be committed"),
                }
            )
            .is_err()
        );
        assert!(cancelled);
    }

    #[test]
    fn download_publishes_only_complete_files_and_never_overwrites() {
        let root = Temp::new();
        let target = root.file("chosen.bin");
        let data = vec![0x42; 2 * CHUNK + 19];
        let mut call = |request| match request {
            ToolFileRequest::Read {
                offset,
                length,
                path,
                ..
            } => {
                assert_eq!(path.components(), &["build.bin"]);
                let end = (offset as usize + length as usize).min(data.len());
                Ok(ToolFileReply::Data {
                    offset,
                    data: hex(&data[offset as usize..end]),
                    eof: end == data.len(),
                })
            }
            _ => panic!("unexpected request"),
        };
        download(
            &entry(data.len() as u64),
            &target,
            &Progress::default(),
            &mut call,
        )
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert!(
            download(
                &entry(data.len() as u64),
                &target,
                &Progress::default(),
                &mut call
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&target).unwrap(), data);
        root.assert_clean();
    }

    #[test]
    fn malformed_or_interrupted_download_leaves_no_destination() {
        for reply in [
            Ok(ToolFileReply::Data {
                offset: 1,
                data: "ab".into(),
                eof: true,
            }),
            Ok(ToolFileReply::Data {
                offset: 0,
                data: "zz".into(),
                eof: true,
            }),
            Ok(ToolFileReply::Data {
                offset: 0,
                data: "abab".into(),
                eof: true,
            }),
            Ok(ToolFileReply::Data {
                offset: 0,
                data: String::new(),
                eof: true,
            }),
            Ok(ToolFileReply::Data {
                offset: 0,
                data: String::new(),
                eof: false,
            }),
            Err("Supervisor disconnected".into()),
        ] {
            let root = Temp::new();
            let target = root.file("chosen.bin");
            assert!(
                download(&entry(1), &target, &Progress::default(), &mut |_| reply
                    .clone())
                .is_err()
            );
            assert!(!target.exists());
            root.assert_clean();
        }
    }

    #[test]
    fn cancellation_before_publication_removes_local_staging() {
        let root = Temp::new();
        let target = root.file("chosen.bin");
        let progress = Progress::default();
        assert!(
            download(&entry(1), &target, &progress, &mut |_| {
                progress.cancel.store(true, Ordering::Relaxed);
                Ok(ToolFileReply::Data {
                    offset: 0,
                    data: "ab".into(),
                    eof: true,
                })
            })
            .is_err()
        );
        assert!(!target.exists());
        root.assert_clean();
    }

    #[test]
    fn limits_and_guest_names_are_checked_before_io_or_rpc() {
        let root = Temp::new();
        let target = root.file("chosen.bin");
        let mut denied =
            |_: ToolFileRequest| -> Result<ToolFileReply> { panic!("must not contact guest") };
        assert!(
            download(
                &entry(MAX_BYTES + 1),
                &target,
                &Progress::default(),
                &mut denied
            )
            .is_err()
        );
        let mut bad = entry(1);
        bad.name = "../escape".into();
        assert!(download(&bad, &target, &Progress::default(), &mut denied).is_err());
        assert!(upload(&root.0, &Progress::default(), &mut denied).is_err());
        root.assert_clean();
    }

    #[test]
    fn empty_files_are_published() {
        let root = Temp::new();
        let target = root.file("empty.bin");
        download(&entry(0), &target, &Progress::default(), &mut |_| {
            Ok(ToolFileReply::Data {
                offset: 0,
                data: String::new(),
                eof: true,
            })
        })
        .unwrap();
        assert_eq!(std::fs::metadata(target).unwrap().len(), 0);
        root.assert_clean();
    }
}
