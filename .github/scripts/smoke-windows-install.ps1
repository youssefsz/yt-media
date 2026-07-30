param(
    [Parameter(Mandatory = $true)]
    [string]$BundleDirectory,
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

$installRoot = Join-Path $env:RUNNER_TEMP "installed-$Target"
$dataRoot = Join-Path $env:LOCALAPPDATA 'dev.ytmedia.desktop'
foreach ($path in @($installRoot, $dataRoot)) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Recurse -Force
    }
    New-Item -ItemType Directory -Path $path | Out-Null
}

$install = Start-Process -FilePath $installer.FullName -ArgumentList @('/S', "/D=$installRoot") -Wait -PassThru
if ($install.ExitCode -ne 0) {
    throw "Silent NSIS install failed with exit code $($install.ExitCode)"
}
$application = Get-ChildItem -LiteralPath $installRoot -Recurse -File -Filter 'yt-media-app.exe' |
    Select-Object -First 1
if ($null -eq $application) {
    throw "Installed application executable was not found below $installRoot"
}

$env:HTTP_PROXY = 'http://127.0.0.1:9'
$env:HTTPS_PROXY = 'http://127.0.0.1:9'
$env:ALL_PROXY = 'http://127.0.0.1:9'
$env:NO_PROXY = ''

$toolProbes = @(
    @{ Name = 'yt-dlp.exe'; Arguments = @('--no-update', '--version') },
    @{ Name = 'ffmpeg.exe'; Arguments = @('-hide_banner', '-version') },
    @{ Name = 'ffprobe.exe'; Arguments = @('-hide_banner', '-version') },
    @{ Name = 'deno.exe'; Arguments = @('--version') }
)
foreach ($probe in $toolProbes) {
    $matches = @(Get-ChildItem -LiteralPath $installRoot -Recurse -File -Filter $probe.Name)
    if ($matches.Count -ne 1) {
        throw "Expected exactly one installed $($probe.Name), found $($matches.Count)"
    }
    [string[]]$probeArguments = $probe.Arguments
    $output = & $matches[0].FullName @probeArguments 2>&1
    if ($LASTEXITCODE -ne 0 -or $null -eq $output) {
        throw "Installed $($probe.Name) could not report its version offline."
    }
    $firstLine = @($output)[0].ToString()
    Write-Output "offline_bundled_tool=$($probe.Name) version=$firstLine"
}

$started = [DateTimeOffset]::UtcNow
$process = Start-Process -FilePath $application.FullName -PassThru
$database = $null
for ($attempt = 0; $attempt -lt 30; $attempt += 1) {
    Start-Sleep -Milliseconds 500
    $database = Get-ChildItem -LiteralPath $dataRoot -Recurse -File -Filter 'jobs.sqlite3' -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $database) {
        break
    }
    if ($process.HasExited) {
        throw "Installed application exited during offline startup with code $($process.ExitCode)"
    }
}
if ($null -eq $database) {
    throw 'Offline first launch did not initialize durable application data.'
}
$coldStartMs = [int64]([DateTimeOffset]::UtcNow - $started).TotalMilliseconds
$process.Refresh()
$idleMemory = [int64]$process.WorkingSet64
Stop-Process -Id $process.Id -Force
$process.WaitForExit()

$installedBytes = (Get-ChildItem -LiteralPath $installRoot -Recurse -File |
    Measure-Object -Property Length -Sum).Sum
$uninstaller = Get-ChildItem -LiteralPath $installRoot -Recurse -File |
    Where-Object { $_.Name -match '^uninstall.*\.exe$' } |
    Select-Object -First 1
if ($null -eq $uninstaller) {
    throw 'NSIS uninstaller was not installed.'
}
$uninstall = Start-Process -FilePath $uninstaller.FullName -ArgumentList '/S' -Wait -PassThru
if ($uninstall.ExitCode -ne 0) {
    throw "Silent NSIS uninstall failed with exit code $($uninstall.ExitCode)"
}
if (Test-Path -LiteralPath $application.FullName) {
    throw 'Application executable remained after uninstall.'
}
if (-not (Test-Path -LiteralPath $database.FullName)) {
    throw 'Uninstall removed durable user data without explicit user authorization.'
}

"YT_MEDIA_INSTALLED_BYTES=$installedBytes" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"YT_MEDIA_COLD_START_MS=$coldStartMs" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"YT_MEDIA_IDLE_MEMORY_BYTES=$idleMemory" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
Write-Output "offline_startup_ms=$coldStartMs idle_memory_bytes=$idleMemory installed_bytes=$installedBytes persistence=preserved uninstall=passed"
