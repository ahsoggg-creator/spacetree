# Creates (once) the self-signed code-signing certificate "ahsoggg" in the current user's certificate store
# and exports its PUBLIC part to certs\ahsoggg.cer (safe to commit). The private key never leaves the store
# unless you ask for a backup:
#
#   powershell -ExecutionPolicy Bypass -File scripts\make_cert.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\make_cert.ps1 -BackupPfx D:\safe\ahsoggg.pfx   # asks for a password
#
# A self-signed certificate proves "signed by the same person as before", not "trusted by Windows":
# SmartScreen still warns until the certificate is trusted on that machine (scripts\trust_cert.ps1).
param([string]$BackupPfx = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$subject = "CN=ahsoggg"

$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object { $_.Subject -eq $subject -and $_.NotAfter -gt (Get-Date).AddDays(30) } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject `
        -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 `
        -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(5) `
        -CertStoreLocation Cert:\CurrentUser\My
    Write-Host "Created certificate $($cert.Thumbprint)"
} else {
    Write-Host "Using existing certificate $($cert.Thumbprint)"
}

New-Item -ItemType Directory -Force (Join-Path $root "certs") | Out-Null
$cer = Join-Path $root "certs\ahsoggg.cer"
Export-Certificate -Cert $cert -FilePath $cer | Out-Null
Write-Host "Public certificate: $cer"
Write-Host "SHA-1 thumbprint:   $($cert.Thumbprint)"
Write-Host ("SHA-256 fingerprint: " + (Get-FileHash $cer -Algorithm SHA256).Hash)

if ($BackupPfx -ne "") {
    $pw = Read-Host "Password for the .pfx backup" -AsSecureString
    Export-PfxCertificate -Cert $cert -FilePath $BackupPfx -Password $pw | Out-Null
    Write-Host "Private key backup: $BackupPfx (keep it secret, never commit it)"
}
