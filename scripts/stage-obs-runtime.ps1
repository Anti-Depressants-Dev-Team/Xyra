param(
    [string]$SourceRoot,
    [string]$Destination = "obs-runtime"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$obsVersion = "32.2.1"
$workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$destinationPath = [System.IO.Path]::GetFullPath((Join-Path $workspace $Destination))
$workspacePrefix = $workspace.TrimEnd('\') + '\'
if (-not $destinationPath.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OBS runtime destination must stay inside the Xyra workspace: $destinationPath"
}

if (-not $SourceRoot) {
    $installed = Join-Path $env:ProgramFiles "obs-studio"
    if (Test-Path -LiteralPath (Join-Path $installed "bin\64bit\obs.dll") -PathType Leaf) {
        $SourceRoot = $installed
    } else {
        $downloadRoot = Join-Path ([System.IO.Path]::GetTempPath()) "xyra-obs-$obsVersion"
        $archive = Join-Path $downloadRoot "OBS-Studio-$obsVersion-Windows-x64.zip"
        $expanded = Join-Path $downloadRoot "expanded"
        New-Item -ItemType Directory -Path $downloadRoot -Force | Out-Null
        if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
            $url = "https://github.com/obsproject/obs-studio/releases/download/$obsVersion/OBS-Studio-$obsVersion-Windows-x64.zip"
            Invoke-WebRequest -Uri $url -OutFile $archive
        }
        if (-not (Test-Path -LiteralPath $expanded -PathType Container)) {
            Expand-Archive -LiteralPath $archive -DestinationPath $expanded
        }
        $obsDll = Get-ChildItem -LiteralPath $expanded -Recurse -File -Filter "obs.dll" |
            Where-Object { $_.Directory.Name -eq "64bit" -and $_.Directory.Parent.Name -eq "bin" } |
            Select-Object -First 1
        if (-not $obsDll) {
            throw "The official OBS archive did not contain bin\64bit\obs.dll."
        }
        $SourceRoot = $obsDll.Directory.Parent.Parent.FullName
    }
}

$sourcePath = (Resolve-Path -LiteralPath $SourceRoot).Path
$binarySource = Join-Path $sourcePath "bin\64bit"
$pluginSource = Join-Path $sourcePath "obs-plugins\64bit"
$dataSource = Join-Path $sourcePath "data"
foreach ($required in @(
    (Join-Path $binarySource "obs.dll"),
    (Join-Path $binarySource "obs-ffmpeg-mux.exe"),
    (Join-Path $pluginSource "win-capture.dll"),
    (Join-Path $pluginSource "win-wasapi.dll"),
    (Join-Path $dataSource "libobs")
)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "The OBS runtime is incomplete: $required"
    }
}

if (Test-Path -LiteralPath $destinationPath) {
    Remove-Item -LiteralPath $destinationPath -Recurse -Force
}
New-Item -ItemType Directory -Path $destinationPath -Force | Out-Null

# libobs plugins share FFmpeg and graphics dependencies from OBS' binary folder.
Get-ChildItem -LiteralPath $binarySource -File -Filter "*.dll" |
    Copy-Item -Destination $destinationPath
foreach ($helper in @("obs-ffmpeg-mux.exe", "obs-nvenc-test.exe", "obs-amf-test.exe", "obs-qsv-test.exe")) {
    $helperPath = Join-Path $binarySource $helper
    if (Test-Path -LiteralPath $helperPath -PathType Leaf) {
        Copy-Item -LiteralPath $helperPath -Destination $destinationPath
    }
}

$plugins = @(
    "obs-ffmpeg",
    "obs-filters",
    "obs-nvenc",
    "obs-outputs",
    "obs-qsv11",
    "obs-x264",
    "win-capture",
    "win-wasapi"
)
# Keep backend modules out of OBS' automatic ../../obs-plugins discovery path.
# libobs-wrapper adds this curated directory explicitly, so modules load once.
$pluginDestination = Join-Path $destinationPath "xyra-plugins\64bit"
New-Item -ItemType Directory -Path $pluginDestination -Force | Out-Null
foreach ($plugin in $plugins) {
    $pluginDll = Join-Path $pluginSource "$plugin.dll"
    if (Test-Path -LiteralPath $pluginDll -PathType Leaf) {
        Copy-Item -LiteralPath $pluginDll -Destination $pluginDestination
    }
}

$dataDestination = Join-Path $destinationPath "data"
New-Item -ItemType Directory -Path $dataDestination -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $dataSource "libobs") -Destination $dataDestination -Recurse
$pluginDataDestination = Join-Path $destinationPath "data\obs-plugins"
New-Item -ItemType Directory -Path $pluginDataDestination -Force | Out-Null
foreach ($plugin in $plugins) {
    $pluginData = Join-Path $dataSource "obs-plugins\$plugin"
    if (Test-Path -LiteralPath $pluginData -PathType Container) {
        Copy-Item -LiteralPath $pluginData -Destination $pluginDataDestination -Recurse
    }
}

# Debug symbols are not needed by the end-user runtime and add several MiB.
Get-ChildItem -LiteralPath $destinationPath -Recurse -File -Filter "*.pdb" |
    Remove-Item -Force

Write-Host "Staged OBS Studio $obsVersion runtime at $destinationPath"
