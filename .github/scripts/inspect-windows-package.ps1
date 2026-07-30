param(
    [Parameter(Mandatory = $true)]
    [string]$BundleDirectory,
    [Parameter(Mandatory = $true)]
    [string]$StageDirectory,
    [Parameter(Mandatory = $true)]
    [string]$Target
)

$ErrorActionPreference = 'Stop'
$installer = Get-ChildItem -LiteralPath $BundleDirectory -Recurse -File |
    Where-Object { $_.Name -like '*setup.exe' } |
    Select-Object -First 1
if ($null -eq $installer) {
    throw "NSIS installer was not found below $BundleDirectory"
}

$inspectionRoot = Join-Path $env:RUNNER_TEMP "inspect-$Target"
if (Test-Path -LiteralPath $inspectionRoot) {
    Remove-Item -LiteralPath $inspectionRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $inspectionRoot | Out-Null
& 7z x $installer.FullName "-o$inspectionRoot" -y | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "7-Zip could not inspect $($installer.FullName)"
}

$inventory = Get-Content -LiteralPath (Join-Path $StageDirectory 'SHA256SUMS')
foreach ($tool in @('yt-dlp', 'ffmpeg', 'ffprobe', 'deno')) {
    $qualified = "$tool-$Target.exe"
    $expectedLine = $inventory | Where-Object { $_ -match [regex]::Escape("  $qualified") }
    if ($null -eq $expectedLine) {
        throw "Staged checksum inventory omitted $qualified"
    }
    $expected = ($expectedLine -split '\s{2}', 2)[0]
    $packaged = Get-ChildItem -LiteralPath $inspectionRoot -Recurse -File -Filter "$tool.exe"
    if ($packaged.Count -ne 1) {
        throw "Expected one packaged $tool.exe, found $($packaged.Count)"
    }
    $found = (Get-FileHash -LiteralPath $packaged[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($found -ne $expected) {
        throw "Packaged $tool.exe digest $found differs from staged digest $expected"
    }
}

$forbidden = Get-ChildItem -LiteralPath $inspectionRoot -Recurse -Force |
    Where-Object {
        $_.Name -match '(\.cache|\.part|\.tmp)$' -or
        $_.FullName -match '[\\/](staging|updates?|cache)[\\/]'
    }
if ($forbidden.Count -ne 0) {
    throw "Installer contains cache/update artifacts: $($forbidden.FullName -join ', ')"
}

$size = $installer.Length
$digest = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "validated_desktop_artifact=$($installer.Name) bytes=$size sha256=$digest"
