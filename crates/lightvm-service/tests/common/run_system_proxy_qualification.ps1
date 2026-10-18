param(
    [Parameter(Mandatory)][string]$HostProbe,
    [Parameter(Mandatory)][string]$TestExecutable,
    [Parameter(Mandatory)][string]$PythonExecutable,
    [Parameter(Mandatory)][string]$EvidenceDirectory
)
$ErrorActionPreference='Stop'
# Temporarily configure a current-user PAC for example.com ONLY, then restore
# the exact original flags and AutoConfigURL presence/value in finally.
# Refuse an existing explicit proxy/PAC rather than replacing user routing.
$key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Internet Settings',$true)
if(-not $key){throw 'Cannot open current-user proxy key'}
$originalExists=$key.GetValueNames() -contains 'AutoConfigURL'
$originalUrl=$key.GetValue('AutoConfigURL')
$originalKind=if($originalExists){$key.GetValueKind('AutoConfigURL')}else{$null}
if($key.GetValue('ProxyEnable',0) -ne 0 -or -not [string]::IsNullOrEmpty($originalUrl)){
    throw 'This qualification requires no existing explicit proxy or PAC.'
}
$before=@(& $HostProbe)
if($LASTEXITCODE -ne 0){throw 'Cannot read native Windows proxy configuration'}
$flagsText=& $HostProbe --flags
if($LASTEXITCODE -ne 0){throw 'Cannot read original connection flags'}
$originalFlags=[uint32]$flagsText
if(($originalFlags -band 2) -ne 0 -or ($originalFlags -band 1) -eq 0){throw 'Unsupported original proxy flags'}
$sentinel=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$server=$null
$changed=$false
try {
    $sentinel.Start()
    $proxyPort=([Net.IPEndPoint]$sentinel.LocalEndpoint).Port
    $stdout=Join-Path $EvidenceDirectory 'pac-server.stdout.log'
    $stderr=Join-Path $EvidenceDirectory 'pac-server.stderr.log'
    $server=Start-Process -FilePath $PythonExecutable -ArgumentList @('-u',('"{0}"' -f (Join-Path $PSScriptRoot 'host_proxy_pac.py')),[string]$proxyPort) -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $deadline=[DateTime]::UtcNow.AddSeconds(10)
    do {
        if($server.HasExited){throw 'PAC fixture exited before ready'}
        $ready=Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Select-Object -First 1
        if($ready){break}
        if([DateTime]::UtcNow -gt $deadline){throw 'PAC fixture ready timeout'}
        Start-Sleep -Milliseconds 100
    } while($true)
    $pacPort=($ready | ConvertFrom-Json).port
    $url="http://127.0.0.1:$pacPort/lightvm.pac"
    $changed=$true
    & $HostProbe --pac $url ([string]($originalFlags -bor 4))
    if($LASTEXITCODE -ne 0){throw 'Cannot enable temporary current-user PAC'}
    $canary=@(& $HostProbe 'https://example.com/' "127.0.0.1:$proxyPort")
    $canary | Set-Content -Encoding utf8 (Join-Path $EvidenceDirectory 'system-proxy-canary.log')
    if($LASTEXITCODE -ne 0){throw ('Windows did not select the configured PAC proxy: '+($canary -join '; '))}
    & $TestExecutable 'filtered_windows::winpe_filtered_https_and_forbidden_destinations' --ignored --exact --nocapture --test-threads=1 2>&1 | Tee-Object (Join-Path $EvidenceDirectory 'winpe-system-proxy-console.log')
    $testExit=$LASTEXITCODE
    $consulted=$sentinel.Pending()
    [ordered]@{test_exit=$testExit;configured_proxy_verified=$true;proxy_consulted_by_gateway=$consulted;proxy_port=$proxyPort;scope='current-user Windows PAC; only example.com proxied; other names DIRECT';original_flags=$originalFlags} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $EvidenceDirectory 'system-proxy-result.json')
    if($testExit -ne 0 -or $consulted){throw 'Gateway used proxy or WinPE qualification failed'}
} finally {
    try {
        if($changed){
            & $HostProbe --pac '-' ([string]$originalFlags)
            if($LASTEXITCODE -ne 0){throw 'Failed to restore original proxy flags'}
            if($originalExists){$key.SetValue('AutoConfigURL',$originalUrl,$originalKind)}
            elseif($key.GetValueNames() -contains 'AutoConfigURL') {$key.DeleteValue('AutoConfigURL',$false)}
            $after=@(& $HostProbe)
            $restoredFlags=[uint32](& $HostProbe --flags)
            $restored=($restoredFlags -eq $originalFlags) -and (($before -join "`n") -eq ($after -join "`n")) -and (($key.GetValueNames() -contains 'AutoConfigURL') -eq $originalExists)
            [ordered]@{restored=$restored;flags=$restoredFlags;auto_config_url_presence_restored=(($key.GetValueNames() -contains 'AutoConfigURL') -eq $originalExists)} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $EvidenceDirectory 'system-proxy-restored.json')
            if(-not $restored){throw 'Proxy restoration verification failed'}
        }
    } finally {
        $sentinel.Stop()
        if($server -and -not $server.HasExited){$server.Kill();$server.WaitForExit()}
        $key.Dispose()
    }
}
