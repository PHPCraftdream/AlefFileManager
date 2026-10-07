# M0.5 spike, Windows: runs a program on a desktop that nobody sits at (the closest thing to session 0 that
# a regular session can make: a new desktop of the interactive window station, no input, nothing shown).
#   powershell -File desktop.ps1 -Command '"C:\path\alef.exe" --app "C:\site" -- --hold-ms=3000' -Out C:\log.txt
param(
  [Parameter(Mandatory = $true)][string]$Command,
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$TimeoutMs = 120000
)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class SpikeDesktop {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct STARTUPINFO {
    public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
    public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
    public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
  }
  [StructLayout(LayoutKind.Sequential)]
  public struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
  [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern IntPtr CreateDesktop(string name, string device, IntPtr mode, int flags, uint access, IntPtr attributes);
  [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern bool CreateProcess(string app, string command, IntPtr pa, IntPtr ta, bool inherit, uint flags,
    IntPtr env, string cwd, ref STARTUPINFO info, out PROCESS_INFORMATION process);
  [DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(IntPtr handle, uint ms);
  [DllImport("kernel32.dll")] public static extern bool GetExitCodeProcess(IntPtr handle, out uint code);
  [DllImport("kernel32.dll")] public static extern bool TerminateProcess(IntPtr handle, uint code);
}
"@

$name = "alef-spike-$PID"
$desktop = [SpikeDesktop]::CreateDesktop($name, $null, [IntPtr]::Zero, 0, 0x000F01FF, [IntPtr]::Zero)
if ($desktop -eq [IntPtr]::Zero) { Write-Output "NO-DESKTOP error=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"; exit 2 }

$info = New-Object SpikeDesktop+STARTUPINFO
$info.cb = [Runtime.InteropServices.Marshal]::SizeOf($info)
$info.lpDesktop = "winsta0\$name"
$process = New-Object SpikeDesktop+PROCESS_INFORMATION
$line = "cmd.exe /c ($Command) > `"$Out`" 2>&1"
if (-not [SpikeDesktop]::CreateProcess($null, $line, [IntPtr]::Zero, [IntPtr]::Zero, $false, 0, [IntPtr]::Zero, $null, [ref]$info, [ref]$process)) {
  Write-Output "NO-PROCESS error=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
  [void][SpikeDesktop]::CloseDesktop($desktop)
  exit 3
}
$waited = [SpikeDesktop]::WaitForSingleObject($process.hProcess, [uint32]$TimeoutMs)
$code = [uint32]0
if ($waited -ne 0) { [void][SpikeDesktop]::TerminateProcess($process.hProcess, 99); Write-Output "TIMEOUT" }
[void][SpikeDesktop]::GetExitCodeProcess($process.hProcess, [ref]$code)
[void][SpikeDesktop]::CloseDesktop($desktop)
Write-Output "EXIT $code"
