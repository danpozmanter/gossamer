# Cross-build the canonical fixture list for a Linux target. The list includes
# the independent Stable all-tier contract fixtures, so cross AOT validation
# cannot drift from native VM/JIT/AOT conformance.
# Usage: scripts/cross_build_fixtures.ps1 <target-triple> <out-base>
#
# Binaries land in <out-base>/<target-triple>/ so several targets share
# one upload artifact. Windows host cross CI job: produces the binaries,
# uploads them, and a Linux job runs them (natively for x86_64, under
# QEMU for aarch64) and diffs against the bytecode VM. The runtime
# archive comes from the GOS_RUNTIME_LIB_<TRIPLE> env var set by the
# calling job. The OS-crossing ELF link uses rust-lld (shipped with the
# rust toolchain), selected by `gos` for the Windows-host case.
param(
    [Parameter(Mandatory = $true)][string]$Triple,
    [Parameter(Mandatory = $true)][string]$OutBase
)
$ErrorActionPreference = "Stop"
$gos = if ($env:GOS_BIN) { $env:GOS_BIN } else { ".\target\debug\gos.exe" }
$root = (Resolve-Path "$PSScriptRoot\..").Path
$out = Join-Path $OutBase $Triple

New-Item -ItemType Directory -Force -Path $out | Out-Null
foreach ($src in Get-Content "$root\scripts\cross_fixtures.txt") {
    $src = $src.Trim()
    if ($src -eq "" -or $src.StartsWith("#")) { continue }
    Write-Host "cross-build $src -> $Triple"
    & $gos build --release --target $Triple "$root\$src" --out-dir $out
    if ($LASTEXITCODE -ne 0) { throw "cross build failed for $src" }
}
