$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$personalTarget = Join-Path $projectRoot 'target\personal-build'
$releaseExe = Join-Path $personalTarget 'release\tradecraft.exe'
$personalDir = Join-Path $projectRoot 'target\personal'
$personalExe = Join-Path $personalDir 'tradecraft.exe'
$buildMutex = [Threading.Mutex]::new($false, 'Local\TradeCraft.PersonalBuild')
$hasLock = $false
$stagedExe = $null
$previousExe = $null

Push-Location $projectRoot
try {
    try { $hasLock = $buildMutex.WaitOne([TimeSpan]::FromMinutes(15)) }
    catch [Threading.AbandonedMutexException] { $hasLock = $true }
    if (-not $hasLock) { throw 'Another personal build is still running.' }
    $python = $env:TRADECRAFT_PYTHON
    if (-not $python) {
        $node = Get-Command node -ErrorAction SilentlyContinue
        if ($node) {
            $candidate = Join-Path (Split-Path (Split-Path (Split-Path $node.Source))) 'python\python.exe'
            if (Test-Path -LiteralPath $candidate) { $python = $candidate }
        }
    }
    if (-not $python) { $python = 'python' }
    & $python scripts/license-audit.py --target x86_64-pc-windows-msvc --personal --output (Join-Path $personalDir 'licenses')
    if ($LASTEXITCODE -ne 0) { throw "Personal license audit failed (exit $LASTEXITCODE)." }
    & cargo build --release --locked --features vibestrader-credential --target-dir $personalTarget
    if ($LASTEXITCODE -ne 0) { throw "Personal build failed (exit $LASTEXITCODE)." }
    New-Item -ItemType Directory -Force -Path $personalDir | Out-Null
    $buildHash = (Get-FileHash -LiteralPath $releaseExe -Algorithm SHA256).Hash
    $nonce = [Guid]::NewGuid().ToString('N')
    $stagedExe = Join-Path $personalDir "tradecraft.pending.$nonce.exe"
    Copy-Item -LiteralPath $releaseExe -Destination $stagedExe -ErrorAction Stop
    if ((Get-FileHash -LiteralPath $stagedExe -Algorithm SHA256).Hash -ne $buildHash) {
        throw 'The staged personal executable does not match the build.'
    }
    if ((Test-Path -LiteralPath $personalExe) -and
        ((Get-FileHash -LiteralPath $personalExe -Algorithm SHA256).Hash -eq $buildHash)) {
        Remove-Item -LiteralPath $stagedExe -Force -ErrorAction Stop
        $stagedExe = $null
        Write-Host 'The pinned executable already matches this build.'
    } else { try {
        if (Test-Path -LiteralPath $personalExe) {
            $previousExe = Join-Path $personalDir "tradecraft.previous.$nonce.exe"
            Move-Item -LiteralPath $personalExe -Destination $previousExe -ErrorAction Stop
        }
        Move-Item -LiteralPath $stagedExe -Destination $personalExe -ErrorAction Stop
        $stagedExe = $null
        if ((Get-FileHash -LiteralPath $personalExe -Algorithm SHA256).Hash -ne $buildHash) {
            throw 'The pinned personal executable does not match the build.'
        }
    } catch {
        if ($previousExe -and (Test-Path -LiteralPath $previousExe)) {
            if (Test-Path -LiteralPath $personalExe) {
                $failedExe = Join-Path $personalDir "tradecraft.failed.$nonce.exe"
                Move-Item -LiteralPath $personalExe -Destination $failedExe -ErrorAction Stop
            }
            Move-Item -LiteralPath $previousExe -Destination $personalExe -ErrorAction Stop
            $previousExe = $null
        }
        throw
    } }
    Write-Host "Personal build ready: $personalExe"
    Write-Host "SHA-256: $buildHash"
    Write-Host 'An already running TradeCraft window continues using its previous build until it is fully quit and reopened.'
    if ($previousExe) { Write-Host "Previous executable retained: $previousExe" }
} finally {
    if ($stagedExe -and (Test-Path -LiteralPath $stagedExe)) {
        Remove-Item -LiteralPath $stagedExe -Force -ErrorAction SilentlyContinue
    }
    if ($hasLock) { $buildMutex.ReleaseMutex() }
    $buildMutex.Dispose()
    Pop-Location
}
