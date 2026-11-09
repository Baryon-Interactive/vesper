param([string]$Out, [string]$Title = 'Vesper', [long]$WindowHandle = 0, [int]$ProcessId = 0)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  public struct RECT { public int L, T, R, B; }
}
"@
if ($WindowHandle -ne 0) {
  $p = Get-Process -Id $ProcessId -ErrorAction Stop
  $h = [IntPtr]$WindowHandle
} else {
  $p = Get-Process | Where-Object { $_.MainWindowTitle -eq $Title } | Select-Object -First 1
  $h = if ($p) { $p.MainWindowHandle } else { [IntPtr]::Zero }
}
if (-not $p) { throw "window not found" }
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $hgt = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$printed = [W]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
if (-not $printed) { $g.Dispose(); $bmp.Dispose(); throw 'Window capture failed' }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
"saved $Out ${w}x${hgt} pid=$($p.Id)"
