<#
.SYNOPSIS
  Measures idle memory of a windowed app across its FULL process tree.

.DESCRIPTION
  Three things make the naive measurement wrong, and all three were hit during M0:

  1. CIM returns UInt32 PIDs. Hashtable lookups use Object.Equals, and
     UInt32(x).Equals(Int32(x)) is FALSE - so a descendant walk keyed on raw CIM
     values silently stops at the root and reports a flatteringly small number.

  2. WebView2 shares one browser process per user-data-folder. If a previous
     instance leaked, a fresh launch ATTACHES to the orphan's browser process
     instead of spawning its own. The new tree then has no webview child at all
     and the measurement misses ~120MB. Hence the stale-instance guard, and
     identifying webview processes by WebView2's own --webview-exe-name tag
     rather than trusting parentage.

  3. WorkingSetSize counts SHARED pages in every process that maps them. WebView2
     runs 6 processes sharing a large runtime image, so summing working set across
     the tree double-counts it roughly 4.5x (measured: 327.8MB working set vs
     72.7MB private). Summing working set across a process tree is not a
     meaningful number.

  PRIMARY METRIC: private working set - physical memory private to the process,
  summed across the tree. This is what Task Manager's Memory column reports and
  what a user means by "this app uses X MB". It comes from perf counters
  (Win32_PerfRawData_PerfProc_Process.WorkingSetPrivate), not Win32_Process.

  It slightly understates true cost by excluding shared runtime pages - but those
  pages are genuinely shared with any other WebView2 app on the system, so
  charging them wholly to us would overstate it. Private working set is the
  defensible choice. Private commit is reported alongside as a leak signal.

.EXAMPLE
  .\measure-rss.ps1 -ExePath ..\target\release\deepslate.exe -Json out.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ExePath,
    [int[]]$SampleAtSeconds = @(5, 15, 30, 60),
    [int]$BudgetMB = 150,
    [string]$Json
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $ExePath)) { throw "Executable not found: $ExePath" }
$ExePath = (Resolve-Path $ExePath).Path
$exeName = Split-Path $ExePath -Leaf

# --- Pre-flight: a leaked instance silently corrupts the result (see note 2) ---
$stale = @(Get-CimInstance Win32_Process -Filter "Name = '$exeName'")
if ($stale.Count -gt 0) {
    throw ("$($stale.Count) instance(s) of $exeName already running (PIDs: $($stale.ProcessId -join ', ')). " +
           "A fresh launch would attach to their WebView2 browser process and under-report memory. Close them first.")
}

function Get-AppProcesses {
    param([int]$RootPid, [string]$ExeName)

    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, WorkingSetSize, PrivatePageCount, CommandLine

    $byParent = @{}
    $byPid    = @{}
    foreach ($p in $all) {
        $ppid = [int]$p.ParentProcessId    # cast: see note 1
        $cpid = [int]$p.ProcessId
        $byPid[$cpid] = $p
        if (-not $byParent.ContainsKey($ppid)) { $byParent[$ppid] = [System.Collections.ArrayList]::new() }
        [void]$byParent[$ppid].Add($p)
    }

    # Seed with the root plus any WebView2 process WebView2 itself attributes to
    # our exe; the walk then picks up their descendants.
    $seeds = [System.Collections.ArrayList]::new()
    [void]$seeds.Add([int]$RootPid)
    foreach ($p in $all) {
        if ($p.Name -eq 'msedgewebview2.exe' -and $p.CommandLine -like "*--webview-exe-name=$ExeName*") {
            [void]$seeds.Add([int]$p.ProcessId)
        }
    }

    $seen = @{}
    $queue = [System.Collections.Queue]::new()
    foreach ($s in $seeds) { $queue.Enqueue($s) }
    $out = [System.Collections.ArrayList]::new()
    while ($queue.Count -gt 0) {
        $cur = [int]$queue.Dequeue()
        if ($seen.ContainsKey($cur)) { continue }
        $seen[$cur] = $true
        if ($byPid.ContainsKey($cur)) { [void]$out.Add($byPid[$cur]) }
        if ($byParent.ContainsKey($cur)) {
            foreach ($c in $byParent[$cur]) { $queue.Enqueue([int]$c.ProcessId) }
        }
    }
    Write-Verbose "Get-AppProcesses: seeds=$($seeds.Count) collected=$($out.Count) of $($all.Count) system processes"
    return $out.ToArray()
}

