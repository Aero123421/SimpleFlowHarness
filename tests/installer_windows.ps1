param(
    [Parameter(Mandatory)]
    [string]$Binary
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$ResolvedBinary = (Resolve-Path -LiteralPath $Binary).Path
$TestRoot = Join-Path ([IO.Path]::GetTempPath()) "sfh-installer-test-$([Guid]::NewGuid().ToString('N'))"
$OldEnvironment = @{
    SFH_ASSET_DIR = $env:SFH_ASSET_DIR
    SFH_INSTALL_DIR = $env:SFH_INSTALL_DIR
    SFH_NO_MODIFY_PATH = $env:SFH_NO_MODIFY_PATH
    Path = $env:Path
}

try {
    $PackageDir = Join-Path $TestRoot "package"
    $AssetDir = Join-Path $TestRoot "assets"
    $InstallDir = Join-Path $TestRoot "installed"
    New-Item -ItemType Directory -Path $PackageDir, $AssetDir | Out-Null

    Copy-Item -LiteralPath $ResolvedBinary -Destination $PackageDir
    Copy-Item -LiteralPath @(
        (Join-Path $RepoRoot "README.md"),
        (Join-Path $RepoRoot "README.ja.md"),
        (Join-Path $RepoRoot "LICENSE")
    ) -Destination $PackageDir

    $AssetName = "sfh-windows-x64.zip"
    $Asset = Join-Path $AssetDir $AssetName
    Compress-Archive -Path (Join-Path $PackageDir "*") -DestinationPath $Asset
    $Hash = (Get-FileHash -LiteralPath $Asset -Algorithm SHA256).Hash.ToLowerInvariant()
    "$Hash  $AssetName`n" |
        Out-File -LiteralPath "$Asset.sha256" -Encoding ascii -NoNewline

    $ExpectedVersion = & $ResolvedBinary --version
    if ($LASTEXITCODE -ne 0) {
        throw "Fixture binary did not start"
    }

    $env:SFH_ASSET_DIR = $AssetDir
    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_NO_MODIFY_PATH = "1"
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")

    $ActualVersion = & (Join-Path $InstallDir "sfh.exe") --version
    if ($LASTEXITCODE -ne 0 -or $ActualVersion -ne $ExpectedVersion) {
        throw "Installed version mismatch: $ActualVersion != $ExpectedVersion"
    }
    $PathVersion = & sfh --version
    if ($LASTEXITCODE -ne 0 -or $PathVersion -ne $ExpectedVersion) {
        throw "PATH invocation mismatch: $PathVersion != $ExpectedVersion"
    }

    $BadAssetDir = Join-Path $TestRoot "bad-assets"
    Copy-Item -LiteralPath $AssetDir -Destination $BadAssetDir -Recurse
    [IO.File]::AppendAllText(
        (Join-Path $BadAssetDir $AssetName),
        "corrupt"
    )

    $env:SFH_ASSET_DIR = $BadAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "rejected"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "SHA-256 mismatch") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted a corrupted archive"
    }

    Write-Host "Windows installer checks passed ($ExpectedVersion, $AssetName)"
} finally {
    foreach ($Name in $OldEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable(
            $Name,
            $OldEnvironment[$Name],
            [EnvironmentVariableTarget]::Process
        )
    }
    if (Test-Path -LiteralPath $TestRoot) {
        Remove-Item -LiteralPath $TestRoot -Recurse -Force
    }
}
