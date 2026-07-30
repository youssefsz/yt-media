$ErrorActionPreference = 'Stop'

function Measure-Fixture {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    $started = [DateTimeOffset]::UtcNow
    $process = Start-Process -FilePath 'cargo.exe' -ArgumentList $Arguments -NoNewWindow -PassThru
    $null = $process.Handle
    $maximumMemory = 0L
    while (-not $process.HasExited) {
        $process.Refresh()
        $maximumMemory = [Math]::Max($maximumMemory, [int64]$process.WorkingSet64)
        Start-Sleep -Milliseconds 50
    }
    $process.WaitForExit()
    $process.Refresh()
    if ($process.ExitCode -ne 0) {
        throw "Controlled fixture failed with exit code $($process.ExitCode)"
    }
    return @{
        Milliseconds = [int64]([DateTimeOffset]::UtcNow - $started).TotalMilliseconds
        MaximumMemory = $maximumMemory
    }
}

$analysis = Measure-Fixture @(
    'test',
    '-p', 'yt-media-engine',
    '--all-features',
    '--lib',
    'analysis::ytdlp::tests::adaptive_fixture_has_unique_descending_heights_and_merge',
    '--', '--exact'
)
$download = Measure-Fixture @(
    'test',
    '-p', 'yt-media-engine',
    '--all-features',
    '--test', 'download_job'
)

"YT_MEDIA_ANALYSIS_MS=$($analysis.Milliseconds)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"YT_MEDIA_ACTIVE_MEMORY_BYTES=$($download.MaximumMemory)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
Write-Output "fixture_analysis_ms=$($analysis.Milliseconds) active_download_memory_bytes=$($download.MaximumMemory) download_fixture_ms=$($download.Milliseconds)"
