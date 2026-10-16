@echo off
rem Runs only in the disposable installed guest, under its audit account.
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders" /v Personal /t REG_SZ /d C:\LightVMGuestDocs /f
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders" /v Personal /t REG_EXPAND_SZ /d C:\LightVMGuestDocs /f
mkdir "C:\LightVMGuestDocs\LightVM Tools\outbox" 2>nul
sc start tbs
"C:\LightVMLab\TPMTEST.EXE" >"C:\LightVMGuestDocs\LightVM Tools\outbox\tpm.log" 2>&1
powershell.exe -NoProfile -Command "$os=Get-CimInstance Win32_OperatingSystem; $os | Select Caption,Version,LastBootUpTime | Format-List; Get-Tpm | Format-List; Confirm-SecureBootUEFI" >"C:\LightVMGuestDocs\LightVM Tools\outbox\installed.log" 2>&1
start "" /b C:\LightVMLab\TOOLS.EXE
if exist C:\LightVMLab\warm-reboot-done exit /b 0
rem Leave enough time for the host to retrieve the first authenticated log.
timeout /t 90 /nobreak >nul
echo requested>C:\LightVMLab\warm-reboot-done
shutdown.exe /r /t 0
