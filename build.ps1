#!/usr/bin/env pwsh
<#
.SYNOPSIS
    EasyTier Windows build script (PowerShell equivalent of build.sh)

.DESCRIPTION
    支持 debug / release / release-small / official 四种编译模式，
    无参数运行进入交互式菜单，传参数则进入 CLI 模式。

.PARAMETER Method
    编译方式: debug | release | release-small | official

.PARAMETER Bin
    二进制名称: easytier-core | easytier-cli

.PARAMETER Target
    Rust target triple，例如 x86_64-pc-windows-msvc

.PARAMETER Features
    覆盖 cargo features，例如 "jemalloc" 或 "mimalloc"

.PARAMETER Clean
    构建前清理旧产物

.PARAMETER Offline
    使用 cargo offline 模式

.PARAMETER Upx
    强制启用 UPX 压缩

.PARAMETER NoUpx
    禁用 UPX 压缩

.PARAMETER Menu
    强制进入交互式菜单模式

.PARAMETER Help
    显示帮助信息
#>

param(
    [ValidateSet("debug", "release", "release-small", "official")]
    [string]$Method = "release",

    [ValidateSet("easytier-core", "easytier-cli")]
    [string]$Bin = "easytier-core",

    [string]$Target = "",

    [string]$Features = "",

    [switch]$Clean,

    [switch]$Offline,

    [switch]$Upx,

    [switch]$NoUpx,

    [switch]$Menu,

    [switch]$Help
)

$ErrorActionPreference = "Stop"

# ── Global color definitions ────────────────────────────────────────────────────
$ESC = [char]27
$C_RESET  = "${ESC}[0m"
$C_BOLD   = "${ESC}[1m"
$C_RED    = "${ESC}[31m"
$C_GREEN  = "${ESC}[32m"
$C_YELLOW = "${ESC}[33m"
$C_BLUE   = "${ESC}[34m"
$C_CYAN   = "${ESC}[36m"

# Suppress ANSI when NO_COLOR is set or output is redirected
$supportsVT = $false
try { $supportsVT = $Host.UI.SupportsVirtualTerminal } catch { }
if ($env:NO_COLOR -or -not $supportsVT) {
    $C_RESET = $C_BOLD = $C_RED = $C_GREEN = $C_YELLOW = $C_BLUE = $C_CYAN = ""
}

# ── Global build state ──────────────────────────────────────────────────────────
$RootDir    = Split-Path -Parent $MyInvocation.MyCommand.Path
$Toolchain  = if ($env:TOOLCHAIN) { $env:TOOLCHAIN } else { "1.95" }
$ProtocBin  = $env:PROTOC
$UseUpx     = "auto"
$ExtraCargo = @()
$BuildEnv   = @{}
$CleanBuild = if ($Clean) { 1 } else { 0 }
$OfflineBld = if ($Offline) { 1 } else { 0 }

# ── Helper: colored text fragments ──────────────────────────────────────────────

function c($color, $text) {
    return "$color$text$C_RESET"
}

# ── Logging helpers ─────────────────────────────────────────────────────────────

function info($msg)    { Write-Host "[INFO]  $msg" }
function step($msg)    { Write-Host (c $C_CYAN "[STEP]") " $msg" }
function warn($msg)    { Write-Host (c $C_YELLOW "[WARN]") " $msg" }
function success($msg) { Write-Host (c $C_GREEN "[OK]") "   $msg" }
function die($msg)     { Write-Host (c $C_RED "[ERROR]") " $msg"; exit 1 }

# ── Usage ───────────────────────────────────────────────────────────────────────

