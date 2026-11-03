# Scores rawr's engine against WinRAR on one or more folders or files:
#   pwsh scripts/bench.ps1 -Path <folder or file>[,<another>] [-Runs 3] [-Csv results.csv]
# Each path is one data set; join siblings with | to put them in one archive.
# A score of 100 matches WinRAR and higher is better, so 125 means WinRAR needs
# 1.25x what rawr needs. Each score is a geometric mean over the rows below it:
#   speed     wall time on every row
#   resources peak memory and CPU time on every row, weighted equally
#   size      archive size on the create rows
#   overall   the three together, weighted 70% speed, 20% resources, 10% size
# Extract and test read the same archive on both sides, made by WinRAR. Every
# archive rawr creates is checked with UnRAR.
param(
  [Parameter(Mandatory)][string[]]$Path,
  [int]$Runs = 3,
  [string]$Csv,
  [string]$WinRAR = 'C:\Program Files\WinRAR'
)
$ErrorActionPreference = 'Stop'
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$rar = Join-Path $WinRAR 'Rar.exe'
$unrar = Join-Path $WinRAR 'UnRAR.exe'
$root = Split-Path $PSScriptRoot -Parent
$rawr = Join-Path $root 'target\release\examples\bench.exe'

cargo build --release --example bench --manifest-path (Join-Path $root 'Cargo.toml') 2>&1 | Out-Null
if ($LASTEXITCODE) { throw 'could not build the bench example' }

$work = Join-Path ([IO.Path]::GetTempPath()) "rawr-bench-$PID"
New-Item -ItemType Directory $work | Out-Null
$made = Join-Path $work 'made.rar'
$reference = Join-Path $work 'reference.rar'
$out = Join-Path $work 'out'
$rows = [Collections.Generic.List[object]]::new()

function Clear-Path($target) {
  if (Test-Path $target) { Remove-Item $target -Recurse -Force }
}

function Measure-Once($exe, $arguments) {
  $info = [Diagnostics.ProcessStartInfo]::new($exe)
  foreach ($argument in $arguments) { $info.ArgumentList.Add($argument) }
  $info.UseShellExecute = $false
  $info.CreateNoWindow = $true
  $info.WorkingDirectory = $from
  $info.RedirectStandardOutput = $true
  $info.RedirectStandardError = $true
  $clock = [Diagnostics.Stopwatch]::StartNew()
  $process = [Diagnostics.Process]::Start($info)
  $null = $process.StandardOutput.ReadToEndAsync()
  $null = $process.StandardError.ReadToEndAsync()
  $peak = 0
  $cpu = 0
  while (-not $process.HasExited) {
    try {
      $process.Refresh()
      $peak = [Math]::Max($peak, $process.PeakWorkingSet64)
      $cpu = $process.TotalProcessorTime.TotalSeconds
    } catch {}
    Start-Sleep -Milliseconds 15
  }
  $process.WaitForExit()
  $seconds = $clock.Elapsed.TotalSeconds
  try { $cpu = $process.TotalProcessorTime.TotalSeconds } catch {}
  if ($process.ExitCode) { throw "$exe $arguments failed with exit code $($process.ExitCode)" }
  [pscustomobject]@{ Seconds = $seconds; Cpu = $cpu; MB = $peak / 1MB }
}

function Measure-Best($prepare, $exe, $arguments) {
  $results = 1..$Runs | ForEach-Object { & $prepare; Measure-Once $exe $arguments }
  [pscustomobject]@{
    Seconds = ($results | Measure-Object Seconds -Minimum).Minimum
    Cpu = ($results | Measure-Object Cpu -Minimum).Minimum
    MB = ($results | Measure-Object MB -Maximum).Maximum
  }
}

function Add-Row($set, $operation, $mine, $theirs, $mineSize, $theirSize) {
  $rows.Add([pscustomobject]@{
    Set = $set; Operation = $operation
    RawrSeconds = $mine.Seconds; WinRARSeconds = $theirs.Seconds
    RawrCpu = $mine.Cpu; WinRARCpu = $theirs.Cpu
    RawrMB = $mine.MB; WinRARMB = $theirs.MB
    RawrSizeMB = $mineSize; WinRARSizeMB = $theirSize
  })
}

# The archive WinRAR made stays in $reference, for the extract and test rows.
function Compare-Create($set, $operation, $level, $solid) {
  $mine = Measure-Best { Clear-Path $made } $rawr (@('create', $made, $level, $solid) + $source)
  $mineSize = (Get-Item $made).Length / 1MB
  $null = Measure-Once $unrar @('t', '-idq', $made)
  $flags = @('a', '-r', '-ma5', '-idq', "-m$level") + ($solid -eq '1' ? @('-s') : @())
  $theirs = Measure-Best { Clear-Path $reference } $rar ($flags + @($reference) + $pattern)
  $theirSize = (Get-Item $reference).Length / 1MB
  Add-Row $set $operation $mine $theirs $mineSize $theirSize
}

