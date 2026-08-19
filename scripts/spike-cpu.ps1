# PHASE 0 - CPU measurement for the spike. Deleted along with the spike.
#
# The app cannot honestly measure its own consumption: the design's criterion is
# "under 30% of one core", and that has to be seen from outside.
#
# Usage:  pwsh -File scripts/spike-cpu.ps1 -Seconds 30
#
# Samples the app process's CPU time and normalises it to "percentage of one
# core", which is the unit the criterion is written in. On an N-core machine,
# Task Manager would show this number divided by N.

param(
    [int]$Seconds = 30,
    [string]$ProcessName = "tauri-app"
)

$procs = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
if (-not $procs) {
    Write-Error "No process named '$ProcessName'. Start the spike with 'cargo tauri dev' first."
    exit 1
}

Write-Output "Sampling $($procs.Count) '$ProcessName' process(es) for $Seconds s..."
Write-Output "Get the spike measuring BEFORE the countdown ends."
Write-Output ""

# The WebView runs in separate child processes (msedgewebview2). The cost of
# getImageData and the canvas lives there, not in the Rust process: counting only
# the parent would understate the hybrid path's real consumption.
$names = @($ProcessName, "msedgewebview2")

function Get-CpuSnapshot {
    $total = 0.0
    foreach ($n in $names) {
        foreach ($p in (Get-Process -Name $n -ErrorAction SilentlyContinue)) {
            $total += $p.TotalProcessorTime.TotalSeconds
        }
    }
    return $total
}

$t0 = Get-CpuSnapshot
$wall0 = Get-Date
Start-Sleep -Seconds $Seconds
$t1 = Get-CpuSnapshot
$wall1 = Get-Date

$cpuSeconds = $t1 - $t0
$wallSeconds = ($wall1 - $wall0).TotalSeconds
$percentOfOneCore = ($cpuSeconds / $wallSeconds) * 100.0
$cores = [Environment]::ProcessorCount

Write-Output ""
Write-Output "Sampling window   : $([math]::Round($wallSeconds,1)) s"
Write-Output "CPU consumed      : $([math]::Round($cpuSeconds,2)) s"
Write-Output "Percent of 1 core : $([math]::Round($percentOfOneCore,1)) %   <-- criterion: under 30 %"
Write-Output "Percent of total  : $([math]::Round($percentOfOneCore/$cores,1)) %  (of $cores cores)"
Write-Output ""

if ($percentOfOneCore -lt 30.0) {
    Write-Output "PASSES the CPU criterion."
} else {
    Write-Output "FAILS the CPU criterion. Fallbacks in order: crop to ROI, decode in WASM, native nokhwa."
}
