$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$startedAt = [System.Diagnostics.Stopwatch]::StartNew()
$stamp = Join-Path ([System.IO.Path]::GetTempPath()) ('vesper-e2e-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $stamp | Out-Null
$serverProcess = $null

function Run-Vesper([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments) {
    & $vesper @Arguments
    if ($LASTEXITCODE -ne 0) { throw "vesper failed ($LASTEXITCODE): $($Arguments -join ' ')" }
}

function Tree-Bytes([string]$Path) {
    if (-not (Test-Path $Path)) { return [int64]0 }
    $sum = (Get-ChildItem -LiteralPath $Path -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return [int64]0 }
    return [int64]$sum
}

function Http-Bytes([string]$Base) {
    return [int64](Invoke-RestMethod -Uri "$Base/stats").bytes_served
}

function Http-Stats([string]$Base) {
    return Invoke-RestMethod -Uri "$Base/stats"
}

function Start-HttpServer([string[]]$PackDirs, [string]$StateDir) {
    $serverScript = Join-Path $repo 'scripts/e2e-http.py'
    $stdout = Join-Path $stamp 'http.stdout.log'
    $stderr = Join-Path $stamp 'http.stderr.log'
    $arguments = @($serverScript)
    foreach ($packDir in $PackDirs) { $arguments += @('--pack-dir', $packDir) }
    $arguments += @('--state-dir', $StateDir, '--port', '0')
    $process = Start-Process -FilePath 'python' -ArgumentList $arguments -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if (Test-Path $stdout) {
            $line = Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue |
                Select-String '^PORT='
            if ($line) {
                $port = [int]($line.ToString().Split('=')[1])
                return @{ Process = $process; Base = "http://127.0.0.1:$port" }
            }
        }
        if ($process.HasExited) { throw "HTTP server exited: $(Get-Content $stderr -Raw)" }
        Start-Sleep -Milliseconds 100
    }
    throw 'HTTP server did not report a port'
}

try {
    $env:VESPER_DEV_KEY = ('01' * 32)
    $env:VESPER_APP_ID = 'com.example.e2e'
    $buildOutput = Join-Path $stamp 'builds'
    python (Join-Path $repo 'scripts/make-synth-build.py') --out $buildOutput
    $vesper = Join-Path $repo 'target/debug/vesper.exe'
    if (-not (Test-Path $vesper)) {
        cmd /c ('call "' + (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat') + '" -arch=x64 >nul && cargo build -p vesper-cli')
        if ($LASTEXITCODE -ne 0) { throw 'vesper build failed' }
    }

    $index = Join-Path $stamp 'index'; $keys = Join-Path $stamp 'keys'
    Run-Vesper index init --index $index --root-keys 1 --threshold 1 --out-keys $keys
    $config = Join-Path $stamp 'vesper.toml'
    $initOutput = @( & $vesper init --output $config --app-id $env:VESPER_APP_ID )
    if ($LASTEXITCODE -ne 0) { throw 'vesper init failed' }
    $pub = ($initOutput | Select-String 'developer_public_key=').ToString().Split('=')[1].Trim()
    Run-Vesper index route-add --index $index --app $env:VESPER_APP_ID --pubkey $pub
    Run-Vesper index route-sign --index $index --root-key-file (Join-Path $keys 'root-0.key')

    $out1 = Join-Path $stamp 'pack1'; $out2 = Join-Path $stamp 'pack2'
    Run-Vesper pack --input (Join-Path $buildOutput 'v1') --output $out1 --app-id $env:VESPER_APP_ID --platform windows-x64 --version 1 --release-tag v1 --max-pack-size 134217728
    Run-Vesper pack --input (Join-Path $buildOutput 'v2') --output $out2 --app-id $env:VESPER_APP_ID --platform windows-x64 --version 2 --release-tag v2 --max-pack-size 134217728 --previous (Join-Path $out1 'manifest.cbor.zst')

    $serverState = Join-Path $stamp 'http-state'
    $server = Start-HttpServer @((Join-Path $out1 'packs'), (Join-Path $out2 'packs')) $serverState
    $serverProcess = $server.Process
    $base = $server.Base

    Run-Vesper publish $out1 --channel stable --index $index --owner o --repo r --token token --api-base $base --upload-base $base
    Run-Vesper index verify --index $index --app $env:VESPER_APP_ID
    $target = Join-Path $stamp 'installed'
    $statsBeforeV1 = Http-Stats $base
    $v1Preflight = (& $vesper install --manifest (Join-Path $out1 'manifest.cbor.zst') --source $base --target $target --dry-run --json | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'vesper install v1 preflight failed' }
    $v1DownloadBytes = [int64]$v1Preflight.download_bytes
    $v1Timer = [System.Diagnostics.Stopwatch]::StartNew()
    Run-Vesper install --manifest (Join-Path $out1 'manifest.cbor.zst') --source $base --target $target
    $v1Timer.Stop()
    $statsAfterV1 = Http-Stats $base
    $v1Requests = [int64]$statsAfterV1.requests - [int64]$statsBeforeV1.requests
    $v1ServedBytes = [int64]$statsAfterV1.bytes_served - [int64]$statsBeforeV1.bytes_served
    if ($v1ServedBytes -gt [int64]([Math]::Ceiling($v1DownloadBytes * 1.02))) {
        throw "v1 download duplicated bytes: served=$v1ServedBytes required=$v1DownloadBytes"
    }
    $v1Size = Tree-Bytes $target
    $v1Check = (& $vesper check (Join-Path $buildOutput 'v1') --json | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'vesper check v1 failed' }
    $v1BuildBytes = [int64]$v1Check.total_bytes
    if ($v1Size -gt [int64]([Math]::Ceiling($v1BuildBytes * 1.01))) {
        throw "v1 install grew beyond build size: install=$v1Size build=$v1BuildBytes"
    }
    $chunkStore = Join-Path $target '.vesper/chunks'
    if ((Test-Path $chunkStore) -and (Tree-Bytes $chunkStore) -ne 0) {
        throw "verified chunk store was not cleared: $chunkStore"
    }

    Run-Vesper publish $out2 --channel beta --index $index --owner o --repo r --token token --api-base $base --upload-base $base
    $servedBeforeUpdate = Http-Bytes $base
    $diff = (& $vesper diff-check $out2\manifest.cbor.zst --against $out1\manifest.cbor.zst --json | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'vesper diff-check failed' }
    $v2ChangedBytes = (Get-Item (Join-Path $buildOutput 'v2/Content/large-2.pak')).Length +
        (Get-Item (Join-Path $buildOutput 'v2/Content/medium.bin')).Length +
        (Get-Item (Join-Path $buildOutput 'v2/Content/added.bin')).Length
    $preflight = (& $vesper install --manifest (Join-Path $out2 'manifest.cbor.zst') --source $base --target $target --dry-run --json | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'vesper install preflight failed' }
    $peakEstimated = [int64]$preflight.peak_bytes
    $downloadBytes = [int64]$preflight.download_bytes

    $updateStdout = Join-Path $stamp 'update.stdout.log'
    $updateStderr = Join-Path $stamp 'update.stderr.log'
    $updateArgs = @('install', '--manifest', (Join-Path $out2 'manifest.cbor.zst'), '--source', $base, '--target', $target, '--json')
    $updateTimer = [System.Diagnostics.Stopwatch]::StartNew()
    $updateProcess = Start-Process -FilePath $vesper -ArgumentList $updateArgs -PassThru `
        -RedirectStandardOutput $updateStdout -RedirectStandardError $updateStderr
    $peakMeasured = Tree-Bytes $target
    $peakRss = [int64]0
    while (-not $updateProcess.HasExited) {
        $peakMeasured = [Math]::Max($peakMeasured, (Tree-Bytes $target))
        try {
            $updateProcess.Refresh()
            $peakRss = [Math]::Max($peakRss, [int64]$updateProcess.PeakWorkingSet64)
        } catch {}
        Start-Sleep -Milliseconds 100
    }
    $updateProcess.WaitForExit()
    $updateTimer.Stop()
    $updateProcess.Refresh()
    $peakMeasured = [Math]::Max($peakMeasured, (Tree-Bytes $target))
    try { $peakRss = [Math]::Max($peakRss, [int64]$updateProcess.PeakWorkingSet64) } catch {}
    $updateExitCode = $updateProcess.ExitCode
    $updateOutput = Get-Content $updateStdout -Raw -ErrorAction SilentlyContinue
    $updateJson = $null
    if ($updateOutput) {
        try { $updateJson = $updateOutput | ConvertFrom-Json } catch {}
    }
    if ($null -eq $updateExitCode -and ($updateOutput -match 'installed ' -or $updateOutput -match '"installed"')) { $updateExitCode = 0 }
    if ($null -eq $updateExitCode -or $updateExitCode -ne 0) {
        throw "update failed ($updateExitCode): $(Get-Content $updateStderr -Raw) $updateOutput"
    }
    if ((Test-Path $chunkStore) -and (Tree-Bytes $chunkStore) -ne 0) {
        throw "verified chunk store remained after update: $chunkStore"
    }
    $servedAfterUpdate = Http-Bytes $base
    $patchBytes = $servedAfterUpdate - $servedBeforeUpdate
    if ($patchBytes -gt [int64]([Math]::Ceiling($downloadBytes * 1.02))) {
        throw "update download duplicated bytes: served=$patchBytes required=$downloadBytes"
    }
    $actualChangedBytes = 64 * 1024 + 3 * 1024 * 1024 + 256 * 1024 + 11
    if ($patchBytes -le $actualChangedBytes -or $patchBytes -ge $v2ChangedBytes) {
        throw "unexpected patch size: patch=$patchBytes actual=$actualChangedBytes changed_files=$v2ChangedBytes"
    }
    if (($peakMeasured - $v1Size) -gt $peakEstimated) {
        throw "measured delta $($peakMeasured - $v1Size) exceeded estimated peak $peakEstimated"
    }
    $peakDelta = $peakMeasured - $v1Size
    $tightLimit = [int64]$v2ChangedBytes + $downloadBytes + 64 * 1024 * 1024
    if ($peakDelta -gt $tightLimit) {
        throw "tight peak bound failed: delta=$peakDelta limit=$tightLimit"
    }
    $statsAfterUpdate = Http-Stats $base
    $maxConcurrency = [int64]$statsAfterUpdate.max_concurrency
    if ($maxConcurrency -lt 6 -or $maxConcurrency -gt 8) { throw "HTTP server observed invalid parallelism: max=$maxConcurrency" }
    if ($peakRss -le 16 * 1024 * 1024 -or $peakRss -gt 256 * 1024 * 1024) { throw "vesper RSS outside measured bounds: $peakRss" }

    Run-Vesper install-rollback --target $target
    Run-Vesper verify --manifest (Join-Path $out1 'manifest.cbor.zst') --target $target

    $targets = Join-Path $index ('apps/' + $env:VESPER_APP_ID + '/targets.json')
    Add-Content -Path $targets -Value 'x'
    & $vesper index verify --index $index --app $env:VESPER_APP_ID
    if ($LASTEXITCODE -eq 0) { throw 'tampered targets unexpectedly verified' }
    $httpBytes = Http-Bytes $base
    if ($httpBytes -le 0) { throw 'HTTP server served no bytes' }
    $startedAt.Stop()
    $hedgedBytes = if ($null -ne $updateJson -and $null -ne $updateJson.hedged_bytes) { [int64]$updateJson.hedged_bytes } else { [int64]0 }
    Write-Output "e2e v1_install_bytes=$v1Size v2_patch_bytes=$patchBytes http_bytes_served=$httpBytes hedged_bytes=$hedgedBytes v1_requests=$v1Requests v1_seconds=$([Math]::Round($v1Timer.Elapsed.TotalSeconds, 2)) v2_seconds=$([Math]::Round($updateTimer.Elapsed.TotalSeconds, 2)) max_concurrency=$maxConcurrency peak_measured=$peakMeasured peak_estimated=$peakEstimated peak_rss=$peakRss seconds=$([Math]::Round($startedAt.Elapsed.TotalSeconds, 2))"
}
finally {
    if ($null -ne $serverProcess -and -not $serverProcess.HasExited) {
        Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
        $serverProcess.WaitForExit()
    }
    Remove-Item -Recurse -Force $stamp -ErrorAction SilentlyContinue
}
