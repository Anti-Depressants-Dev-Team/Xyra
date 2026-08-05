param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Executable,

    [Parameter(Position = 1, ValueFromRemainingArguments = $true)]
    [string[]]$RemainingArguments
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$runtime = Join-Path $workspace "obs-runtime"
$obsDll = Join-Path $runtime "obs.dll"
$capturePlugin = Join-Path $runtime "xyra-plugins\64bit\win-capture.dll"

if (-not (Test-Path -LiteralPath $obsDll -PathType Leaf) -or
    -not (Test-Path -LiteralPath $capturePlugin -PathType Leaf)) {
    Write-Host "Staging Xyra's OBS runtime for local development..."
    & (Join-Path $PSScriptRoot "stage-obs-runtime.ps1") -Destination "obs-runtime"
}

$env:XYRA_OBS_RUNTIME = $runtime
$env:PATH = "$env:PATH;$runtime"

$processArguments = @{}
if ($null -ne $RemainingArguments -and $RemainingArguments.Length -gt 0) {
    $processArguments.ArgumentList = $RemainingArguments
}
$process = Start-Process -FilePath $Executable -NoNewWindow -PassThru -Wait @processArguments
exit $process.ExitCode
