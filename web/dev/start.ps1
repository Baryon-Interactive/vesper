# Build a local signed catalogue for the web store demo.
#
# Publishes the bundled demo game as a web build (channel stable-web-wasm32).
# Optionally adds another game from existing build folders, for example:
#   -GameId nda -GameTitle 'My Game' -GameSummary '...' -GameWeb <web build dir> `
#   -GameWindows <windows build dir> -GameCover <png>
# Then serve web/dev/out with web/dev/serve.mjs (port 3220) and run the web app.
param(
  [int]$Port = 3220,
  [string]$GameId = '',
  [string]$GameTitle = '',
  [string]$GameSummary = '',
  [string]$GameDescription = '',
  [string]$GameTags = 'demo',
  [string]$GameWeb = '',
  [string]$GameWindows = '',
  [string]$GameCover = ''
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$web = Join-Path $repo 'web'
$work = Join-Path $PSScriptRoot 'work'
$out = Join-Path $PSScriptRoot 'out'

$vsdev = @(
  (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
  $env:VESPER_VCVARS64
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
$env:PATH = ((Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer') + ';') + $env:PATH
function Invoke-Cargo([string]$Arguments) {
  $line = if ($vsdev) { 'call "' + $vsdev + '" -arch=x64 >nul && cargo ' + $Arguments } else { 'cargo ' + $Arguments }
  $output = cmd /c ($line + ' 2>&1')
  if ($LASTEXITCODE -ne 0) { throw "cargo $Arguments failed: $output" }
  return $output
}
Invoke-Cargo ('build --manifest-path "' + $repo + '\Cargo.toml" -p vesper-cli') | Out-Null
$vesper = Join-Path $repo 'target/debug/vesper.exe'
function Invoke-Vesper([string[]]$VesperArgs) {
  $output = & $vesper @VesperArgs 2>&1
  if ($LASTEXITCODE -ne 0) { throw "vesper failed: $($VesperArgs -join ' ') $output" }
  return $output
}
function Get-MediaHash([string]$Path) {
  $helper = Join-Path $repo 'launcher/dev/sign-paid/Cargo.toml'
  $hash = Invoke-Cargo ('run -q --manifest-path "' + $helper + '" -- --media-hash "' + $Path + '"') | Where-Object { $_ -match '^[0-9a-f]{64}$' } | Select-Object -Last 1
  if (-not $hash) { throw "media hash failed for $Path" }
  return $hash
}
function Write-Utf8([string]$Path, [string]$Text) {
  [System.IO.File]::WriteAllText($Path, $Text, [System.Text.UTF8Encoding]::new($false))
}
function New-KeyHex { return (([Guid]::NewGuid().ToString('N')) + ([Guid]::NewGuid().ToString('N'))) }

foreach ($dir in @($work, $out)) { if (Test-Path $dir) { Remove-Item -Recurse -Force $dir } }
$index = Join-Path $work 'index'
$keys = Join-Path $work 'keys'
New-Item -ItemType Directory -Force -Path $index, $keys | Out-Null

$env:VESPER_ONLINE_KEY = New-KeyHex
$onlinePublic = Invoke-Vesper @('index', 'online-pubkey') | Where-Object { $_ -match '^[0-9a-f]{64}$' } | Select-Object -Last 1
Invoke-Vesper @('index', 'init', '--index', $index, '--root-keys', '1', '--threshold', '1', '--out-keys', $keys, '--online-pubkey', $onlinePublic) | Out-Host
$rootKey = Join-Path $keys 'root-0.key'

$fallbackCover = Join-Path $repo 'launcher/src-tauri/icons/icon.ico'
$apps = @(
  @{ Id = 'demo.ember'; Title = 'Ember Drift'; Summary = 'Steer a spark through the dark and collect embers. A tiny canvas game streamed from signed Vesper packs.'
     Description = "Ember Drift is the bundled Vesper web demo.`n`n- Arrow keys or WASD to steer`n- Collect embers, avoid the growing shadows`n`nEvery file is fetched by byte range from content-addressed packs and verified in your browser."
     Tags = @('demo', 'arcade'); Web = (Join-Path $PSScriptRoot 'demo-game'); Windows = ''; Cover = (Join-Path $PSScriptRoot 'ember-cover.png') }
)
if ($GameId) {
  $apps += @{ Id = $GameId; Title = $GameTitle; Summary = $GameSummary; Description = $GameDescription; Tags = @($GameTags -split ','); Web = $GameWeb; Windows = $GameWindows; Cover = $(if ($GameCover) { $GameCover } else { $fallbackCover }) }
}

$releases = @()
$media = @()
foreach ($app in $apps) {
  $env:VESPER_DEV_KEY = New-KeyHex
  $env:VESPER_APP_ID = $app.Id
  $initOutput = Invoke-Vesper @('init', '--output', (Join-Path $work "$($app.Id).toml"), '--app-id', $app.Id)
  $public = ($initOutput | Select-String 'developer_public_key=' | ForEach-Object { ($_ -split '=', 2)[1].Trim() } | Select-Object -First 1)
  if (-not $public) { throw "missing developer public key for $($app.Id)" }
  Invoke-Vesper @('index', 'route-add', '--index', $index, '--app', $app.Id, '--pubkey', $public) | Out-Host
  Invoke-Vesper @('index', 'route-sign', '--index', $index, '--root-key-file', $rootKey) | Out-Host

  $coverName = "$($app.Id)$([IO.Path]::GetExtension($app.Cover))"
  $media += @{ From = $app.Cover; Name = $coverName }
  $cover = @{ url = "http://127.0.0.1:$Port/media/$coverName"; blake3 = (Get-MediaHash $app.Cover); width = 1600; height = 900 }
  $platforms = @()
  if ($app.Windows) { $platforms += 'windows-x64' }
  if ($app.Web) { $platforms += 'web-wasm32' }
  $storeDir = Join-Path $index "apps/$($app.Id)"
  New-Item -ItemType Directory -Force -Path $storeDir | Out-Null
  $store = [ordered]@{
    title = @{ en = $app.Title }; summary = @{ en = $app.Summary }; tags = $app.Tags; screenshots = @(); cover = $cover
    requirements = [ordered]@{ browser = 'Recent Chrome, Edge or Firefox'; input = 'Keyboard and mouse' }
    paid = $false; price_display = 'Free'; developer = 'Vesper demo'; homepage = 'https://github.com/Baryon-Interactive/vesper'; platforms = $platforms
  }
  Write-Utf8 (Join-Path $storeDir 'store.json') (($store | ConvertTo-Json -Depth 8) + "`n")
  $description = if ($app.Description) { $app.Description } else { $app.Summary }
  Write-Utf8 (Join-Path $storeDir 'description.md') ("# $($app.Title)`n`n$description`n")

  $builds = @()
  if ($app.Windows) { $builds += @{ Dir = $app.Windows; Platform = 'windows-x64'; Channel = 'stable'; Suffix = 'win' } }
  if ($app.Web) { $builds += @{ Dir = $app.Web; Platform = 'web-wasm32'; Channel = 'stable-web-wasm32'; Suffix = 'web' } }
  foreach ($b in $builds) {
    $release = "$($app.Id)-$($b.Suffix)-1"
    $pack = Join-Path $work $release
    Invoke-Vesper @('pack', '--input', $b.Dir, '--output', $pack, '--app-id', $app.Id, '--platform', $b.Platform, '--version', '1.0.0', '--release-tag', $release) | Out-Host
    Invoke-Vesper @('publish', $pack, '--tag', $release, '--channel', $b.Channel, '--index', $index) | Out-Host
    $releases += @{ Pack = $pack; Release = $release }
  }
}
foreach ($app in $apps) { Invoke-Vesper @('index', 'snapshot', '--index', $index, '--app', $app.Id, '--initialize') | Out-Host }
Invoke-Vesper @('index', 'timestamp', '--index', $index, '--all') | Out-Host
Invoke-Vesper @('index', 'verify', '--index', $index) | Out-Host
node (Join-Path $repo 'index-template/scripts/build-api.mjs') --index $index --out $out
if ($LASTEXITCODE -ne 0) { throw 'build-api failed' }
New-Item -ItemType Directory -Force -Path (Join-Path $out 'media') | Out-Null
foreach ($m in $media) { Copy-Item -LiteralPath $m.From -Destination (Join-Path $out "media/$($m.Name)") }
foreach ($r in $releases) {
  $dest = Join-Path $out $r.Release
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  Copy-Item (Join-Path $r.Pack 'packs/*') $dest -Force
  Copy-Item (Join-Path $r.Pack 'manifest.cbor.zst') (Join-Path $dest 'manifest.cbor.zst') -Force
}
# Point the local web app at this catalogue. Only localhost pages read the dev config.
Copy-Item (Join-Path $out 'root.json') (Join-Path $web 'public/vesper/dev-root.json') -Force
Write-Utf8 (Join-Path $web 'public/vesper-config.dev.json') ((@{ apiBase = "http://127.0.0.1:$Port/"; trustedRoot = '/vesper/dev-root.json'; playOrigin = 'http://127.0.0.1:3240' } | ConvertTo-Json) + "`n")
# The play site is a separate origin; only the local store may embed it.
Write-Utf8 (Join-Path $web 'play/vesper-config.dev.json') ((@{ apiBase = "http://127.0.0.1:$Port/"; trustedRoot = '/vesper/dev-root.json'; storeOrigins = @('http://127.0.0.1:3230', 'http://localhost:3230') } | ConvertTo-Json) + "`n")
Write-Host "Catalogue output: $out"
Write-Host "Serve it:   node web/dev/serve.mjs --root `"$out`" --port $Port"
Write-Host "Play site:  node web/dev/serve.mjs --root web/play --root web/public --port 3240"
Write-Host "Run the app: npm --prefix web run dev   (http://127.0.0.1:3230)"
