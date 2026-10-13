# Signs the given files with the "ahsoggg" certificate (Authenticode, SHA-256, RFC 3161 timestamp).
#
#   powershell -ExecutionPolicy Bypass -File scripts\sign.ps1 dist\SpaceTree.exe dist\spacetree-cli.exe
#
# Run scripts\make_cert.ps1 first. The timestamp keeps signatures valid after the certificate expires;
# if the timestamp server is unreachable the file is signed without one (with a warning).
param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Files)

$ErrorActionPreference = "Stop"
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object { $_.Subject -eq "CN=ahsoggg" -and $_.NotAfter -gt (Get-Date) } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $cert) { throw "certificate CN=ahsoggg not found - run scripts\make_cert.ps1" }

foreach ($f in $Files) {
    $sig = $null
    foreach ($ts in @("http://timestamp.digicert.com", "http://timestamp.sectigo.com", $null)) {
        try {
            if ($ts) {
                $sig = Set-AuthenticodeSignature -FilePath $f -Certificate $cert -HashAlgorithm SHA256 -TimestampServer $ts
            } else {
                Write-Warning "no timestamp server reachable; signing $f without a timestamp"
                $sig = Set-AuthenticodeSignature -FilePath $f -Certificate $cert -HashAlgorithm SHA256
            }
            break
        } catch { Write-Warning "timestamp server $ts failed: $($_.Exception.Message)" }
    }
    if (-not $sig -or -not $sig.SignerCertificate) { throw "signing $f failed" }
    # 'UnknownError' / 'NotTrusted' is expected for a self-signed certificate that is not in the Trusted Root store.
    Write-Host ("{0}: signed by {1} ({2})" -f $f, $sig.SignerCertificate.Subject, $sig.Status)
}
