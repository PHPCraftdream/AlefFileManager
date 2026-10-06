# Startup probe (Windows): launches a program and records (1) every creation, show, hide and
# destruction of a window of that process (WinEvent hook, so a window that lives a few milliseconds
# is not missed) and (2) screenshots of the program's own top-level window only (PrintWindow: what
# the compositor holds, never the rest of the desktop).
#   powershell -File startup-probe.ps1 -Exe <path> -OutDir <dir> [-Seconds 15] [-Arguments "..."]
#              [-WorkDir <dir>] [-Assert]
# With -Assert the exit code is 1 when the user could see an unfinished start:
#   - a window other than the main one was shown (the flash of a helper window), or
#   - the main window was never shown, or
#   - the first picture of the main window after it was shown is unfinished: its client area has
#     only a few colours (white, or white with a black strip where the window grew).
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [Parameter(Mandatory = $true)][string]$OutDir,
  [int]$Seconds = 15,
  [string]$Arguments = "",
  [string]$WorkDir = "",
  [switch]$Assert
)
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Text;
using System.Threading;
using System.Runtime.InteropServices;
public static class Probe {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  delegate void HookProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Auto)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, HookProc proc, uint pid, uint thread, uint flags);
  [DllImport("user32.dll")] static extern int GetMessage(out MSG m, IntPtr h, uint a, uint b);
  [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
  [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG m);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr h; public uint m; public IntPtr w; public IntPtr l; public uint t; public int x, y; }

  public static readonly List<string> Events = new List<string>();
  public static IntPtr MainHandle;
  static Stopwatch clock;
  static HookProc keep;

  static string Describe(IntPtr h) {
    var c = new StringBuilder(128); GetClassName(h, c, 128);
    RECT r; GetWindowRect(h, out r);
    return c + " " + (r.R - r.L) + "x" + (r.B - r.T) + "@" + r.L + "," + r.T + (IsWindowVisible(h) ? " visible" : " hidden");
  }

  public static void Watch(uint pid, Stopwatch stopwatch) {
    clock = stopwatch;
    SetProcessDPIAware();
    var thread = new Thread(() => {
      keep = (hook, ev, hwnd, idObject, idChild, tid, time) => {
        if (idObject != 0 || hwnd == IntPtr.Zero) return;
        string kind = ev == 0x8000 ? "create" : ev == 0x8001 ? "destroy" : ev == 0x8002 ? "show" : ev == 0x8003 ? "hide" : "event" + ev;
        lock (Events) Events.Add(string.Format("{0,6} ms  {1,-7} {2}", clock.ElapsedMilliseconds, kind, Describe(hwnd)));
      };
      SetWinEventHook(0x8000, 0x8003, IntPtr.Zero, keep, pid, 0, 0);
      MSG msg;
      while (GetMessage(out msg, IntPtr.Zero, 0, 0) > 0) { TranslateMessage(ref msg); DispatchMessage(ref msg); }
    });
    thread.IsBackground = true;
    thread.SetApartmentState(ApartmentState.STA);
    thread.Start();
  }

  public static Bitmap Capture(IntPtr h, int width, int height) {
    var bitmap = new Bitmap(width, height);
    using (var g = Graphics.FromImage(bitmap)) {
      IntPtr hdc = g.GetHdc();
      PrintWindow(h, hdc, 2);
      g.ReleaseHdc(hdc);
    }
    return bitmap;
  }

  // Number of distinct colours of the client area of the captured window, counted up to `limit`.
  public static int ClientColors(Bitmap window, IntPtr h, int limit) {
    RECT outer; GetWindowRect(h, out outer);
    RECT client; GetClientRect(h, out client);
    POINT origin = new POINT(); ClientToScreen(h, ref origin);
    var area = new Rectangle(origin.X - outer.L, origin.Y - outer.T, client.R - client.L, client.B - client.T);
    area.Intersect(new Rectangle(0, 0, window.Width, window.Height));
    if (area.Width <= 0 || area.Height <= 0) return 0;
    var data = window.LockBits(area, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
    try {
      var bytes = new byte[Math.Abs(data.Stride) * data.Height];
      Marshal.Copy(data.Scan0, bytes, 0, bytes.Length);
      var seen = new HashSet<int>();
      for (int row = 0; row < data.Height; row++) {
        for (int x = 0; x < data.Width; x++) {
          int at = row * Math.Abs(data.Stride) + x * 4;
          seen.Add(bytes[at] | (bytes[at + 1] << 8) | (bytes[at + 2] << 16));
          if (seen.Count >= limit) return limit;
        }
      }
      return seen.Count;
    } finally { window.UnlockBits(data); }
  }

  // The largest visible top-level window of the process (also sets MainHandle), or null.
  public static RECT? MainWindow(uint pid) {
    RECT? best = null; long area = 0;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != pid || !IsWindowVisible(h)) return true;
      RECT r; GetWindowRect(h, out r);
      long a = (long)(r.R - r.L) * (r.B - r.T);
      if (a > 64 * 64 && a > area) { area = a; best = r; MainHandle = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
}
"@

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Get-ChildItem $OutDir -Filter *.png | Remove-Item -Force
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = $Exe
$start.Arguments = $Arguments
$start.UseShellExecute = $false
if ($WorkDir -ne "") { $start.WorkingDirectory = $WorkDir }
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$process = [System.Diagnostics.Process]::Start($start)
[Probe]::Watch([uint32]$process.Id, $watch)
$lastHash = ""
$shots = New-Object System.Collections.Generic.List[object]
try {
  while ($watch.Elapsed.TotalSeconds -lt $Seconds -and -not $process.HasExited) {
    $rect = [Probe]::MainWindow([uint32]$process.Id)
    if ($rect -ne $null -and $shots.Count -lt 60) {
      $width = $rect.R - $rect.L
      $height = $rect.B - $rect.T
      $full = [Probe]::Capture([Probe]::MainHandle, $width, $height)
      $colors = [Probe]::ClientColors($full, [Probe]::MainHandle, 16)
      $small = New-Object System.Drawing.Bitmap 480, ([int](480 * $height / $width))
      $g2 = [System.Drawing.Graphics]::FromImage($small)
      $g2.DrawImage($full, 0, 0, $small.Width, $small.Height)
      $g2.Dispose()
      $full.Dispose()
      $ms = New-Object System.IO.MemoryStream
      $small.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
      $hash = [System.BitConverter]::ToString([System.Security.Cryptography.MD5]::Create().ComputeHash($ms.ToArray()))
      if ($hash -ne $lastHash) {
        $lastHash = $hash
        $at = [int]$watch.ElapsedMilliseconds
        [System.IO.File]::WriteAllBytes((Join-Path $OutDir ("{0:D5}ms.png" -f $at)), $ms.ToArray())
        $shots.Add([pscustomobject]@{ At = $at; Colors = $colors; Blank = ($colors -lt 16) })
      }
      $small.Dispose()
      $ms.Dispose()
    }
    Start-Sleep -Milliseconds 2
  }
} finally {
  if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
}
Start-Sleep -Milliseconds 200
$events = @([Probe]::Events)
$events | ForEach-Object { $_ }
foreach ($shot in $shots) { "{0,6} ms  picture {1} ({2} colours in the client area)" -f $shot.At, $(if ($shot.Blank) { "unfinished" } else { "content" }), $shot.Colors }
"saved $($shots.Count) window pictures in $OutDir; exit code: $(if ($process.HasExited) { $process.ExitCode } else { 'killed' })"

if ($Assert) {
  $shown = @($events | ForEach-Object {
    if ($_ -match '^\s*(\d+) ms\s+show\s+(.+?) (\d+)x(\d+)@(-?\d+),(-?\d+) visible$') {
      [pscustomobject]@{ At = [int]$Matches[1]; Class = $Matches[2]; Area = [long]$Matches[3] * [long]$Matches[4] }
    }
  } | Where-Object { $_ -ne $null })
  $problems = @()
  if ($shown.Count -eq 0) {
    $problems += "no window was ever shown"
  } else {
    $main = $shown | Sort-Object Area -Descending | Select-Object -First 1
    $strangers = @($shown | Where-Object { $_.Class -ne $main.Class -and $_.Area -gt 64 * 64 })
    foreach ($stranger in $strangers) { $problems += "another window was shown at $($stranger.At) ms: $($stranger.Class)" }
    $first = $shots | Where-Object { $_.At -ge $main.At - 100 } | Select-Object -First 1
    if ($first -eq $null) { $problems += "no picture of the main window was taken" }
    elseif ($first.Blank) { $problems += "the first picture of the main window ($($first.At) ms) is unfinished: $($first.Colors) colours in the client area" }
  }
  if ($problems.Count -gt 0) {
    $problems | ForEach-Object { "ASSERT FAIL: $_" }
    exit 1
  }
  "ASSERT OK: only the main window was shown, and its first picture has content"
}
