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
if exist C:\LightVMLab\warm-reboot-done (echo %date% %time% after warm reboot>>"%OUT%\lab.log" & exit /b 0)
rem Leave enough time for the host to retrieve the first authenticated log.
rem (timeout.exe needs a console, which RunAsynchronous does not have.)
ping -n 91 127.0.0.1 >nul
echo requested>C:\LightVMLab\warm-reboot-done
echo %date% %time% shutdown /r>>"%OUT%\lab.log"
shutdown.exe /r /t 0 /f >>"%OUT%\lab.log" 2>&1
echo %date% %time% shutdown rc=%errorlevel%>>"%OUT%\lab.log"
