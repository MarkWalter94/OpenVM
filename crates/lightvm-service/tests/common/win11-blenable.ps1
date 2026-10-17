# Disposable lab guest only: BitLocker with a TPM protector on C:, then wait
# (at most 5 minutes) until encryption has visibly started.
$ErrorActionPreference = 'Continue'
Get-Tpm | Format-List TpmPresent, TpmReady, TpmEnabled, TpmActivated, TpmOwned
Enable-BitLocker -MountPoint C: -TpmProtector -UsedSpaceOnly -SkipHardwareTest -EncryptionMethod XtsAes128 | Format-List VolumeStatus, KeyProtector
$t = Get-Date
do {
  Start-Sleep 5
  $v = Get-BitLockerVolume -MountPoint C:
} while ($v.EncryptionPercentage -lt 1 -and $v.VolumeStatus -ne 'FullyEncrypted' -and ((Get-Date) - $t).TotalSeconds -lt 300)
"waited {0:N0} s" -f ((Get-Date) - $t).TotalSeconds
