# LAB ONLY: native x64 guest probes for disposable Windows PE. Never execute
# TPMTEST.EXE against the host TPM. The shipped Tools remain the XP/i686 build.
param(
  [Parameter(Mandatory)][string]$Cc,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference='Stop'
$root=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
if ((& $Cc -dumpmachine) -notmatch '^x86_64-') { throw 'Native x64 MinGW GCC required' }
if (Test-Path -LiteralPath $Out) { throw 'Output must be a new lab directory' }
New-Item -ItemType Directory -Path $Out | Out-Null
$Out=(Resolve-Path -LiteralPath $Out).Path
$flags=@('-Wall','-Wextra','-Werror','-Wno-misleading-indentation','-Os','-ffreestanding','-fno-builtin','-fno-stack-protector','-D_WIN32_WINNT=0x0501','-DWINVER=0x0501','-nostdlib','-Wl,--entry,mainCRTStartup')
foreach ($item in @(
  @('TOOLS64.EXE','apps/guest-tools/windows/tools.c','windows:6.0',$true),
  @('CLIPTEST.EXE','apps/guest-tools/windows/test_clipboard.c','console:6.0',$true),
  @('TPMTEST.EXE','crates/lightvm-service/tests/common/tpm-probe.c','console:6.0',$false)
)) {
  $sources=@((Join-Path $root $item[1]))
  if ($item[3]) { $sources+=Join-Path $root 'apps/guest-tools/windows/json.c' }
  & $Cc @flags "-Wl,--subsystem,$($item[2])" @sources -o (Join-Path $Out $item[0]) -lkernel32 -lws2_32 -lole32 -loleaut32 -luuid -luser32 -ladvapi32 -lshell32 -lmpr -lgcc
  if ($LASTEXITCODE -ne 0) { throw "Lab probe compilation failed: $($item[0])" }
}
Get-ChildItem -LiteralPath $Out -File | Get-FileHash -Algorithm SHA256