function Compare-Read($set, $label) {
  $mine = Measure-Best { Clear-Path $out } $rawr @('extract', $reference, $out)
  $theirs = Measure-Best { Clear-Path $out; New-Item -ItemType Directory $out | Out-Null } $unrar @('x', '-idq', '-o+', $reference, "$out\")
  Add-Row $set "${label}extract" $mine $theirs
  $mine = Measure-Best { } $rawr @('test', $reference)
  $theirs = Measure-Best { } $unrar @('t', '-idq', $reference)
  Add-Row $set "${label}test" $mine $theirs
}

function Get-Score($ratios) {
  $sum = 0.0
  foreach ($ratio in $ratios) { $sum += [Math]::Log($ratio) }
  100 / [Math]::Exp($sum / @($ratios).Count)
}

function Get-Ratio($mine, $theirs) { [Math]::Max($mine, 0.001) / [Math]::Max($theirs, 0.001) }

try {
  foreach ($one in $Path) {
    $items = @($one -split '\|' | ForEach-Object { Get-Item $_ })
    $from = $items[0].PSIsContainer ? $items[0].Parent.FullName : $items[0].DirectoryName
    $source = @($items | ForEach-Object Name)
    $pattern = @($items | ForEach-Object { $_.PSIsContainer ? "$($_.Name)\" : $_.Name })
    $set = $source -join '+'
    Compare-Create $set 'fast' '1' '0'
    foreach ($level in 2, 4, 5) { Compare-Create $set "level $level" "$level" '0' }
    Compare-Create $set 'normal' '3' '0'
    Compare-Read $set ''
    Compare-Create $set 'solid fast' '1' '1'
    Compare-Create $set 'solid level 5' '5' '1'
    Compare-Create $set 'solid normal' '3' '1'
    Compare-Read $set 'solid '
  }

  $speed = Get-Score ($rows | ForEach-Object { Get-Ratio $_.RawrSeconds $_.WinRARSeconds })
  $memory = Get-Score ($rows | ForEach-Object { Get-Ratio $_.RawrMB $_.WinRARMB })
  $cpu = Get-Score ($rows | ForEach-Object { Get-Ratio $_.RawrCpu $_.WinRARCpu })
  $resources = [Math]::Sqrt($memory * $cpu)
  $size = Get-Score ($rows | Where-Object RawrSizeMB | ForEach-Object { $_.RawrSizeMB / $_.WinRARSizeMB })
  $slower = @($rows | Where-Object { $_.RawrSeconds -gt $_.WinRARSeconds }).Count

  $overall = [Math]::Pow($speed, 0.7) * [Math]::Pow($resources, 0.2) * [Math]::Pow($size, 0.1)

  "best of $Runs. 100 matches WinRAR, higher is better"
  ''
  'OVERALL    {0,5:N1}   70% speed, 20% resources, 10% size' -f $overall
  'SPEED      {0,5:N1}   faster on {1} of {2} rows' -f $speed, ($rows.Count - $slower), $rows.Count
  'RESOURCES  {0,5:N1}   memory {1:N1}, CPU time {2:N1}' -f $resources, $memory, $cpu
  'SIZE       {0,5:N1}' -f $size
  ''
  '{0,-8} {1,-14} | {2,7} {3,7} {4,6} | {5,6} {6,6} {7,5} | {8,6} {9,6} {10,5} | {11,7} {12,7} {13,5}' -f 'set', 'operation', 'rawr s', 'WinRAR', 'speed', 'rawrMB', 'WinRAR', 'mem', 'rawrCPU', 'WinRAR', 'cpu', 'rawrMB', 'WinRAR', 'size'
  foreach ($row in $rows) {
    $sizes = $row.RawrSizeMB ? ('{0,7:N1} {1,7:N1} {2,5:N0}' -f $row.RawrSizeMB, $row.WinRARSizeMB, (100 * $row.WinRARSizeMB / $row.RawrSizeMB)) : ''
    '{0,-8} {1,-14} | {2,7:N2} {3,7:N2} {4,6:N0} | {5,6:N0} {6,6:N0} {7,5:N0} | {8,6:N2} {9,6:N2} {10,5:N0} | {11}' -f $row.Set, $row.Operation,
      $row.RawrSeconds, $row.WinRARSeconds, (100 / (Get-Ratio $row.RawrSeconds $row.WinRARSeconds)),
      $row.RawrMB, $row.WinRARMB, (100 / (Get-Ratio $row.RawrMB $row.WinRARMB)),
      $row.RawrCpu, $row.WinRARCpu, (100 / (Get-Ratio $row.RawrCpu $row.WinRARCpu)), $sizes
  }
  if ($Csv) { $rows | Export-Csv $Csv -NoTypeInformation }
} finally {
  Clear-Path $work
}