function Get-PrivateWorkingSets {
    # PID -> private working set bytes. Not available from Win32_Process.
    $map = @{}
    Get-CimInstance Win32_PerfRawData_PerfProc_Process -Property IDProcess, WorkingSetPrivate |
        ForEach-Object { $map[[int]$_.IDProcess] = [uint64]$_.WorkingSetPrivate }
    return $map
}

Write-Host "Launching $exeName ..."
$proc = Start-Process -FilePath $ExePath -PassThru
$startedAt = Get-Date

$samples = @()
try {
    foreach ($t in ($SampleAtSeconds | Sort-Object)) {
        $wait = $t - ((Get-Date) - $startedAt).TotalSeconds
        if ($wait -gt 0) { Start-Sleep -Seconds $wait }

        if (-not (Get-Process -Id $proc.Id -ErrorAction SilentlyContinue)) {
            throw "Process exited before the ${t}s sample - it crashed or failed to start."
        }

        $tree = @(Get-AppProcesses -RootPid $proc.Id -ExeName $exeName)
        if (-not ($tree | Where-Object { $_.Name -like '*webview*' -or $_.Name -like '*WebKit*' })) {
            throw "No webview process attributed to $exeName at t+${t}s. The number would be meaningless."
        }

        $pws = Get-PrivateWorkingSets
        $missing = @($tree | Where-Object { -not $pws.ContainsKey([int]$_.ProcessId) })
        if ($missing.Count -gt 0) {
            throw ("No perf counter for PID(s) $($missing.ProcessId -join ', '). " +
                   "Private working set would be understated; refusing to report a partial figure.")
        }

        $privMB   = [math]::Round((($tree | ForEach-Object { $pws[[int]$_.ProcessId] } | Measure-Object -Sum).Sum) / 1MB, 1)
        $wsMB     = [math]::Round((($tree | Measure-Object WorkingSetSize   -Sum).Sum) / 1MB, 1)
        $commitMB = [math]::Round((($tree | Measure-Object PrivatePageCount -Sum).Sum) / 1MB, 1)

        $breakdown = $tree | Group-Object Name | ForEach-Object {
            [pscustomobject]@{
                name      = $_.Name
                count     = $_.Count
                privateMB = [math]::Round((($_.Group | ForEach-Object { $pws[[int]$_.ProcessId] } | Measure-Object -Sum).Sum) / 1MB, 1)
                wsMB      = [math]::Round((($_.Group | Measure-Object WorkingSetSize -Sum).Sum) / 1MB, 1)
            }
        } | Sort-Object privateMB -Descending

        $samples += [pscustomobject]@{
            atSeconds = $t; privateWorkingSetMB = $privMB; workingSetMB = $wsMB
            privateCommitMB = $commitMB; processes = $tree.Count; breakdown = $breakdown
        }

        Write-Host ("  t+{0,-3}s  private {1,7} MB   (ws {2} MB, commit {3} MB, {4} processes)" -f $t, $privMB, $wsMB, $commitMB, $tree.Count)
        foreach ($b in $breakdown) {
            Write-Host ("            {0,-22} x{1,-3} private {2,7} MB   ws {3,7} MB" -f $b.name, $b.count, $b.privateMB, $b.wsMB)
        }
    }
}
finally {
    # Kill the whole tree - a surviving browser process corrupts the NEXT run (note 2).
    foreach ($p in @(Get-AppProcesses -RootPid $proc.Id -ExeName $exeName)) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

$idleMB = $samples[-1].privateWorkingSetMB
$pass = $idleMB -le $BudgetMB

Write-Host ""
Write-Host ("IDLE (private working set) : {0} MB" -f $idleMB)
Write-Host ("BUDGET                     : {0} MB" -f $BudgetMB)
Write-Host ("HEADROOM                   : {0} MB" -f [math]::Round($BudgetMB - $idleMB, 1))
Write-Host ("RESULT                     : {0}" -f $(if ($pass) { "PASS" } else { "FAIL - over by $([math]::Round($idleMB - $BudgetMB,1)) MB" }))

$result = [pscustomobject]@{
    exe = $ExePath; measuredAt = (Get-Date).ToString('o')
    metric = 'privateWorkingSet'; idleMB = $idleMB; budgetMB = $BudgetMB
    pass = $pass; samples = $samples
}

if ($Json) {
    $result | ConvertTo-Json -Depth 6 | Out-File -FilePath $Json -Encoding utf8
    Write-Host "wrote $Json"
}

if (-not $pass) { exit 1 }
