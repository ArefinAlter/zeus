param([Parameter(Mandatory=$true)][string]$ExpectedTui,
      [Parameter(Mandatory=$true)][string]$DesktopHome,
      [Parameter(Mandatory=$true)][string]$ReportPath,
      [switch]$Close)
$ErrorActionPreference = 'Stop'
# Match this newly generated desktop profile as well as its exact built executable.
$children = @(Get-CimInstance Win32_Process -Filter "Name='zeus-tui.exe'" | Where-Object { $_.ExecutablePath -eq $ExpectedTui -and $_.CommandLine -and $_.CommandLine.Contains('--desktop-home') -and $_.CommandLine.IndexOf($DesktopHome,[StringComparison]::OrdinalIgnoreCase) -ge 0 })
if ($children.Count -ne 1) { @{found=$false;childCount=$children.Count} | ConvertTo-Json | Set-Content -LiteralPath $ReportPath -Encoding UTF8; exit 0 }
$owners = @(Get-CimInstance Win32_Process -Filter "ProcessId=$($children[0].ParentProcessId)" | Where-Object { $_.Name -eq 'powershell.exe' -and $_.CommandLine -and $_.CommandLine.IndexOf($DesktopHome,[StringComparison]::OrdinalIgnoreCase) -ge 0 })
$owner = if ($owners.Count -eq 1) { $owners[0] } else { $null }
if ($Close) {
 $target = if ($owner) { $owner.ProcessId } else { $children[0].ProcessId }
 # Capture native stderr without PowerShell 5 promoting it to a terminating error.
 $killInfo = New-Object System.Diagnostics.ProcessStartInfo
 $killInfo.FileName = "$env:SystemRoot\System32\taskkill.exe"
 $killInfo.Arguments = "/PID $target /T /F"
 $killInfo.UseShellExecute = $false
 $killInfo.CreateNoWindow = $true
 $killInfo.RedirectStandardOutput = $true
 $killInfo.RedirectStandardError = $true
 $killer = New-Object System.Diagnostics.Process
 $killer.StartInfo = $killInfo
 try {
   if (!$killer.Start()) { throw 'Cannot start fixture cleanup.' }
   $killOutput = $killer.StandardOutput.ReadToEndAsync()
   $killError = $killer.StandardError.ReadToEndAsync()
   if (!$killer.WaitForExit(10000)) { $killer.Kill(); throw 'Fixture cleanup deadline exceeded.' }
   $killExit = $killer.ExitCode
   $null = $killOutput.GetAwaiter().GetResult()
   $killDiagnostic = $killError.GetAwaiter().GetResult().Trim()
 } finally { $killer.Dispose() }
 # A short-lived polling CLI can exit during taskkill's tree walk, making its exit code
 # nonzero even after the owned TUI is gone. Verify the exact fixture identity instead.
 $remaining = @()
 for ($attempt = 0; $attempt -lt 20; $attempt++) {
   $remaining = @(Get-CimInstance Win32_Process -Filter "Name='zeus-tui.exe'" | Where-Object { $_.ExecutablePath -eq $ExpectedTui -and $_.CommandLine -and $_.CommandLine.Contains('--desktop-home') -and $_.CommandLine.IndexOf($DesktopHome,[StringComparison]::OrdinalIgnoreCase) -ge 0 })
   if ($remaining.Count -eq 0) { break }
   Start-Sleep -Milliseconds 250
 }
 @{closed=($remaining.Count -eq 0);childCount=$remaining.Count;taskkillExit=$killExit;taskkillDiagnostic=$killDiagnostic} | ConvertTo-Json | Set-Content -LiteralPath $ReportPath -Encoding UTF8
 exit 0
}
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class ZeusConsoleProbe {
 [StructLayout(LayoutKind.Sequential)] public struct Coord { public short X,Y; }
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public short Left,Top,Right,Bottom; }
 [StructLayout(LayoutKind.Sequential)] public struct Info { public Coord Size,Cursor; public ushort Attributes; public Rect Window; public Coord Max; }
 [DllImport("kernel32.dll",SetLastError=true)] public static extern bool AttachConsole(uint id);
 [DllImport("kernel32.dll")] public static extern bool FreeConsole();
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern IntPtr CreateFile(string path,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
 [DllImport("kernel32.dll",SetLastError=true)] public static extern bool GetConsoleScreenBufferInfo(IntPtr h,out Info info);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool ReadConsoleOutputCharacter(IntPtr h,StringBuilder text,uint length,Coord start,out uint read);
 [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
}
'@
$report = @{found=$true;owner=$owner.ProcessId;child=$children[0].ProcessId;readable=$false}
[ZeusConsoleProbe]::FreeConsole() | Out-Null
try {
 if (![ZeusConsoleProbe]::AttachConsole($children[0].ProcessId)) { throw 'Cannot attach to the owned fixture console.' }
 $handle = [ZeusConsoleProbe]::CreateFile('CONOUT$',[uint32]2147483648,3,[IntPtr]::Zero,3,0,[IntPtr]::Zero)
 try {
  $info = New-Object ZeusConsoleProbe+Info
  if (![ZeusConsoleProbe]::GetConsoleScreenBufferInfo($handle,[ref]$info)) { throw 'Cannot inspect the owned fixture console.' }
  $start = New-Object ZeusConsoleProbe+Coord
  $start.X=0; $start.Y=$info.Window.Top
  $length=[Math]::Min(32768,$info.Size.X*($info.Window.Bottom-$info.Window.Top+1))
  $text = New-Object Text.StringBuilder($length); [uint32]$read=0
  if (![ZeusConsoleProbe]::ReadConsoleOutputCharacter($handle,$text,$length,$start,[ref]$read)) { throw 'Cannot read the owned fixture console.' }
  $report.readable=$true; $report.text=$text.ToString(); $report.width=$info.Size.X
 } finally { [ZeusConsoleProbe]::CloseHandle($handle) | Out-Null }
} catch { $report.error=$_.Exception.Message }
finally { [ZeusConsoleProbe]::FreeConsole() | Out-Null }
$report | ConvertTo-Json | Set-Content -LiteralPath $ReportPath -Encoding UTF8
