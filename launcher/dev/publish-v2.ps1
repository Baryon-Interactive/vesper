param(
  [string]$Work = (Join-Path $PSScriptRoot 'work')
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$platform = Join-Path $repo 'index-template'
$Work = [IO.Path]::GetFullPath($Work)
if (-not $Work.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Demo work directory must stay inside this checkout' }

$vsdev = @(
  (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
  $env:VESPER_VCVARS64
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $vsdev) { throw 'Visual Studio developer command script was not found' }
$env:PATH = ((Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer') + ';') + $env:PATH
$index = Join-Path $Work 'index'
$out = Join-Path $PSScriptRoot 'out'
$free = Join-Path $Work 'builds/free'
if (!(Test-Path $index) -or !(Test-Path $free)) { throw 'Run start.ps1 first so the v1 index exists.' }
$env:VESPER_DEV_KEY = (Get-Content -Raw -Encoding UTF8 (Join-Path $Work 'free.devkey')).Trim()
$env:VESPER_APP_ID = 'demo.free'
$env:VESPER_ONLINE_KEY = (Get-Content -Raw -Encoding UTF8 (Join-Path $Work 'online.key')).Trim()
function Invoke-Vesper([string[]]$VesperArgs) {
  $quoted = ($VesperArgs | ForEach-Object { '"' + $_.Replace('"','\"') + '"' }) -join ' '
  $line = 'call "' + $vsdev + '" >nul && cargo run --manifest-path "' + $repo + '\Cargo.toml" -p vesper-cli -- ' + $quoted
  $output = cmd /c ($line + ' 2>&1')
  if ($LASTEXITCODE -ne 0) { throw "vesper failed: $($VesperArgs -join ' ') $output" }
  $output
}
function Write-Utf8([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false)) }
$bytes = [IO.File]::ReadAllBytes((Join-Path $free 'content.bin'))
for ($i = 524288; $i -lt 786432; $i++) { $bytes[$i] = [byte](($bytes[$i] + 73) % 251) }
[IO.File]::WriteAllBytes((Join-Path $free 'content.bin'), $bytes)
Write-Utf8 (Join-Path $free 'VERSION.txt') '2'
Write-Utf8 (Join-Path $free 'bonus.txt') 'v2 bonus content'
$pack = Join-Path $Work 'free-v2-pack'
if (Test-Path $pack) { Remove-Item -Recurse -Force $pack }
Invoke-Vesper @('pack','--input',$free,'--output',$pack,'--app-id','demo.free','--platform','windows-x64','--version','2','--release-tag','free-2','--previous',(Join-Path $Work 'free-pack/manifest.cbor.zst')) | Out-Host
Invoke-Vesper @('publish',$pack,'--channel','stable','--tag','free-2','--index',$index) | Out-Host
Invoke-Vesper @('index','snapshot','--index',$index,'--app','demo.free') | Out-Host
Invoke-Vesper @('index','timestamp','--index',$index,'--all') | Out-Host
 $refresh = Join-Path $Work 'out-refresh'
if (Test-Path $refresh) { Remove-Item -Recurse -Force $refresh }
New-Item -ItemType Directory -Force -Path $refresh | Out-Null
node (Join-Path $platform 'scripts/build-api.mjs') --index $index --out $refresh
$appOut = Join-Path $refresh 'apps/demo.free'
Copy-Item (Join-Path $pack 'manifest.cbor.zst') (Join-Path $appOut 'manifest.cbor.zst') -Force
$releaseOut = Join-Path $refresh 'free-2'
New-Item -ItemType Directory -Force -Path $releaseOut | Out-Null
Copy-Item (Join-Path $pack 'packs/*') $releaseOut -Force
Copy-Item (Join-Path $pack 'manifest.cbor.zst') (Join-Path $releaseOut 'manifest.cbor.zst') -Force
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item (Join-Path $refresh '*') $out -Recurse -Force
Write-Host "Published demo.free v2 to stable. Existing servers read the refreshed out/ tree on the next request."
