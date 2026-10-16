@echo off
rem Test guest only: no install, disk partitioning or guest-requirement bypass.
wpeutil InitializeNetwork
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders" /v Personal /t REG_EXPAND_SZ /d X:\LightVMGuestDocs /f
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders" /v Personal /t REG_SZ /d X:\LightVMGuestDocs /f
mkdir "X:\LightVMGuestDocs\LightVM Tools\outbox"
"%~dp0CLIPTEST.EXE" >"X:\LightVMGuestDocs\LightVM Tools\outbox\native.log" 2>&1
start "" /b "%~dp0TOOLS.EXE"
exit /b 0