function Show-Usage {
@'
Usage:
  .\build.ps1 [options]

Options:
  -Method <name>           Build method: debug | release | release-small | official
  -Bin <name>              Binary name: easytier-core | easytier-cli
  -Target <triple>         Rust target triple, for example x86_64-pc-windows-msvc
  -Features <list>         Override cargo features, for example "jemalloc" or "mimalloc"
  -Clean                   Clean the easytier build artifacts before building
  -Offline                 Run cargo in offline mode
  -Upx                     Force UPX compression in official mode
  -NoUpx                   Disable UPX compression in official mode
  -Menu                    Force interactive menu mode
  -Help                    Show this help message

Behavior:
  - Running .\build.ps1 without arguments enters interactive menu mode
  - Running .\build.ps1 with arguments keeps the non-interactive CLI mode

Methods:
  debug           cargo build
  release         cargo build --release
  release-small   cargo build --profile release-small
  official        Emulate the project CI flow (--release + allocator + UPX)

Examples:
  .\build.ps1
  .\build.ps1 -Method release -Bin easytier-core
  .\build.ps1 -Method release-small -Bin easytier-core -Target x86_64-pc-windows-msvc
  .\build.ps1 -Method official -Bin easytier-core -Target x86_64-pc-windows-msvc -Clean
  .\build.ps1 -Method official -Bin easytier-cli -Target x86_64-pc-windows-msvc -Features mimalloc
'@
}

# ── Detection helpers ───────────────────────────────────────────────────────────

function Get-HostTarget {
    $out = & rustc "+$Toolchain" -vV 2>&1 | Out-String
    if ($out -match 'host:\s*(\S+)') {
        return $Matches[1]
    }
    die "Failed to detect host target from rustc"
}

function Find-Protoc {
    if ($env:PROTOC -and (Test-Path $env:PROTOC -PathType Leaf)) {
        return $env:PROTOC
    }
    $cmd = Get-Command protoc -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $paths = @(
        "$env:TEMP\protoc-35.1\bin\protoc.exe",
        "$env:LOCALAPPDATA\protoc-35.1\bin\protoc.exe"
    )
    foreach ($p in $paths) {
        if (Test-Path $p -PathType Leaf) { return $p }
    }
    return $null
}

function Assert-Protoc {
    if ($ProtocBin -and (Test-Path $ProtocBin -PathType Leaf)) { return }
    $found = Find-Protoc
    if ($found) {
        $script:ProtocBin = $found
        return
    }
    die "protoc not found; set `$env:PROTOC or install protoc 35.1"
}

# ── Build helpers ───────────────────────────────────────────────────────────────

function Get-EnvKey($Target) {
    return ($Target.ToUpper() -replace '-', '_')
}

function Get-VarKey($Target) {
    return ($Target -replace '-', '_')
}

function Get-DefaultOfficialFeatures($Target) {
    if ($Target -match '^(riscv64|loongarch64|aarch64)') {
        return "mimalloc"
    }
    if ($Target -match '(freebsd|windows)') {
        return "mimalloc"
    }
    return "jemalloc"
}

function Get-CargoOutputDir($Method, $Target) {
    $base = Join-Path (Join-Path $RootDir "target") $Target
    switch ($Method) {
        "debug"         { return Join-Path $base "debug" }
        "release"       { return Join-Path $base "release" }
        "official"      { return Join-Path $base "release" }
        "release-small" { return Join-Path $base "release-small" }
    }
}

function Get-OfficialArtifactDir($Target) {
    switch -Wildcard ($Target) {
        "x86_64-unknown-linux-musl"  { return Join-Path $RootDir "target" "easytier-linux-x86_64" }
        "aarch64-unknown-linux-musl" { return Join-Path $RootDir "target" "easytier-linux-aarch64" }
        "x86_64-pc-windows-gnu"      { return Join-Path $RootDir "target" "easytier-windows-x86_64" }
        "x86_64-pc-windows-msvc"     { return Join-Path $RootDir "target" "easytier-windows-x86_64" }
        default                      { return Join-Path $RootDir "target" "official-$Target" }
    }
}

function Get-BinarySuffix($Target) {
    if ($Target -match "windows") { return ".exe" }
    return ""
}

