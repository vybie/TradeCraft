$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$releaseExe = Join-Path $projectRoot 'target\release\tradecraft.exe'
$personalDir = Join-Path $projectRoot 'target\personal'
$personalExe = Join-Path $personalDir 'tradecraft.exe'

Push-Location $projectRoot
try {
    & cargo build --release --features vibestrader-credential
    if ($LASTEXITCODE -ne 0) { throw "Personal build failed (exit $LASTEXITCODE)." }
    New-Item -ItemType Directory -Force -Path $personalDir | Out-Null
    Copy-Item -LiteralPath $releaseExe -Destination $personalExe -Force
    if ((Get-FileHash -LiteralPath $releaseExe).Hash -ne (Get-FileHash -LiteralPath $personalExe).Hash) {
        throw 'The personal executable does not match the build.'
    }
    Write-Host "Personal build ready: $personalExe"
} finally {
    Pop-Location
}
