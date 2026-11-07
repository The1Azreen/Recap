<#
Builds whisper.cpp with the Vulkan backend so Recap can transcribe on AMD (and Intel/NVIDIA) GPUs
on Windows. whisper.cpp does not publish a prebuilt Windows Vulkan binary, so we compile one.

Needs: Git, CMake, Visual Studio 2022 with "Desktop development with C++", and the Vulkan SDK
  winget install --id KhronosGroup.VulkanSDK -e

Usage:  powershell -ExecutionPolicy Bypass -File scripts\build_whisper_vulkan.ps1
#>
param(
    [string]$Tag = "v1.9.5",
    [string]$Dest = $(if ($env:RECAP_HOME) { "$env:RECAP_HOME\tools\whisper.cpp" } else { "$HOME\Recap\tools\whisper.cpp" })
)
$ErrorActionPreference = "Stop"

# A fresh Vulkan SDK install only sets VULKAN_SDK machine-wide; pick it up without a new shell.
if (-not $env:VULKAN_SDK) { $env:VULKAN_SDK = [Environment]::GetEnvironmentVariable("VULKAN_SDK", "Machine") }
if (-not $env:VULKAN_SDK -or -not (Test-Path "$env:VULKAN_SDK\Bin\glslc.exe")) {
    throw "Vulkan SDK not found. Install it with: winget install --id KhronosGroup.VulkanSDK -e"
}
$env:PATH = "$env:VULKAN_SDK\Bin;$env:PATH"

$src = Join-Path $env:TEMP "recap-whisper.cpp-$Tag"
if (-not (Test-Path $src)) {
    git clone --depth 1 --branch $Tag https://github.com/ggml-org/whisper.cpp.git $src
    if ($LASTEXITCODE) { throw "git clone failed" }
}

cmake -S $src -B "$src\build" -DGGML_VULKAN=ON -DBUILD_SHARED_LIBS=ON -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_EXAMPLES=ON
if ($LASTEXITCODE) { throw "cmake configure failed" }
cmake --build "$src\build" --config Release --parallel --target whisper-cli
if ($LASTEXITCODE) { throw "cmake build failed" }

New-Item -ItemType Directory -Force $Dest | Out-Null
Copy-Item "$src\build\bin\Release\*" $Dest -Force
Write-Host "whisper.cpp ($Tag, Vulkan) installed to $Dest"
