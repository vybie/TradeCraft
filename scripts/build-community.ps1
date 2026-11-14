$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    $python = $env:TRADECRAFT_PYTHON
    if (-not $python) {
        $node = Get-Command node -ErrorAction SilentlyContinue
        if ($node) {
            $candidate = Join-Path (Split-Path (Split-Path (Split-Path $node.Source))) 'python\python.exe'
            if (Test-Path -LiteralPath $candidate) { $python = $candidate }
        }
    }
    if (-not $python) { $python = 'python' }
    & $python scripts/license-audit.py --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "Community license audit failed (exit $LASTEXITCODE)." }
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "Community build failed (exit $LASTEXITCODE)." }
    & $python scripts/package-community.py
    if ($LASTEXITCODE -ne 0) { throw "Community packaging failed (exit $LASTEXITCODE)." }
} finally {
    Pop-Location
}
