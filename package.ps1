# Builds a release and packs it into dist\ (RustKnob.exe + README + zip) to share.
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$env:Path += ";$env:USERPROFILE\.cargo\bin"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\rustknob.exe dist\RustKnob.exe -Force
Copy-Item README.md dist\README.md -Force
Compress-Archive -Path dist\RustKnob.exe, dist\README.md -DestinationPath dist\RustKnob.zip -Force

Get-ChildItem dist | Select-Object Name, @{n='KB'; e={[math]::Round($_.Length / 1KB)}}
