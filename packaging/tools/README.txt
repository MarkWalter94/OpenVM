LightVM Tools (experimental)

Enable Tools per VM in Edit. Start the VM and open its LIGHTVM_TOOLS CD:
Windows: run INSTALL.CMD as the guest user. The installer configures per-user
startup. An administrator can additionally enable the guest WebClient service
and its large-file limit. No host driver is installed.
Linux: run sh INSTALL.SH in the graphical guest session. Python 3 is required;
clipboard needs wl-clipboard or xclip (plus Pillow for images), accessibility
needs python3-pyatspi.

Named shared folders are available through guest-loopback WebDAV:
http://127.0.0.1:47081/<ShareName>/. Windows attempts drive mapping. Linux can
open dav://127.0.0.1:47081/<ShareName>/ in a WebDAV-capable file manager.
The host enforces read-only/write scope independently of guest mappings.
The WebDAV compatibility layer does not yet enforce exclusive file locks.

Console clipboard buttons perform one explicit text or image copy in each
direction while the console window has focus. Images are limited to 4,194,304
pixels and 8192 pixels per side. Text synchronization is separately opt-in
and runs only while the guest screen captures input. Console file pickers,
host file drops and prepared Outbox copy drag-out (Windows hosts) are present.
Native drag into Explorer still needs manual qualification. USB redirection
is not implemented; no host USB driver is installed by this bundle.

Agent sessions require separate approval for every integration. UI Automation
must be installed in Windows; older systems can return unsupported. Password
values are masked. No shell or host path is exposed to agents.

Files are streamed with exact size/SHA-256 validation and create-only guest
inbox publication; host share writes can explicitly replace a file. Interrupted
writes are cancelled on channel disconnect. A private supervisor journal records host staging writes. On the next Tools
start it removes recorded staging files; final destinations are preserved.
Corrupt records or links stop recovery. Host power-loss durability is unqualified.

Ubuntu live/WHPX passed a synthetic 750 MB transfer, WebDAV operations and
revocation/reconnect/flush. Disposable Windows PE x64 passed native clipboard,
image streaming and file-channel tests with a lab-only x64 build. Installed
guest interoperability remains unqualified. The shipped PE is cross-compiled
for i686, subsystem 5.1, without UCRT imports; static checks and x64 WinPE tests
do not certify XP or another installed Windows release.

Running disk snapshots request a filesystem flush when Tools are configured.
Busy transfers or unavailable/failed Tools refuse the snapshot. This is not
a filesystem freeze or application-consistent snapshot; Windows fixed-volume
flush requires elevation inside the guest.