function Get-BuiltBinaryPath($Method, $Target, $Bin) {
    $dir    = Get-CargoOutputDir $Method $Target
    $suffix = Get-BinarySuffix $Target
    return Join-Path $dir "$Bin$suffix"
}

# ── Interactive menu utilities ──────────────────────────────────────────────────

function Menu-Choice($title, $count, $default = 0) {
    $hint = if ($default -ge 1 -and $default -le $count) { " [$default]" } else { "" }
    while ($true) {
        $in = Read-Host (c $C_BOLD "$title$hint")
        if ($in -eq '') {
            if ($default -ge 1 -and $default -le $count) { return $default }
        }
        if ($in -match '^\d+$') {
            $n = [int]$in
            if ($n -ge 1 -and $n -le $count) { return $n }
        }
        warn "请输入 1 到 $count 之间的编号"
    }
}

function Menu-YesNo($title, $default) {
    $hint = if ($default -eq "yes") { "[Y/n]" } else { "[y/N]" }
    while ($true) {
        $in = Read-Host (c $C_BOLD "$title $hint")
        if ([string]::IsNullOrEmpty($in)) { return $default }
        if ($in -match '^[yY]') { return "yes" }
        if ($in -match '^[nN]') { return "no" }
        warn "请输入 y 或 n"
    }
}

function Menu-Select($title, $options) {
    for ($i = 0; $i -lt $options.Count; $i++) {
        $num = $i + 1
        Write-Host ("  " + (c $C_CYAN "$num") + ") " + $options[$i])
    }
    $choice = Menu-Choice $title $options.Count 1
    return $options[$choice - 1]
}

# ── Interactive menu ────────────────────────────────────────────────────────────

function Show-InteractiveMenu($hostTarget) {
    Write-Host ($C_BOLD + (c $C_CYAN 'EasyTier Build Menu'))
    Write-Host ("  Host target: " + (c $C_GREEN $hostTarget))
    Write-Host ("  Toolchain:   " + (c $C_GREEN $Toolchain))
    Write-Host ""

    $Bin = Menu-Select "请选择项目" @("easytier-core", "easytier-cli")
    Write-Host ""

    $Method = Menu-Select "请选择编译方式" @("debug", "release", "release-small", "official")
    Write-Host ""

    # Target selection
    $targetOptions = @(
        "host ($hostTarget)"
        "x86_64-pc-windows-msvc"
        "x86_64-pc-windows-gnu"
        "x86_64-unknown-linux-musl"
        "aarch64-unknown-linux-musl"
        "自定义 target"
    )
    for ($i = 0; $i -lt $targetOptions.Count; $i++) {
        $num = $i + 1
        Write-Host ("  " + (c $C_CYAN "$num") + ") " + $targetOptions[$i])
    }
    $targetChoice = Menu-Choice "请选择目标平台" 6 1
    $Target = switch ($targetChoice) {
        1 { $hostTarget }
        2 { "x86_64-pc-windows-msvc" }
        3 { "x86_64-pc-windows-gnu" }
        4 { "x86_64-unknown-linux-musl" }
        5 { "aarch64-unknown-linux-musl" }
        6 {
            $custom = Read-Host "请输入自定义 target triple"
            if (-not $custom) { die "target 不能为空" }
            $custom
        }
    }
    Write-Host ""

    $cleanBld = if ((Menu-YesNo "构建前是否清理已有产物？" "no") -eq "yes") { 1 } else { 0 }
    $offlineBld = if ((Menu-YesNo "是否使用离线模式？" "yes") -eq "yes") { 1 } else { 0 }

    $featureDefault = ""
    $upxOpt = "auto"
    if ($Method -eq "official") {
        Write-Host ""
        $upxOpt = Menu-Select "官方模式是否启用 UPX 压缩" @("auto", "yes", "no")
        $featureDefault = Get-DefaultOfficialFeatures $Target
    }

    Write-Host ""
    $feat = Read-Host "请输入 features（留空使用默认）"
    if (-not $feat) { $feat = $featureDefault }

    # Summary & confirm
    Write-Host ""
    step "已选择配置"
    info "method=$Method"
    info "bin=$Bin"
    info "target=$Target"
    if ($feat) { info "features=$feat" } else { info "features=<default>" }
    if ($Method -eq "official") { info "upx=$upxOpt" }
    if ($cleanBld -eq 1) { info "clean=yes" } else { info "clean=no" }
    if ($offlineBld -eq 1) { info "offline=yes" } else { info "offline=no" }
    Write-Host ""

    $confirm = Menu-YesNo "确认以上配置并开始构建？" "yes"
    if ($confirm -ne "yes") {
        warn "已取消构建"
        exit 0
    }
    Write-Host ""

    return @{
        Method       = $Method
        Bin          = $Bin
        Target       = $Target
        Features     = $feat
        CleanBuild   = $cleanBld
        OfflineBuild = $offlineBld
        UseUpx       = $upxOpt
    }
}

