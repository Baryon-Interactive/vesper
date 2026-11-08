$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$vesper = Join-Path $repo 'target/debug/vesper.exe'
if (-not (Test-Path $vesper)) {
    $vsdev = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
        $env:VESPER_VCVARS64
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $vsdev) { throw 'Visual Studio developer command script was not found' }
    $line = 'call "' + $vsdev + '" -arch=x64 >nul && cargo build --manifest-path "' + (Join-Path $repo 'Cargo.toml') + '" -p vesper-cli'
    cmd /c ($line + ' 2>&1') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'vesper build failed' }
}
$stamp = Join-Path ([System.IO.Path]::GetTempPath()) ('vesper-mem-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $stamp | Out-Null

function Make-RandomFile([string]$Path, [int64]$Bytes, [int]$Seed) {
    $rng = [System.Random]::new($Seed)
    $buffer = New-Object byte[] (1024 * 1024)
    $stream = [System.IO.File]::Create($Path)
    try {
        $remaining = $Bytes
        while ($remaining -gt 0) {
            $count = [int][Math]::Min($remaining, $buffer.Length)
            $rng.NextBytes($buffer)
            $stream.Write($buffer, 0, $count)
            $remaining -= $count
        }
    } finally { $stream.Dispose() }
}

function Measure-Install([int64]$MiB, [int]$Seed) {
    $input = Join-Path $stamp "input-$MiB"
    $build = Join-Path $stamp "build-$MiB"
    $target = Join-Path $stamp "target-$MiB"
    New-Item -ItemType Directory -Path $input | Out-Null
    Make-RandomFile (Join-Path $input 'payload.bin') ($MiB * 1024 * 1024) $Seed
    & $vesper pack --input $input --output $build --app-id mem-check --platform windows-x64 --version 1 --max-pack-size 134217728 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "pack failed for $MiB MiB" }
    $stdout = Join-Path $stamp "install-$MiB.out"
    $stderr = Join-Path $stamp "install-$MiB.err"
    $args = @('install', '--manifest', (Join-Path $build 'manifest.cbor.zst'), '--source', $build, '--target', $target)
    $process = Start-Process -FilePath $vesper -ArgumentList $args -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    [int64]$peak = 0
    while (-not $process.HasExited) {
        try {
            $process.Refresh()
            $peak = [Math]::Max($peak, [int64]$process.PeakWorkingSet64)
        } catch {}
        Start-Sleep -Milliseconds 100
    }
    $process.WaitForExit()
    $process.Refresh()
    try { $peak = [Math]::Max($peak, [int64]$process.PeakWorkingSet64) } catch {}
    $err = if (Test-Path $stderr) { Get-Content $stderr -Raw } else { '' }
    $out = if (Test-Path $stdout) { Get-Content $stdout -Raw } else { '' }
    $exitCode = $process.ExitCode
    if (($null -eq $exitCode -or $exitCode -ne 0) -and $out -notmatch 'installed ') {
        throw "install failed for $MiB MiB (exit=$exitCode): $err $out"
    }
    return $peak
}

try {
    $build400 = Measure-Install 400 400
    $build1200 = Measure-Install 1200 1200
    if ($build400 -le 16 * 1024 * 1024 -or $build1200 -le 16 * 1024 * 1024 -or
        $build400 -gt 256 * 1024 * 1024 -or $build1200 -gt 256 * 1024 * 1024) {
        throw "memory limit exceeded: build400=$build400 build1200=$build1200"
    }
    if ([Math]::Abs($build1200 - $build400) -gt 64 * 1024 * 1024) {
        throw "memory scaled with build size: build400=$build400 build1200=$build1200"
    }
    Write-Output "mem build400=$build400 build1200=$build1200"
} finally {
    Remove-Item -Recurse -Force $stamp -ErrorAction SilentlyContinue
}
