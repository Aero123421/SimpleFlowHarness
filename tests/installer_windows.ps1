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
    LOCALAPPDATA = $env:LOCALAPPDATA
    SFH_ASSET_DIR = $env:SFH_ASSET_DIR
    SFH_DATA_DIR = $env:SFH_DATA_DIR
    SFH_INSTALL_DIR = $env:SFH_INSTALL_DIR
    SFH_NO_MODIFY_PATH = $env:SFH_NO_MODIFY_PATH
    SFH_STATE_DIR = $env:SFH_STATE_DIR
    Path = $env:Path
}

try {
    $PackageDir = Join-Path $TestRoot "package"
    $AssetDir = Join-Path $TestRoot "assets"
    $InstallDir = Join-Path $TestRoot "installed"
    $DataDir = Join-Path $TestRoot "data"
    New-Item -ItemType Directory -Path $PackageDir, $AssetDir | Out-Null
    $RequiredResources = [IO.File]::ReadAllLines(
        (Join-Path $RepoRoot "release-resources.txt"),
        [Text.Encoding]::ASCII
    )

    function Write-AssetChecksum {
        param(
            [Parameter(Mandatory)][string]$Path,
            [Parameter(Mandatory)][string]$Name
        )

        $Hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
        [IO.File]::WriteAllText(
            "$Path.sha256",
            "$Hash  $Name`n",
            [Text.Encoding]::ASCII
        )
    }

    function Assert-ResourcesEqual {
        param(
            [Parameter(Mandatory)][string]$Expected,
            [Parameter(Mandatory)][string]$Actual
        )

        foreach ($Resource in @($RequiredResources) + "release-resources.txt") {
            $relative = $Resource.TrimEnd("/")
            $expectedPath = Join-Path $Expected $relative
            $actualPath = Join-Path $Actual $relative
            if ($Resource.EndsWith("/")) {
                $expectedFiles = Get-ChildItem -LiteralPath $expectedPath -File -Recurse |
                    ForEach-Object { $_.FullName.Substring($expectedPath.Length + 1) } |
                    Sort-Object
                $actualFiles = Get-ChildItem -LiteralPath $actualPath -File -Recurse |
                    ForEach-Object { $_.FullName.Substring($actualPath.Length + 1) } |
                    Sort-Object
                if (Compare-Object $expectedFiles $actualFiles) {
                    throw "Installed resource tree differs: $Resource"
                }
                foreach ($file in $expectedFiles) {
                    $expectedHash = (Get-FileHash -LiteralPath (Join-Path $expectedPath $file)).Hash
                    $actualHash = (Get-FileHash -LiteralPath (Join-Path $actualPath $file)).Hash
                    if ($actualHash -ne $expectedHash) {
                        throw "Installed resource differs: $Resource$file"
                    }
                }
            } else {
                $expectedHash = (Get-FileHash -LiteralPath $expectedPath).Hash
                $actualHash = (Get-FileHash -LiteralPath $actualPath).Hash
                if ($actualHash -ne $expectedHash) {
                    throw "Installed resource differs: $Resource"
                }
            }
        }
    }

    $AssetName = "sfh-windows-x64.zip"
    $Asset = Join-Path $AssetDir $AssetName
    $Python = Get-Command python -ErrorAction Stop
    & $Python.Source (Join-Path $RepoRoot "scripts/release_assets.py") package `
        --binary $ResolvedBinary `
        --asset $Asset
    if ($LASTEXITCODE -ne 0) {
        throw "Release helper could not build the installer fixture"
    }
    Expand-Archive -LiteralPath $Asset -DestinationPath $PackageDir

    $ExpectedVersion = & $ResolvedBinary --version
    if ($LASTEXITCODE -ne 0) {
        throw "Fixture binary did not start"
    }
    $InstallerSource = Get-Content -LiteralPath (
        Join-Path $RepoRoot "installers/sfh-installer.ps1"
    ) -Raw
    foreach ($requiredSource in @(
        '{{WINDOWS_CODESIGN_CERT_SHA256}}',
        '{{WINDOWS_X64_SHA256}}',
        '{{VERSION}}',
        'if ($ExpectedSignerCertificateSha256 -cne "UNSIGNED")',
        'Get-AuthenticodeSignature -LiteralPath $staged',
        '$signature.TimeStamperCertificate',
        '$signature.SignerCertificate.RawData',
        '$manifestProcessInfo.Arguments = "__release-manifest"',
        '$manifestProcess.StandardOutput.BaseStream.CopyTo($embeddedInventoryStream)',
        'Official build release manifest does not match the downloaded resources',
        'Release sidecar SHA-256 does not match this installer',
        'Archive exceeds the 2000-member limit',
        'Assert-NoAlternateDataStreams $path $relative',
        '[IO.FileOptions]::DeleteOnClose',
        '$lockPath = Get-InstallerLockPath',
        'Stale sfh installer lock requires inspection and manual removal',
        'Remove-InventoriedResourceTree $dataPrevious'
    )) {
        if (-not $InstallerSource.Contains($requiredSource)) {
            throw "Windows installer is missing official signature verification: $requiredSource"
        }
    }
    if (([regex]::Matches($InstallerSource, 'Assert-ResourceInventory \$dataPrevious')).Count `
        -lt 2) {
        throw "Windows installer does not revalidate previous resources at both commit boundaries"
    }
    if (([regex]::Matches(
            $InstallerSource,
            'Assert-ActivatedResourceTree \$DataDir \$expectedStagedInventory'
        )).Count -ne 2) {
        throw "Windows installer does not validate activated resources at both commit boundaries"
    }

    $env:SFH_ASSET_DIR = $AssetDir
    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir
    $env:SFH_NO_MODIFY_PATH = "1"
    [Environment]::SetEnvironmentVariable(
        "SFH_STATE_DIR",
        $null,
        [EnvironmentVariableTarget]::Process
    )
    $GlobalInstallerLock = Join-Path (
        [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    ) "sfh-installer.lock"
    if (Test-Path -LiteralPath $GlobalInstallerLock) {
        throw "Cannot run stale-lock test while another installer lock exists: $GlobalInstallerLock"
    }
    [IO.File]::WriteAllText($GlobalInstallerLock, "2147483647`n", [Text.Encoding]::ASCII)
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "stale-lock-install"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "stale-lock-data"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch
            [regex]::Escape("Stale sfh installer lock requires inspection and manual removal: $GlobalInstallerLock")) {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or -not (Test-Path -LiteralPath $GlobalInstallerLock)) {
        throw "Installer automatically removed a stale global lock"
    }
    if ((Test-Path -LiteralPath (Join-Path $TestRoot "stale-lock-install\sfh.exe")) -or
        (Test-Path -LiteralPath (Join-Path $TestRoot "stale-lock-data"))) {
        throw "Stale-lock refusal mutated an install destination"
    }
    Remove-Item -LiteralPath $GlobalInstallerLock -Force
    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    Assert-ResourcesEqual $PackageDir $DataDir
    if (
        (Get-Content -LiteralPath (Join-Path $DataDir ".sfh-installer-owned") -Raw) -cne
        "sfh installer resource directory v1`n"
    ) {
        throw "Installer did not write its ownership marker"
    }
    $InventoryPath = Join-Path $DataDir ".sfh-installer-inventory"
    if (-not (Test-Path -LiteralPath $InventoryPath -PathType Leaf)) {
        throw "Installer did not write its private resource inventory"
    }
    $InventoryRecords = [string[]](Get-Content -LiteralPath $InventoryPath)
    $InventoryPaths = foreach ($record in $InventoryRecords) {
        if ($record -match '^d - (.+)/$') {
            $Matches[1]
        } elseif ($record -match '^f [0-9a-f]{64} (.+)$') {
            $Matches[1]
        } else {
            throw "Installer inventory contains an invalid record: $record"
        }
    }
    $SortedInventoryPaths = [string[]]$InventoryPaths.Clone()
    [Array]::Sort($SortedInventoryPaths, [StringComparer]::Ordinal)
    if (Compare-Object $InventoryPaths $SortedInventoryPaths -SyncWindow 0) {
        throw "Installer inventory is not canonically sorted"
    }
    if ($InventoryPaths -contains ".sfh-installer-owned" -or
        $InventoryPaths -contains ".sfh-installer-inventory") {
        throw "Installer inventory includes its private metadata"
    }

    $DefaultLocalAppData = Join-Path $TestRoot "default-local-app-data"
    $DefaultStateRoot = Join-Path $DefaultLocalAppData "sfh"
    $DefaultDataDir = Join-Path $DefaultLocalAppData "sfh-resources"
    $StateSentinel = Join-Path $DefaultStateRoot "state-sentinel.txt"
    $WorkspaceSentinel = Join-Path $DefaultStateRoot "workspaces\owned-worktree\state.txt"
    New-Item -ItemType Directory -Path (Split-Path -Parent $WorkspaceSentinel) -Force |
        Out-Null
    [IO.File]::WriteAllText($StateSentinel, "managed state`n", [Text.Encoding]::ASCII)
    [IO.File]::WriteAllText($WorkspaceSentinel, "workspace state`n", [Text.Encoding]::ASCII)

    $env:LOCALAPPDATA = $DefaultLocalAppData
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "default-data-install"

    $env:SFH_DATA_DIR = $DefaultStateRoot
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "state directory must not overlap") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted the default runtime state root as its resource destination"
    }
    if ((Get-Content -LiteralPath $StateSentinel -Raw) -cne "managed state`n") {
        throw "State/resource overlap rejection modified the default state root"
    }

    [Environment]::SetEnvironmentVariable(
        "SFH_DATA_DIR",
        $null,
        [EnvironmentVariableTarget]::Process
    )
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    Assert-ResourcesEqual $PackageDir $DefaultDataDir
    if ((Get-Content -LiteralPath $StateSentinel -Raw) -cne "managed state`n") {
        throw "Default resource install modified the managed state root"
    }
    if ((Get-Content -LiteralPath $WorkspaceSentinel -Raw) -cne "workspace state`n") {
        throw "Default resource install modified a managed workspace"
    }

    $DefaultUnknownDir = Join-Path $DefaultDataDir "examples\.runtime-state\runs"
    $DefaultUnknownSentinel = Join-Path $DefaultUnknownDir "keep.txt"
    New-Item -ItemType Directory -Path $DefaultUnknownDir -Force | Out-Null
    [IO.File]::WriteAllText(
        $DefaultUnknownSentinel,
        "keep unknown runtime state`n",
        [Text.Encoding]::ASCII
    )
    $DefaultBinary = Join-Path $env:SFH_INSTALL_DIR "sfh.exe"
    $DefaultBinaryHash = (Get-FileHash -LiteralPath $DefaultBinary -Algorithm SHA256).Hash
    $DefaultInventoryHash = (
        Get-FileHash -LiteralPath (Join-Path $DefaultDataDir ".sfh-installer-inventory") `
            -Algorithm SHA256
    ).Hash
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "does not match its installer inventory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Default resource reinstall replaced an unknown nested entry"
    }
    if ((Get-Content -LiteralPath $DefaultUnknownSentinel -Raw) -cne "keep unknown runtime state`n") {
        throw "Default resource reinstall modified unknown nested state"
    }
    if ((Get-FileHash -LiteralPath $DefaultBinary -Algorithm SHA256).Hash -cne $DefaultBinaryHash) {
        throw "Default resource inventory refusal modified the installed binary"
    }
    if ((Get-FileHash -LiteralPath (Join-Path $DefaultDataDir ".sfh-installer-inventory") `
            -Algorithm SHA256).Hash -cne $DefaultInventoryHash) {
        throw "Default resource inventory refusal modified the resource inventory"
    }
    if ((Get-Content -LiteralPath $StateSentinel -Raw) -cne "managed state`n") {
        throw "Default resource reinstall modified the managed state root"
    }
    if ((Get-Content -LiteralPath $WorkspaceSentinel -Raw) -cne "workspace state`n") {
        throw "Default resource reinstall modified a managed workspace"
    }
    Remove-Item -LiteralPath (Join-Path $DefaultDataDir "examples\.runtime-state") `
        -Recurse -Force
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    Assert-ResourcesEqual $PackageDir $DefaultDataDir
    $StateEntries = Get-ChildItem -LiteralPath $DefaultStateRoot -Force |
        Select-Object -ExpandProperty Name |
        Sort-Object
    if (
        Compare-Object `
            -ReferenceObject @("state-sentinel.txt", "workspaces") `
            -DifferenceObject $StateEntries
    ) {
        throw "Default resource install added unexpected entries to the managed state root"
    }

    $env:LOCALAPPDATA = $OldEnvironment.LOCALAPPDATA
    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir

    $CollidingStateDir = Join-Path $DataDir "runtime-state"
    $CollidingStateSentinel = Join-Path $CollidingStateDir "runs\keep.txt"
    New-Item -ItemType Directory -Path (Split-Path -Parent $CollidingStateSentinel) -Force |
        Out-Null
    [IO.File]::WriteAllText(
        $CollidingStateSentinel,
        "keep runtime state`n",
        [Text.Encoding]::ASCII
    )
    $env:SFH_STATE_DIR = $CollidingStateDir
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "state directory must not overlap") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted an explicit state directory inside its resource destination"
    }
    if ((Get-Content -LiteralPath $CollidingStateSentinel -Raw) -cne "keep runtime state`n") {
        throw "State/resource overlap rejection modified explicit runtime state"
    }
    Remove-Item -LiteralPath $CollidingStateDir -Recurse -Force
    [Environment]::SetEnvironmentVariable(
        "SFH_STATE_DIR",
        $null,
        [EnvironmentVariableTarget]::Process
    )

    $OverlapRoot = Join-Path $TestRoot "overlap"
    $env:SFH_INSTALL_DIR = Join-Path $OverlapRoot "bin"
    $env:SFH_DATA_DIR = Join-Path $OverlapRoot "missing\..\bin\sfh.exe\resources"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "must not overlap") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted overlapping binary and resource destinations"
    }
    if (Test-Path -LiteralPath $OverlapRoot) {
        throw "Installer mutated an overlapping destination before rejecting it"
    }

    $env:SFH_INSTALL_DIR = Join-Path $OverlapRoot "parent\child\bin"
    $env:SFH_DATA_DIR = Join-Path $OverlapRoot "parent\missing\.."
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "must not overlap") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted a resource destination containing the install directory"
    }
    if (Test-Path -LiteralPath $OverlapRoot) {
        throw "Installer mutated a reverse-overlapping destination before rejecting it"
    }

    $OverlapBin = Join-Path $OverlapRoot "bin"
    New-Item -ItemType Directory -Path $OverlapBin | Out-Null
    $OverlapLink = Join-Path $TestRoot "overlap-link"
    New-Item -ItemType Junction -Path $OverlapLink -Target $OverlapBin | Out-Null
    $env:SFH_INSTALL_DIR = $OverlapBin
    $env:SFH_DATA_DIR = Join-Path $OverlapLink "resources"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "must not overlap") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted overlapping destinations through a junction"
    }
    if (Test-Path -LiteralPath (Join-Path $OverlapBin "resources")) {
        throw "Installer mutated a junction-overlapping destination before rejecting it"
    }

    $MalformedInstallDir = Join-Path $TestRoot "malformed-install"
    $MalformedDataDir = Join-Path $TestRoot "malformed-data"
    New-Item -ItemType Directory -Path (Join-Path $MalformedInstallDir "sfh.exe") -Force | Out-Null
    Copy-Item -LiteralPath $DataDir -Destination $MalformedDataDir -Recurse
    Set-Content -LiteralPath (Join-Path $MalformedDataDir "previous.txt") -Value "previous"
    $env:SFH_INSTALL_DIR = $MalformedInstallDir
    $env:SFH_DATA_DIR = $MalformedDataDir
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "Binary destination is a directory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted a directory at the binary destination"
    }
    if ((Get-Content -LiteralPath (Join-Path $MalformedDataDir "previous.txt") -Raw) -notmatch "previous") {
        throw "Installer modified resources after rejecting the binary destination"
    }

    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir
    $UnknownResourceDir = Join-Path $DataDir "runtime-state\runs"
    $UnknownResourceSentinel = Join-Path $UnknownResourceDir "keep.txt"
    New-Item -ItemType Directory -Path $UnknownResourceDir -Force | Out-Null
    [IO.File]::WriteAllText(
        $UnknownResourceSentinel,
        "keep unknown runtime state`n",
        [Text.Encoding]::ASCII
    )
    $InstalledBinary = Join-Path $InstallDir "sfh.exe"
    $BinaryBeforeUnknown = (Get-FileHash -LiteralPath $InstalledBinary -Algorithm SHA256).Hash
    $ReadmeBeforeUnknown = (Get-FileHash -LiteralPath (Join-Path $DataDir "README.md") `
        -Algorithm SHA256).Hash
    $InventoryBeforeUnknown = (Get-FileHash -LiteralPath (Join-Path $DataDir `
        ".sfh-installer-inventory") -Algorithm SHA256).Hash
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "does not match its installer inventory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer replaced a resource tree containing an unknown top-level entry"
    }
    if ((Get-Content -LiteralPath $UnknownResourceSentinel -Raw) -cne
        "keep unknown runtime state`n") {
        throw "Installer modified an unknown top-level entry"
    }
    if ((Get-FileHash -LiteralPath $InstalledBinary -Algorithm SHA256).Hash -cne
        $BinaryBeforeUnknown) {
        throw "Resource inventory refusal modified the installed binary"
    }
    if ((Get-FileHash -LiteralPath (Join-Path $DataDir "README.md") -Algorithm SHA256).Hash `
        -cne $ReadmeBeforeUnknown) {
        throw "Resource inventory refusal modified an installed resource"
    }
    if ((Get-FileHash -LiteralPath (Join-Path $DataDir ".sfh-installer-inventory") `
            -Algorithm SHA256).Hash -cne $InventoryBeforeUnknown) {
        throw "Resource inventory refusal modified the resource inventory"
    }
    Remove-Item -LiteralPath (Join-Path $DataDir "runtime-state") -Recurse -Force

    $MissingInventoryData = Join-Path $TestRoot "missing-inventory-data"
    $MissingInventoryInstall = Join-Path $TestRoot "missing-inventory-install"
    Copy-Item -LiteralPath $DataDir -Destination $MissingInventoryData -Recurse
    New-Item -ItemType Directory -Path $MissingInventoryInstall | Out-Null
    Copy-Item -LiteralPath $InstalledBinary -Destination $MissingInventoryInstall
    Remove-Item -LiteralPath (Join-Path $MissingInventoryData "README.md")
    $MissingBinary = Join-Path $MissingInventoryInstall "sfh.exe"
    $MissingBinaryHash = (Get-FileHash -LiteralPath $MissingBinary -Algorithm SHA256).Hash
    $env:SFH_INSTALL_DIR = $MissingInventoryInstall
    $env:SFH_DATA_DIR = $MissingInventoryData
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "does not match its installer inventory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or (Test-Path -LiteralPath (Join-Path $MissingInventoryData `
            "README.md"))) {
        throw "Installer did not preserve a resource tree with an inventoried file missing"
    }
    if ((Get-FileHash -LiteralPath $MissingBinary -Algorithm SHA256).Hash -cne
        $MissingBinaryHash) {
        throw "Missing-resource refusal modified the installed binary"
    }

    $TypeChangeData = Join-Path $TestRoot "type-change-data"
    $TypeChangeInstall = Join-Path $TestRoot "type-change-install"
    Copy-Item -LiteralPath $DataDir -Destination $TypeChangeData -Recurse
    New-Item -ItemType Directory -Path $TypeChangeInstall | Out-Null
    Copy-Item -LiteralPath $InstalledBinary -Destination $TypeChangeInstall
    Remove-Item -LiteralPath (Join-Path $TypeChangeData "SUPPORT.md")
    New-Item -ItemType Directory -Path (Join-Path $TypeChangeData "SUPPORT.md") | Out-Null
    Set-Content -LiteralPath (Join-Path $TypeChangeData "SUPPORT.md\keep.txt") `
        -Value "keep type change"
    $env:SFH_INSTALL_DIR = $TypeChangeInstall
    $env:SFH_DATA_DIR = $TypeChangeData
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "does not match its installer inventory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or
        -not (Test-Path -LiteralPath (Join-Path $TypeChangeData "SUPPORT.md\keep.txt"))) {
        throw "Installer did not preserve a resource tree with a file/directory type change"
    }

    $ContentChangeData = Join-Path $TestRoot "content-change-data"
    $ContentChangeInstall = Join-Path $TestRoot "content-change-install"
    Copy-Item -LiteralPath $DataDir -Destination $ContentChangeData -Recurse
    New-Item -ItemType Directory -Path $ContentChangeInstall | Out-Null
    Copy-Item -LiteralPath $InstalledBinary -Destination $ContentChangeInstall
    [IO.File]::WriteAllText(
        (Join-Path $ContentChangeData "README.md"),
        "locally edited resource`n",
        [Text.Encoding]::ASCII
    )
    $env:SFH_INSTALL_DIR = $ContentChangeInstall
    $env:SFH_DATA_DIR = $ContentChangeData
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "does not match its installer inventory") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or
        (Get-Content -LiteralPath (Join-Path $ContentChangeData "README.md") -Raw) -cne
        "locally edited resource`n") {
        throw "Installer overwrote a locally modified resource"
    }

    $ReparseData = Join-Path $TestRoot "reparse-data"
    $ReparseInstall = Join-Path $TestRoot "reparse-install"
    $ReparseTarget = Join-Path $TestRoot "reparse-target"
    Copy-Item -LiteralPath $DataDir -Destination $ReparseData -Recurse
    New-Item -ItemType Directory -Path $ReparseInstall, $ReparseTarget | Out-Null
    Copy-Item -LiteralPath $InstalledBinary -Destination $ReparseInstall
    Set-Content -LiteralPath (Join-Path $ReparseTarget "keep.txt") -Value "keep reparse target"
    New-Item -ItemType Junction -Path (Join-Path $ReparseData "examples\runtime-link") `
        -Target $ReparseTarget | Out-Null
    $env:SFH_INSTALL_DIR = $ReparseInstall
    $env:SFH_DATA_DIR = $ReparseData
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "reparse point") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or
        (Get-Content -LiteralPath (Join-Path $ReparseTarget "keep.txt") -Raw) -notmatch
        "keep reparse target") {
        throw "Installer mutated a resource reparse point or its target"
    }

    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir
    & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    Assert-ResourcesEqual $PackageDir $DataDir

    $ConcurrentRoot = Join-Path $TestRoot "concurrent"
    $ConcurrentInstall = Join-Path $ConcurrentRoot "install"
    $ConcurrentData = Join-Path $ConcurrentRoot "data"
    New-Item -ItemType Directory -Path $ConcurrentRoot | Out-Null
    $env:SFH_ASSET_DIR = $AssetDir
    $env:SFH_INSTALL_DIR = $ConcurrentInstall
    $env:SFH_DATA_DIR = $ConcurrentData
    $PowerShellHost = (Get-Process -Id $PID).Path
    $InstallerPath = Join-Path $RepoRoot "installers/sfh-installer.ps1"
    $ConcurrentOutput1 = Join-Path $TestRoot "concurrent-first.out"
    $ConcurrentError1 = Join-Path $TestRoot "concurrent-first.err"
    $ConcurrentOutput2 = Join-Path $TestRoot "concurrent-second.out"
    $ConcurrentError2 = Join-Path $TestRoot "concurrent-second.err"
    $ConcurrentRunner = Join-Path $TestRoot "concurrent-installer-runner.ps1"
    $ConcurrentRunnerSource = @'
param(
    [Parameter(Mandatory)][string]$Installer,
    [Parameter(Mandatory)][string]$Output,
    [Parameter(Mandatory)][string]$ErrorOutput,
    [Parameter(Mandatory)][string]$InstallDir,
    [Parameter(Mandatory)][string]$DataDir
)
$ErrorActionPreference = "Stop"
$env:SFH_INSTALL_DIR = $InstallDir
$env:SFH_DATA_DIR = $DataDir
[IO.File]::WriteAllText($ErrorOutput, "", [Text.Encoding]::UTF8)
try {
    & $Installer *> $Output
    exit 0
} catch {
    [IO.File]::WriteAllText(
        $ErrorOutput,
        ($_ | Out-String),
        [Text.Encoding]::UTF8
    )
    exit 1
}
'@
    [IO.File]::WriteAllText(
        $ConcurrentRunner,
        $ConcurrentRunnerSource,
        [Text.Encoding]::ASCII
    )
    $ConcurrentArguments1 = @(
        "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
        "`"$ConcurrentRunner`"", "`"$InstallerPath`"", "`"$ConcurrentOutput1`"",
        "`"$ConcurrentError1`"", "`"$ConcurrentInstall`"", "`"$ConcurrentData`""
    )
    $ConcurrentArguments2 = @(
        "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
        "`"$ConcurrentRunner`"", "`"$InstallerPath`"", "`"$ConcurrentOutput2`"",
        "`"$ConcurrentError2`"", "`"$(Join-Path $ConcurrentRoot 'other-install')`"",
        "`"$ConcurrentInstall`""
    )
    $GlobalLockPath = Join-Path (
        [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    ) "sfh-installer.lock"
    $ConcurrentProcess1 = Start-Process `
        -FilePath $PowerShellHost `
        -ArgumentList $ConcurrentArguments1 `
        -WindowStyle Hidden `
        -PassThru
    $LockWaitCount = 0
    while (-not (Test-Path -LiteralPath $GlobalLockPath)) {
        if ($ConcurrentProcess1.HasExited) {
            throw "First concurrent installer exited before acquiring the global lock"
        }
        $LockWaitCount++
        if ($LockWaitCount -gt 500) {
            $ConcurrentProcess1.Kill()
            throw "Timed out waiting for the first concurrent installer to acquire the global lock"
        }
        Start-Sleep -Milliseconds 20
        $ConcurrentProcess1.Refresh()
    }
    $ConcurrentProcess2 = Start-Process `
        -FilePath $PowerShellHost `
        -ArgumentList $ConcurrentArguments2 `
        -WindowStyle Hidden `
        -PassThru
    $ConcurrentProcess1.WaitForExit()
    $ConcurrentProcess2.WaitForExit()
    $ConcurrentProcess1.Refresh()
    $ConcurrentProcess2.Refresh()
    $ConcurrentExitCodes = @(
        @($ConcurrentProcess1.ExitCode, $ConcurrentProcess2.ExitCode) | Sort-Object
    )
    if ($ConcurrentExitCodes.Count -ne 2 -or
        $ConcurrentProcess1.ExitCode -ne 0 -or
        $ConcurrentProcess2.ExitCode -eq 0) {
        throw "Concurrent installers did not produce exactly one success and one refusal: $ConcurrentExitCodes"
    }
    $ConcurrentErrors = (Get-Content -LiteralPath $ConcurrentError1 -Raw) +
        (Get-Content -LiteralPath $ConcurrentError2 -Raw)
    if ($ConcurrentErrors -notmatch "Another sfh installer is active") {
        throw "Concurrent installer refusal did not identify the global lock"
    }
    Assert-ResourcesEqual $PackageDir $ConcurrentData
    $ConcurrentVersion = & (Join-Path $ConcurrentInstall "sfh.exe") --version
    if ($LASTEXITCODE -ne 0 -or $ConcurrentVersion -ne $ExpectedVersion) {
        throw "Concurrent install left an invalid binary"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $ConcurrentData ".sfh-installer-inventory") `
            -PathType Leaf)) {
        throw "Concurrent install did not leave a resource inventory"
    }
    $ConcurrentArtifacts = Get-ChildItem -LiteralPath $ConcurrentRoot -Force -Recurse |
        Where-Object {
            $_.Name -like ".sfh-installer-lock-*" -or
            $_.Name -like ".sfh-data-*" -or
            $_.Name -like ".sfh.new-*"
        }
    if ($ConcurrentArtifacts) {
        throw "Concurrent install left a lock or transaction artifact"
    }
    if (Test-Path -LiteralPath $GlobalLockPath) {
        throw "Concurrent install left the global installer lock behind"
    }

    $env:SFH_INSTALL_DIR = $InstallDir
    $env:SFH_DATA_DIR = $DataDir

    $AdsResource = Join-Path $DataDir "README.md"
    $AdsBinaryHash = (Get-FileHash -LiteralPath (Join-Path $InstallDir "sfh.exe") `
        -Algorithm SHA256).Hash
    $AdsResourceHash = (Get-FileHash -LiteralPath $AdsResource -Algorithm SHA256).Hash
    $AdsInventoryHash = (Get-FileHash -LiteralPath (Join-Path $DataDir `
        ".sfh-installer-inventory") -Algorithm SHA256).Hash
    Set-Content -LiteralPath $AdsResource -Stream "sfh-installer-test" -Value "preserve me"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "alternate data stream") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer replaced a resource containing an alternate data stream"
    }
    if ((Get-Content -LiteralPath $AdsResource -Stream "sfh-installer-test" -Raw) `
        -notmatch "preserve me") {
        throw "Installer modified a resource alternate data stream"
    }
    if ((Get-FileHash -LiteralPath (Join-Path $InstallDir "sfh.exe") `
            -Algorithm SHA256).Hash -cne $AdsBinaryHash -or
        (Get-FileHash -LiteralPath $AdsResource -Algorithm SHA256).Hash -cne $AdsResourceHash -or
        (Get-FileHash -LiteralPath (Join-Path $DataDir ".sfh-installer-inventory") `
            -Algorithm SHA256).Hash -cne $AdsInventoryHash) {
        throw "Alternate-stream refusal modified binary or resource data"
    }
    Remove-Item -LiteralPath $AdsResource -Stream "sfh-installer-test"

    $ActualVersion = & (Join-Path $InstallDir "sfh.exe") --version
    if ($LASTEXITCODE -ne 0 -or $ActualVersion -ne $ExpectedVersion) {
        throw "Installed version mismatch: $ActualVersion != $ExpectedVersion"
    }
    $PathVersion = & sfh --version
    if ($LASTEXITCODE -ne 0 -or $PathVersion -ne $ExpectedVersion) {
        throw "PATH invocation mismatch: $PathVersion != $ExpectedVersion"
    }

    $RollbackMarker = Join-Path $DataDir "README.md"
    $RollbackMarkerHash = (Get-FileHash -LiteralPath $RollbackMarker -Algorithm SHA256).Hash
    $RollbackInventoryHash = (Get-FileHash -LiteralPath (Join-Path $DataDir `
        ".sfh-installer-inventory") -Algorithm SHA256).Hash
    $RollbackAssetDir = Join-Path $TestRoot "rollback-assets"
    New-Item -ItemType Directory -Path $RollbackAssetDir | Out-Null
    $RollbackAsset = Join-Path $RollbackAssetDir $AssetName
    Copy-Item -LiteralPath $Asset -Destination $RollbackAsset
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::Open(
        $RollbackAsset,
        [IO.Compression.ZipArchiveMode]::Update
    )
    try {
        $oldReadme = $zip.GetEntry("README.md")
        $oldReadme.Delete()
        $newReadme = $zip.CreateEntry("README.md")
        $writer = [IO.StreamWriter]::new($newReadme.Open(), [Text.Encoding]::UTF8)
        try {
            $writer.Write("replacement resource from new packet`n")
        } finally {
            $writer.Dispose()
        }
    } finally {
        $zip.Dispose()
    }
    Write-AssetChecksum $RollbackAsset $AssetName
    $env:SFH_ASSET_DIR = $RollbackAssetDir
    $LockedBinary = Join-Path $InstallDir "sfh.exe"
    $BinaryLock = [IO.File]::Open(
        $LockedBinary,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::None
    )
    try {
        $FailedAsExpected = $false
        try {
            & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
        } catch {
            if ($_.Exception.Message -notmatch "Could not install the binary") {
                throw
            }
            $FailedAsExpected = $true
        }
        if (-not $FailedAsExpected) {
            throw "Installer committed resources while binary replacement was blocked"
        }
        if ((Get-FileHash -LiteralPath $RollbackMarker -Algorithm SHA256).Hash -cne
            $RollbackMarkerHash) {
            throw "Installer did not restore previous resources after binary refusal"
        }
        if ((Get-FileHash -LiteralPath (Join-Path $DataDir ".sfh-installer-inventory") `
                -Algorithm SHA256).Hash -cne $RollbackInventoryHash) {
            throw "Installer did not restore the previous inventory after binary refusal"
        }
    } finally {
        $BinaryLock.Dispose()
    }
    $env:SFH_ASSET_DIR = $AssetDir
    $ActualVersion = & $LockedBinary --version
    if ($LASTEXITCODE -ne 0 -or $ActualVersion -ne $ExpectedVersion) {
        throw "Previous binary was damaged by failed replacement"
    }

    $DuplicateAssetDir = Join-Path $TestRoot "duplicate-assets"
    New-Item -ItemType Directory -Path $DuplicateAssetDir | Out-Null
    $DuplicateAsset = Join-Path $DuplicateAssetDir $AssetName
    Copy-Item -LiteralPath $Asset -Destination $DuplicateAsset
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::Open($DuplicateAsset, [IO.Compression.ZipArchiveMode]::Update)
    try {
        $duplicate = $zip.CreateEntry("README.md")
        $writer = [IO.StreamWriter]::new($duplicate.Open(), [Text.Encoding]::ASCII)
        try {
            $writer.Write("duplicate")
        } finally {
            $writer.Dispose()
        }
    } finally {
        $zip.Dispose()
    }
    Write-AssetChecksum $DuplicateAsset $AssetName
    $env:SFH_ASSET_DIR = $DuplicateAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "duplicate-install"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "duplicate-data"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "duplicate member") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted a duplicate archive member"
    }

    $OversizedAssetDir = Join-Path $TestRoot "oversized-assets"
    New-Item -ItemType Directory -Path $OversizedAssetDir | Out-Null
    $OversizedAsset = Join-Path $OversizedAssetDir $AssetName
    $OversizedStream = [IO.File]::Open(
        $OversizedAsset,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $OversizedStream.SetLength(51MB)
    } finally {
        $OversizedStream.Dispose()
    }
    Write-AssetChecksum $OversizedAsset $AssetName
    $env:SFH_ASSET_DIR = $OversizedAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "oversized-install"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "oversized-data"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "exceeds the 50 MiB download-size limit") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or
        (Test-Path -LiteralPath (Join-Path $TestRoot "oversized-install\sfh.exe")) -or
        (Test-Path -LiteralPath (Join-Path $TestRoot "oversized-data"))) {
        throw "Installer accepted or installed an archive over the compressed-size limit"
    }

    $ManyAssetDir = Join-Path $TestRoot "many-assets"
    New-Item -ItemType Directory -Path $ManyAssetDir | Out-Null
    $ManyAsset = Join-Path $ManyAssetDir $AssetName
    Copy-Item -LiteralPath $Asset -Destination $ManyAsset
    $zip = [IO.Compression.ZipFile]::Open($ManyAsset, [IO.Compression.ZipArchiveMode]::Update)
    try {
        for ($index = 0; $index -lt 2001; $index++) {
            $entry = $zip.CreateEntry("tests/archive-member-limit/member-$index")
            $entry.Open().Dispose()
        }
    } finally {
        $zip.Dispose()
    }
    Write-AssetChecksum $ManyAsset $AssetName
    $env:SFH_ASSET_DIR = $ManyAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "many-install"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "many-data"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "exceeds the 2000-member limit") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected -or
        (Test-Path -LiteralPath (Join-Path $TestRoot "many-install\sfh.exe")) -or
        (Test-Path -LiteralPath (Join-Path $TestRoot "many-data"))) {
        throw "Installer accepted or installed an archive over the member-count limit"
    }

    $MissingAssetDir = Join-Path $TestRoot "missing-assets"
    New-Item -ItemType Directory -Path $MissingAssetDir | Out-Null
    $MissingAsset = Join-Path $MissingAssetDir $AssetName
    Copy-Item -LiteralPath $Asset -Destination $MissingAsset
    $zip = [IO.Compression.ZipFile]::Open($MissingAsset, [IO.Compression.ZipArchiveMode]::Update)
    try {
        foreach ($entry in @($zip.Entries | Where-Object { $_.FullName.StartsWith("schema/") })) {
            $entry.Delete()
        }
    } finally {
        $zip.Dispose()
    }
    Write-AssetChecksum $MissingAsset $AssetName
    $env:SFH_ASSET_DIR = $MissingAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "missing-install"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "missing-data"
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "required resource: schema/") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer accepted an archive with a missing resource directory"
    }

    $UnownedDataDir = Join-Path $TestRoot "unowned-data"
    New-Item -ItemType Directory -Path $UnownedDataDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $PackageDir "release-resources.txt") -Destination $UnownedDataDir
    Set-Content -LiteralPath (Join-Path $UnownedDataDir "personal.txt") -Value "keep"
    $env:SFH_ASSET_DIR = $AssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "unowned-install"
    $env:SFH_DATA_DIR = $UnownedDataDir
    $FailedAsExpected = $false
    try {
        & (Join-Path $RepoRoot "installers/sfh-installer.ps1")
    } catch {
        if ($_.Exception.Message -notmatch "not owned by the sfh installer") {
            throw
        }
        $FailedAsExpected = $true
    }
    if (-not $FailedAsExpected) {
        throw "Installer replaced an unowned resource directory"
    }
    if ((Get-Content -LiteralPath (Join-Path $UnownedDataDir "personal.txt") -Raw) -notmatch "keep") {
        throw "Installer modified an unowned resource directory"
    }

    $BadAssetDir = Join-Path $TestRoot "bad-assets"
    Copy-Item -LiteralPath $AssetDir -Destination $BadAssetDir -Recurse
    [IO.File]::AppendAllText(
        (Join-Path $BadAssetDir $AssetName),
        "corrupt"
    )

    $env:SFH_ASSET_DIR = $BadAssetDir
    $env:SFH_INSTALL_DIR = Join-Path $TestRoot "rejected"
    $env:SFH_DATA_DIR = Join-Path $TestRoot "rejected-data"
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