# ── Build steps ─────────────────────────────────────────────────────────────────

function Invoke-CleanOutputs($method, $target, $bin) {
    step "清理旧产物"
    & cargo "+$Toolchain" clean -p easytier --target $target

    if ($method -eq "official") {
        $dir    = Get-OfficialArtifactDir $target
        $suffix = Get-BinarySuffix $target
        $path   = Join-Path $dir "$bin$suffix"
        if (Test-Path $path) { Remove-Item $path -Force }
    }
}

function Invoke-ConfigureBuildMode($method, $hostTarget) {
    if (-not $script:Target) {
        $script:Target = $hostTarget
    }

    switch ($method) {
        "debug"         { }
        "release"       { $script:ExtraCargo += "--release" }
        "release-small" {
            $script:ExtraCargo += @("--profile", "release-small")
            $script:BuildEnv["RUSTFLAGS"] = $env:RUSTFLAGS + " -C link-arg=-Wl,--gc-sections"
        }
        "official" {
            $script:ExtraCargo += "--release"
            if (-not $script:Features) {
                $script:Features = Get-DefaultOfficialFeatures $script:Target
            }
            if ($script:UseUpx -eq "auto") {
                $script:UseUpx = "yes"
            }
        }
    }
}

function Invoke-UpxCompress($binaryPath, $method) {
    if ($method -ne "official") { return }
    if ($script:UseUpx -eq "no") {
        info "UPX compression disabled"
        return
    }

    $upxBin = Get-Command upx -ErrorAction SilentlyContinue
    if (-not $upxBin) {
        if ($script:UseUpx -eq "yes") { warn "UPX not found, skipping compression" }
        else                          { info "UPX not found, skipping compression" }
        return
    }

    step "使用 UPX 压缩产物"
    & $upxBin.Source --lzma --best $binaryPath
    if ($LASTEXITCODE -ne 0) {
        warn "UPX compression failed, continuing..."
    }
}

function Invoke-CopyArtifact($binaryPath, $method, $target, $bin) {
    if ($method -ne "official") { return }

    $dir    = Get-OfficialArtifactDir $target
    $suffix = Get-BinarySuffix $target
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dest = Join-Path $dir "$bin$suffix"
    Copy-Item $binaryPath $dest -Force
    success "已复制到 $dest"
}

function Invoke-Summary($binaryPath, $method, $target, $bin) {
    $finalPath = $binaryPath
    if ($method -eq "official") {
        $finalPath = Join-Path (Get-OfficialArtifactDir $target) "$bin$(Get-BinarySuffix $target)"
    }

    Write-Host ""
    Write-Host ($C_BOLD + (c $C_GREEN 'Build Result'))
    $item      = Get-Item $finalPath
    $sizeBytes = $item.Length
    if ($sizeBytes -ge 1MB) {
        $sizeStr = "{0:N2} MB" -f ($sizeBytes / 1MB)
    } elseif ($sizeBytes -ge 1KB) {
        $sizeStr = "{0:N2} KB" -f ($sizeBytes / 1KB)
    } else {
        $sizeStr = "$sizeBytes bytes"
    }
    Write-Host "  Path: $finalPath"
    Write-Host "  Size: $sizeStr"
}

