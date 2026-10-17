# Windows TPM helper (ADR-0035)

`build.sh <work> <new-empty-output>` runs inside a dedicated, user-local
Cygwin installation. It verifies both v0.10.2 source archive hashes and
Stefan Berger's signature before building libtpms and swtpm. No driver,
administrator privilege or host TPM access is required. Inputs are the
GitHub tag archives and detached release signatures, renamed
`libtpms.tar.gz[.asc]` and `swtpm.tar.gz[.asc]`.

Required Cygwin packages include gcc-core, gcc-g++, make, autoconf, automake,
libtool, pkg-config, libglib2.0-devel, libjson-glib1.0-devel, libssl-devel,
libgnutls-devel, gnutls (certtool), libtasn1-devel, git, patch, wget and gnupg2.
The recipe disables upstream tests; runtime/product checks are separate.

The output contains swtpm.exe, libtpms and the full transitive Cygwin DLL
closure, source licences and SHA256SUMS. Assemble it with the QEMU and
firmware outputs using `../qemu-win11/assemble.ps1`. swtpm is dynamically
linked, runs in the same Windows Job Object as its VM and uses per-VM state.
Windows uses two loopback channels; listener and connected peer PIDs are
checked using the kernel TCP table before guest execution. These checks do
not turn swtpm's protocol into an authenticated transport.

`collect-licenses.sh <swtpm-out> <setup.ini> <dest>` (the same Cygwin, with
the `setup.ini` that installed it) maps every Cygwin DLL of the output to its
package and version (`cygcheck -f`, `installed.db`), downloads the Cygwin
source package of that version into `<dest>/sources`, checks its SHA-512
against `setup.ini` (which Cygwin setup verified with its signature), and
copies the package's licence docs, or the licence files of the upstream
archive in the source package. libtpms and swtpm themselves are covered by
`LICENSE-libtpms`/`LICENSE-swtpm`.

Distribution still requires the applicable LGPL source/relinking
provisions and signing, as inventoried in
`docs/verification/R6-05-windows11-x64-swtpm.md`. This development bundle is
not release-qualified.
