# Disposable lab guest only: BitLocker state of C: and the boot time.
Get-BitLockerVolume -MountPoint C: | Format-List MountPoint, VolumeStatus, ProtectionStatus, LockStatus, EncryptionPercentage, KeyProtector
"LastBootUpTime : {0}" -f (Get-CimInstance Win32_OperatingSystem).LastBootUpTime