# ═══════════════════════════════════════════════════════════════════════════════
#  MAIN
# ═══════════════════════════════════════════════════════════════════════════════

if ($Help) {
    Show-Usage
    exit 0
}

# Determine interactive mode
$boundCount = ($MyInvocation.BoundParameters.Keys | Where-Object { $_ -ne 'Help' }).Count
$isMenu = ($boundCount -eq 0) -or $Menu

# Detect host target
$hostTarget = Get-HostTarget

# Check Cargo.toml
if (-not (Test-Path (Join-Path $RootDir "Cargo.toml"))) {
    die "Cargo.toml not found in $RootDir"
}

# Interactive menu
if ($isMenu) {
    $result = Show-InteractiveMenu $hostTarget
    $script:Method  = $result.Method
    $script:Bin     = $result.Bin
    $script:Target  = $result.Target
    $script:Features = $result.Features
    $script:CleanBuild = $result.CleanBuild
    $script:OfflineBld = $result.OfflineBuild
    $script:UseUpx  = $result.UseUpx
} else {
    if ($Upx)   { $script:UseUpx = "yes" }
    if ($NoUpx) { $script:UseUpx = "no"  }
}

# Detect protoc
$foundProtoc = Find-Protoc
if ($foundProtoc) {
    $script:ProtocBin = $foundProtoc
    info "detected protoc=$ProtocBin"
} elseif ($isMenu) {
    warn "未检测到 protoc，后续真正构建时会报错；可先设置 `$env:PROTOC"
}
Assert-Protoc

# Configure build
Invoke-ConfigureBuildMode $Method $hostTarget

# Clean if requested
if ($CleanBuild -eq 1) {
    Invoke-CleanOutputs $Method $Target $Bin
}

# Build cargo args
$cargoArgs = @(
    "+$Toolchain", "build",
    "-p", "easytier",
    "--bin", $Bin,
    "--target", $Target
)
if ($Features) {
    $cargoArgs += @("--features", $Features)
}
if ($OfflineBld -eq 1) {
    $cargoArgs += "--offline"
}
$cargoArgs += $ExtraCargo

step "开始构建"
info "method=$Method"
info "bin=$Bin"
info "target=$Target"
if ($Features) { info "features=$Features" } else { info "features=<default>" }
info "protoc=$ProtocBin"
if ($Method -eq "official") { info "upx=$UseUpx" }
info "running: cargo $($cargoArgs -join ' ')"

# Execute build
$origEnv = @{}
foreach ($k in $BuildEnv.Keys) {
    $origEnv[$k] = [Environment]::GetEnvironmentVariable($k)
    [Environment]::SetEnvironmentVariable($k, $BuildEnv[$k])
}
if ($ProtocBin) {
    $origEnv["PROTOC"] = [Environment]::GetEnvironmentVariable("PROTOC")
    [Environment]::SetEnvironmentVariable("PROTOC", $ProtocBin)
}

try {
    & cargo $cargoArgs
    if ($LASTEXITCODE -ne 0) {
        die "cargo build failed with exit code $LASTEXITCODE"
    }
} finally {
    foreach ($k in $origEnv.Keys) {
        [Environment]::SetEnvironmentVariable($k, $origEnv[$k])
    }
}

# Verify & post-process
$builtBinary = Get-BuiltBinaryPath $Method $Target $Bin
if (-not (Test-Path $builtBinary)) {
    die "build finished but binary not found: $builtBinary"
}

Invoke-UpxCompress $builtBinary $Method
Invoke-CopyArtifact $builtBinary $Method $Target $Bin
Invoke-Summary $builtBinary $Method $Target $Bin
