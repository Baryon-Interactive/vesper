$ErrorActionPreference = 'Stop'
$launcher = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$vsdev = @(
  (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
  $env:VESPER_VCVARS64
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $vsdev) { throw 'Visual Studio developer command script was not found' }
$env:PATH = ((Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer') + ';') + $env:PATH
$line = 'call "' + $vsdev + '" && cd /d "' + $launcher + '" && npx tauri build --debug --no-bundle'
cmd /c ($line + ' 2>&1') | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'Tauri debug build failed' }
$source = Join-Path $launcher 'src-tauri/target/debug/vesper-launcher.exe'
$destination = Join-Path $PSScriptRoot 'out/bin/vesper-launcher.exe'
if (!(Test-Path $source)) { throw "Build did not produce $source" }
New-Item -ItemType Directory -Force -Path (Split-Path $destination) | Out-Null
Copy-Item $source $destination -Force
Write-Host "Debug executable copied to $destination"
