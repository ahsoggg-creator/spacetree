# Builds the release artifacts into dist\ and (with -Publish) creates a GitHub release.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1            # build only
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Publish   # build + gh release create
#
# Assets: SpaceTree-Setup.exe (installer), SpaceTree.exe (portable; also what the auto-updater downloads),
# SpaceTree.exe.sha256 (checked by the updater), spacetree-cli.exe.
param([switch]$Publish, [string]$NotesFile = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path crates\app\Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$tag = "v$version"
Write-Host "SpaceTree $tag"

cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "tests failed" }
cargo build --release -p spacetree-app -p spacetree
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$dist = Join-Path $root "dist"
if (Test-Path $dist) { Remove-Item -LiteralPath $dist -Recurse -Force }
New-Item -ItemType Directory $dist | Out-Null
Copy-Item target\release\SpaceTree.exe (Join-Path $dist "SpaceTree.exe")
Copy-Item target\release\SpaceTree.exe (Join-Path $dist "SpaceTree-Setup.exe")
Copy-Item target\release\spacetree-cli.exe (Join-Path $dist "spacetree-cli.exe")
$hash = (Get-FileHash (Join-Path $dist "SpaceTree.exe") -Algorithm SHA256).Hash.ToLower()
Set-Content -LiteralPath (Join-Path $dist "SpaceTree.exe.sha256") -Value "$hash  SpaceTree.exe" -NoNewline -Encoding ascii
Get-ChildItem $dist | Format-Table Name, Length

if ($Publish) {
    $assets = Get-ChildItem $dist | ForEach-Object { $_.FullName }
    if ($NotesFile -ne "") {
        gh release create $tag @assets --title "SpaceTree $version" --notes-file $NotesFile
    } else {
        gh release create $tag @assets --title "SpaceTree $version" --generate-notes
    }
    if ($LASTEXITCODE -ne 0) { throw "gh release failed" }
}
