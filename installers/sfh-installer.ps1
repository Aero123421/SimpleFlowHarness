& {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"

    $Repository = "Aero123421/SimpleFlowHarness"
    $RequestedVersion = if ($env:SFH_VERSION) { $env:SFH_VERSION } else { "latest" }
    $InstallDir = if ($env:SFH_INSTALL_DIR) {
        $env:SFH_INSTALL_DIR
    } else {
        Join-Path $env:LOCALAPPDATA "Programs\sfh"
    }
    $AssetDir = $env:SFH_ASSET_DIR
    $BaseUrlOverride = $env:SFH_BASE_URL

    function Normalize-Version {
        param([Parameter(Mandatory)][string]$Version)

        if ($Version -eq "latest") {
            return "latest"
        }
        if ($Version -notmatch '^v?\d+\.\d+\.\d+(?:[-.][0-9A-Za-z.-]+)?$') {
            throw "Invalid SFH_VERSION '$Version' (expected latest, 1.2.3, or v1.2.3)"
        }
        if ($Version.StartsWith("v", [StringComparison]::Ordinal)) {
            return $Version
        }
        return "v$Version"
    }

    function Copy-ReleaseAsset {
        param(
            [Parameter(Mandatory)][string]$Name,
            [Parameter(Mandatory)][string]$Destination
        )

        if ($AssetDir) {
            if (-not (Test-Path -LiteralPath $AssetDir -PathType Container)) {
                throw "SFH_ASSET_DIR is not a directory: $AssetDir"
            }
            $source = Join-Path $AssetDir $Name
            if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
                throw "Asset not found in SFH_ASSET_DIR: $Name"
            }
            Copy-Item -LiteralPath $source -Destination $Destination
            return
        }

        Invoke-WebRequest -Uri "$BaseUrl/$Name" -OutFile $Destination
    }

    $version = Normalize-Version $RequestedVersion
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($architecture) {
        "X64" { }
        "Arm64" {
            Write-Host "Native Windows arm64 is not published yet; installing the Windows x64 build."
        }
        default {
            throw "Unsupported Windows CPU architecture '$architecture'"
        }
    }

    $asset = "sfh-windows-x64.zip"
    if ($BaseUrlOverride) {
        $BaseUrl = $BaseUrlOverride.TrimEnd("/")
    } elseif ($version -eq "latest") {
        $BaseUrl = "https://github.com/$Repository/releases/latest/download"
    } else {
        $BaseUrl = "https://github.com/$Repository/releases/download/$version"
    }

    $tempDir = Join-Path ([IO.Path]::GetTempPath()) "sfh-install-$([Guid]::NewGuid().ToString('N'))"
    $archive = Join-Path $tempDir $asset
    $checksum = "$archive.sha256"
    $extractDir = Join-Path $tempDir "extract"
    $staged = $null

    try {
        New-Item -ItemType Directory -Path $tempDir | Out-Null

        Write-Host "Downloading $asset..."
        Copy-ReleaseAsset $asset $archive
        Copy-ReleaseAsset "$asset.sha256" $checksum

        $expected = ((Get-Content -LiteralPath $checksum -Raw) -split '\s+')[0].ToLowerInvariant()
        if ($expected -notmatch '^[0-9a-f]{64}$') {
            throw "Invalid SHA-256 file for $asset"
        }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "SHA-256 mismatch for $asset (expected $expected, got $actual)"
        }

        New-Item -ItemType Directory -Path $extractDir | Out-Null
        Expand-Archive -LiteralPath $archive -DestinationPath $extractDir
        $executable = Join-Path $extractDir "sfh.exe"
        if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw "Archive does not contain sfh.exe"
        }

        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        $staged = Join-Path $InstallDir ".sfh.new-$PID-$([Guid]::NewGuid().ToString('N')).exe"
        Copy-Item -LiteralPath $executable -Destination $staged
        Unblock-File -LiteralPath $staged -ErrorAction SilentlyContinue
        Move-Item -LiteralPath $staged -Destination (Join-Path $InstallDir "sfh.exe") -Force

        $env:Path = "$InstallDir;$env:Path"
        if ($env:SFH_NO_MODIFY_PATH -notin @("1", "true", "TRUE", "yes", "YES")) {
            $userPath = [string][Environment]::GetEnvironmentVariable("Path", "User")
            $normalizedInstallDir = $InstallDir.TrimEnd("\")
            $alreadyPresent = ($userPath -split ";") |
                Where-Object {
                    $_ -and $_.TrimEnd("\").Equals(
                        $normalizedInstallDir,
                        [StringComparison]::OrdinalIgnoreCase
                    )
                }
            if (-not $alreadyPresent) {
                $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
                    $InstallDir
                } else {
                    "$userPath;$InstallDir"
                }
                [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
                Write-Host "Added $InstallDir to the user PATH."
            }
        }

        $installedVersion = & (Join-Path $InstallDir "sfh.exe") --version
        if ($LASTEXITCODE -ne 0) {
            throw "Installed binary did not start"
        }
        Write-Host "Installed $installedVersion to $(Join-Path $InstallDir 'sfh.exe')"
    } finally {
        if ($staged -and (Test-Path -LiteralPath $staged)) {
            Remove-Item -LiteralPath $staged -Force
        }
        if (Test-Path -LiteralPath $tempDir) {
            Remove-Item -LiteralPath $tempDir -Recurse -Force
        }
    }
}
