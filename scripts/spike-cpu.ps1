# FASE 0 — medición de CPU del spike. Se borra con el spike.
#
# La app no puede medir su propio consumo de forma honesta: el criterio del
# diseño es "<30% de un core", y eso hay que verlo desde fuera.
#
# Uso:  pwsh -File scripts/spike-cpu.ps1 -Seconds 30
#
# Muestrea el tiempo de CPU del proceso de la app y lo normaliza a "porcentaje
# de un core", que es la unidad en la que está escrito el criterio. En una
# máquina de N cores, el Task Manager mostraría este número dividido por N.

param(
    [int]$Seconds = 30,
    [string]$ProcessName = "tauri-app"
)

$procs = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
if (-not $procs) {
    Write-Error "No hay ningún proceso '$ProcessName'. Arranca el spike con 'cargo tauri dev' primero."
    exit 1
}

Write-Output "Muestreando $($procs.Count) proceso(s) '$ProcessName' durante $Seconds s…"
Write-Output "Pon el spike a medir ANTES de que termine la cuenta."
Write-Output ""

# El WebView corre en procesos hijos separados (msedgewebview2). El coste de
# getImageData y del canvas vive ahí, no en el proceso Rust: contar solo el
# padre subestimaría el consumo real de la ruta híbrida.
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
Write-Output "Ventana de muestreo : $([math]::Round($wallSeconds,1)) s"
Write-Output "CPU consumida       : $([math]::Round($cpuSeconds,2)) s"
Write-Output "Porcentaje de 1 core: $([math]::Round($percentOfOneCore,1)) %   <-- criterio: <30 %"
Write-Output "Porcentaje total    : $([math]::Round($percentOfOneCore/$cores,1)) %  (de $cores cores)"
Write-Output ""

if ($percentOfOneCore -lt 30.0) {
    Write-Output "PASA el criterio de CPU."
} else {
    Write-Output "NO pasa el criterio de CPU. Repliegues en orden: recorte a ROI, decode en WASM, nokhwa nativo."
}
