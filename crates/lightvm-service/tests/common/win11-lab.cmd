@echo off
rem Runs only in the disposable installed guest, under its audit account.
rem auditUser starts it at the first audit logon; the Run key starts the copy
rem on the current run's CD again after every later boot (warm reboot).
setlocal
set OUT=C:\LightVMGuestDocs\LightVM Tools\outbox
mkdir "%OUT%" 2>nul
tasklist /fi "imagename eq TOOLS.EXE" | find /i "TOOLS.EXE" >nul && (echo %date% %time% tools already running>>"%OUT%\lab.log" & exit /b 0)
echo %date% %time% lab start>>"%OUT%\lab.log"
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v LightVMLab /t REG_SZ /d "cmd.exe /c for %%d in (D E F G H I J K L M N O P Q R S T U V W Y Z) do @if exist %%d:\LAB.CMD (%%d:\LAB.CMD & exit)" /f >>"%OUT%\lab.log" 2>&1
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders" /v Personal /t REG_SZ /d C:\LightVMGuestDocs /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders" /v Personal /t REG_EXPAND_SZ /d C:\LightVMGuestDocs /f >nul
sc start tbs >nul 2>&1
"C:\LightVMLab\TPMTEST.EXE" >"%OUT%\tpm.log" 2>&1
powershell.exe -NoProfile -Command "$os=Get-CimInstance Win32_OperatingSystem; $os | Select Caption,Version,LastBootUpTime | Format-List; Get-Tpm | Format-List; Confirm-SecureBootUEFI" >"%OUT%\installed.log" 2>&1
start "" /b C:\LightVMLab\TOOLS.EXE
echo %date% %time% tools started>>"%OUT%\lab.log"
if not exist C:\LightVMLab\warm-reboot-done (
  rem Leave enough time for the host to retrieve the first authenticated log.
  rem (timeout.exe needs a console, which RunAsynchronous does not have.)
  ping -n 91 127.0.0.1 >nul
  echo requested>C:\LightVMLab\warm-reboot-done
  echo %date% %time% shutdown /r>>"%OUT%\lab.log"
  shutdown.exe /r /t 0 /f >>"%OUT%\lab.log" 2>&1
  exit /b 0
)
echo %date% %time% after warm reboot>>"%OUT%\lab.log"
rem BitLocker variant only (BLENABLE.PS1 on the CD): TPM protector on C:,
rem reboot once encryption started, then record the state after the boot.
if not exist "%~dp0BLENABLE.PS1" exit /b 0
rem BitLocker refuses while bootable Windows media is attached (0x80310030).
rem Windows reboots itself; the host starts that boot without the ISO (a
rem host reset would stop the audit-mode logon of this lab guest).
for %%d in (D E F G H I J K L M N O P Q R S T U V W Y Z) do if exist %%d:\sources\boot.wim (echo %date% %time% installation media attached, rebooting>>"%OUT%\lab.log" & ping -n 31 127.0.0.1 >nul & shutdown.exe /r /t 0 /f & exit /b 0)
if exist C:\LightVMLab\bitlocker-on (
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0BLSTATE.PS1" >"%OUT%\bitlocker-after.log" 2>&1
  echo done>"%OUT%\bitlocker-done.log"
  echo %date% %time% bitlocker state after reboot>>"%OUT%\lab.log"
  exit /b 0
)
ping -n 31 127.0.0.1 >nul
echo %date% %time% enabling bitlocker>>"%OUT%\lab.log"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0BLENABLE.PS1" >"%OUT%\bitlocker-enable.log" 2>&1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0BLSTATE.PS1" >"%OUT%\bitlocker-before.log" 2>&1
echo on>C:\LightVMLab\bitlocker-on
echo %date% %time% bitlocker reboot>>"%OUT%\lab.log"
ping -n 31 127.0.0.1 >nul
shutdown.exe /r /t 0 /f >>"%OUT%\lab.log" 2>&1
