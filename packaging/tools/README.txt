LightVM Tools (experimental)

Enable Tools per VM in Edit. Start the VM and open its LIGHTVM_TOOLS CD:
Windows: run INSTALL.CMD as the guest user. The installer configures per-user
startup. An administrator can additionally enable the guest WebClient service
and its large-file limit. No host driver is installed.
Linux: run sh INSTALL.SH in the graphical guest session. Python 3 is required;
clipboard needs wl-clipboard or xclip, accessibility needs python3-pyatspi.

Named shared folders are available through guest-loopback WebDAV:
http://127.0.0.1:47081/<ShareName>/. Windows attempts drive mapping. Linux can
open dav://127.0.0.1:47081/<ShareName>/ in a WebDAV-capable file manager.
The host enforces read-only/write scope independently of guest mappings.
The WebDAV compatibility layer does not yet enforce exclusive file locks.

Console clipboard buttons perform one explicit text copy in each direction,
only while the console window has focus. Automatic synchronization, images,
file drag/drop and USB redirection are not available in this build.

Agent sessions require separate approval for every integration. UI Automation
must be installed in Windows; older systems can return unsupported. Password
values are masked. No shell or host path is exposed to agents.

Files are streamed with exact size/SHA-256 validation and create-only guest
inbox publication; host share writes can explicitly replace a file. Interrupted
writes are cancelled on channel disconnect. A private supervisor journal records host staging writes. On the next Tools
start it removes recorded staging files; final destinations are preserved.
Corrupt records or links stop recovery. Host power-loss durability is unqualified.

Windows runtime and real guest interoperability have not been qualified on
this Mac. The PE is cross-compiled for i686, subsystem 5.1, without UCRT imports;
those static checks do not certify XP or any other Windows release.
