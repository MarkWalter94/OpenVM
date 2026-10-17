# Disposable lab guest only: BitLocker with a TPM protector on C:, then wait
# (at most 80 minutes) until the volume is fully encrypted and protection is
# on. While encryption runs, protection is off and the volume key is kept in
# clear: only after this does a boot depend on the TPM.
$ErrorActionPreference = 'Continue'
Get-Tpm | Format-List TpmPresent, TpmReady, TpmEnabled, TpmActivated, TpmOwned
Enable-BitLocker -MountPoint C: -TpmProtector -UsedSpaceOnly -SkipHardwareTest -EncryptionMethod XtsAes128 | Format-List VolumeStatus, KeyProtector
# A recovery password as well: without the right TPM, boot then stops at the
# BitLocker recovery screen (with the TPM protector alone there is nothing to
# ask for). The password stays inside this disposable guest.
Add-BitLockerKeyProtector -MountPoint C: -RecoveryPasswordProtector | Out-Null
$t = Get-Date
do {
  Start-Sleep 15
  $v = Get-BitLockerVolume -MountPoint C:
  # With the extra protector, protection stays suspended after encryption:
  # turn it on (the clear key is removed).
  if ($v.VolumeStatus -eq 'FullyEncrypted' -and $v.ProtectionStatus -ne 'On') {
    Resume-BitLocker -MountPoint C: | Out-Null
    $v = Get-BitLockerVolume -MountPoint C:
  }
} while (($v.VolumeStatus -ne 'FullyEncrypted' -or $v.ProtectionStatus -ne 'On') -and ((Get-Date) - $t).TotalMinutes -lt 80)
"waited {0:N0} s" -f ((Get-Date) - $t).TotalSeconds
