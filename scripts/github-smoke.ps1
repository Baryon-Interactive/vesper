param(
  [Parameter(Mandatory = $true)][string]$Owner,
  [Parameter(Mandatory = $true)][string]$Repo,
  [Parameter(Mandatory = $true)][string]$Manifest,
  [Parameter(Mandatory = $true)][string]$Release,
  [string]$Update,
  [switch]$Rollback
)
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$source = "https://github.com/$Owner/$Repo/releases/download/{release}/{pack_id}"
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("vesper-github-smoke-" + [Guid]::NewGuid().ToString('N'))
$target = Join-Path $tempRoot 'install'
$manifestPath = Join-Path $tempRoot 'manifest.cbor.zst'
$updatePath = Join-Path $tempRoot 'update-manifest.cbor.zst'
$vsdev = @(
  (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
  $env:VESPER_VCVARS64
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $vsdev) { throw 'Visual Studio developer command script was not found' }

function Resolve-Manifest([string]$Value, [string]$Destination, [string]$Tag) {
  if (Test-Path -LiteralPath $Value -PathType Leaf) {
    Copy-Item -LiteralPath $Value -Destination $Destination -Force
    return
  }
  $url = if ($Value -match '^https?://') { $Value } else {
    "https://github.com/$Owner/$Repo/releases/download/$Tag/$Value"
  }
  Invoke-WebRequest -Uri $url -OutFile $Destination
}

function Invoke-Vesper([string[]]$VesperArgs) {
  $quoted = ($VesperArgs | ForEach-Object { '"' + $_.Replace('"', '\"') + '"' }) -join ' '
  $line = 'call "' + $vsdev + '" >nul && cargo ' + $quoted
  $timer = [Diagnostics.Stopwatch]::StartNew()
  $output = cmd /c $line
  $exitCode = $LASTEXITCODE
  $timer.Stop()
  if ($exitCode -ne 0) { $output | Out-Host; throw "vesper failed ($exitCode): $($VesperArgs -join ' ')" }
  [PSCustomObject]@{ Output = @($output); Seconds = $timer.Elapsed.TotalSeconds }
}

function Measure-Install([string]$ManifestFile, [string]$Tag) {
  $dry = Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','install','--manifest',$ManifestFile,'--source',$source,'--release',$Tag,'--target',$target,'--dry-run','--json')
  $lines = @($dry | ForEach-Object {
    if ($_ -is [string]) { $_ }
    elseif ($_.PSObject.Properties['Output']) { $_.Output }
  })
  $jsonLine = @($lines | Where-Object { $_ -match '^\s*\{' } | Select-Object -Last 1)
  if (-not $jsonLine) { throw 'vesper dry-run did not return JSON' }
  $json = ($jsonLine -join "`n" | ConvertFrom-Json)
  return [uint64]$json.download_bytes
}

try {
  New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
  Resolve-Manifest $Manifest $manifestPath $Release
  $bytes = Measure-Install $manifestPath $Release
  $start = [Diagnostics.Stopwatch]::StartNew()
  Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','install','--manifest',$manifestPath,'--source',$source,'--release',$Release,'--target',$target) | ForEach-Object { $_.Output | Out-Host }
  $start.Stop()
  Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','verify','--manifest',$manifestPath,'--target',$target) | ForEach-Object { $_.Output | Out-Host }
  Write-Host ("step=install release={0} elapsed_seconds={1:N3} downloaded_bytes={2}" -f $Release,$start.Elapsed.TotalSeconds,$bytes)

  if ($Update) {
    Resolve-Manifest $Update $updatePath $Release
    $bytes = Measure-Install $updatePath $Release
    $start = [Diagnostics.Stopwatch]::StartNew()
    Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','install','--manifest',$updatePath,'--source',$source,'--release',$Release,'--target',$target) | ForEach-Object { $_.Output | Out-Host }
    $start.Stop()
    Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','verify','--manifest',$updatePath,'--target',$target) | ForEach-Object { $_.Output | Out-Host }
    Write-Host ("step=update release={0} elapsed_seconds={1:N3} downloaded_bytes={2}" -f $Release,$start.Elapsed.TotalSeconds,$bytes)
  }

  if ($Rollback) {
    $start = [Diagnostics.Stopwatch]::StartNew()
    Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','install-rollback','--target',$target) | ForEach-Object { $_.Output | Out-Host }
    $start.Stop()
    Invoke-Vesper @('run','--manifest-path',(Join-Path $repoRoot 'Cargo.toml'),'-p','vesper-cli','--','verify','--manifest',$manifestPath,'--target',$target) | ForEach-Object { $_.Output | Out-Host }
    Write-Host ("step=rollback elapsed_seconds={0:N3} downloaded_bytes=0" -f $start.Elapsed.TotalSeconds)
  }
  exit 0
} finally {
  if (Test-Path $tempRoot) { Remove-Item -LiteralPath $tempRoot -Recurse -Force }
}
