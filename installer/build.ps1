# Builds the release binaries and the UPS-BMC installer.
# Output: target\installer\UPS-BMC-setup-<version>.exe
# Requires Inno Setup 6 (set $env:ISCC to override the ISCC.exe path).

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release --locked -p ups-bmc -p ups-collector -p hid-probe
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $meta = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $version = ($meta.packages | Where-Object name -eq 'ups-bmc').version

    $iscc = $env:ISCC
    if (-not $iscc) {
        $iscc = @(
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if (-not $iscc) { throw "Inno Setup 6 (ISCC.exe) not found; install it or set `$env:ISCC" }

    & $iscc "/DAppVersion=$version" "$PSScriptRoot\ups-bmc.iss"
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
    Get-Item "$root\target\installer\UPS-BMC-setup-$version.exe"
}
finally {
    Pop-Location
}
