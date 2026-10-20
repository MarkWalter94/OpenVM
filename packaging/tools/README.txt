LightVM Tools (experimental)

Enable Tools per VM in Edit. Start the VM and open its LIGHTVM_TOOLS CD:
Windows: run INSTALL.CMD as the guest user. The installer configures per-user
startup. An administrator can additionally enable the guest WebClient service
and its large-file limit. No host driver is installed.
Linux: run sh INSTALL.SH in the graphical guest session. Python 3 is required;
clipboard needs wl-clipboard or xclip (plus Pillow for images), accessibility
needs python3-pyatspi.
At login Tools can request read-only mounting of their labelled optical CD
through the desktop's UDisks2 service (gdbus). This uses the guest user's
existing permission, without an administrator prompt. Other disks are not
mounted. If desktop mounting is unavailable or denied, open the CD manually.

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
values are masked. Host paths are not exposed to agents. Guest command
execution is separately opt-in: enable it in VM settings and approve the
guest.exec session permission. MCP vm_guest_exec starts a command inside
the VM as the Tools user; vm_guest_exec_poll returns output and exit status,
and vm_guest_exec_cancel stops it. Commands use /bin/sh on Linux and cmd.exe
on Windows, with a deadline up to 120 seconds (default 30) and capture limited
to the first 2048 bytes per stream. No graphical terminal is required.
Update the installed guest component to use this channel.

Files are streamed with exact size/SHA-256 validation and create-only guest
inbox publication; host share writes can explicitly replace a file. Interrupted
writes are cancelled on channel disconnect. A private supervisor journal records host staging writes. On the next Tools
start it removes recorded staging files; final destinations are preserved.
Corrupt records or links stop recovery. Host power-loss durability is unqualified.

Ubuntu live/WHPX passed a synthetic 750 MB transfer, WebDAV operations and
revocation/reconnect/flush. Disposable Windows PE x64 passed native clipboard,
image streaming and file-channel tests with a lab-only x64 build. Installed
Ubuntu 24.04 with installed Tools passed application build and checkpoint
tests. A separate installed Xfce/X11 lab session passed unprivileged XDG
autostart, native GIO/GVfs WebDAV file operations and text/RGBA clipboard on
two boots. A later LightDM/Xorg session passed local graphical auto-login,
automatic read-only CD mounting by Tools and two clean ACPI shutdowns.
GNOME/Wayland and installed Windows remain unqualified.
The shipped PE is cross-compiled
for i686, subsystem 5.1, without UCRT imports; static checks and x64 WinPE tests
do not certify XP or another installed Windows release.

Running disk snapshots request a filesystem flush when Tools are configured.
Busy transfers or unavailable/failed Tools refuse the snapshot. This is not
a filesystem freeze or application-consistent snapshot; Windows fixed-volume
flush requires elevation inside the guest.
