& {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"

    $Repository = "Aero123421/SimpleFlowHarness"
    $ExpectedSignerCertificateSha256 = "{{WINDOWS_CODESIGN_CERT_SHA256}}"
    $ExpectedWindowsX64Sha256 = "{{WINDOWS_X64_SHA256}}"
    $InstallerVersion = "{{VERSION}}"
    $RequestedVersion = if ($env:SFH_VERSION) { $env:SFH_VERSION } else { "latest" }
    $InstallDir = if ($env:SFH_INSTALL_DIR) {
        $env:SFH_INSTALL_DIR
    } else {
        Join-Path $env:LOCALAPPDATA "Programs\sfh"
    }
    $DataDir = if ($env:SFH_DATA_DIR) {
        $env:SFH_DATA_DIR
    } else {
        Join-Path $env:LOCALAPPDATA "sfh-resources"
    }
    $StateDir = if ($env:SFH_STATE_DIR) {
        $env:SFH_STATE_DIR
    } elseif ($env:LOCALAPPDATA) {
        Join-Path $env:LOCALAPPDATA "sfh"
    } else {
        $null
    }
    $AssetDir = $env:SFH_ASSET_DIR
    $BaseUrlOverride = $env:SFH_BASE_URL
    $RequiredResources = @(
        "AGENTS.md"
        "CHANGELOG.md"
        "CONTRIBUTING.md"
        "LICENSE"
        "README.ja.md"
        "README.md"
        "SECURITY.md"
        "SUPPORT.md"
        "docs/"
        "examples/"
        "schema/"
        "skills/"
        "tests/"
    )
    $ExpectedManifest = ($RequiredResources -join "`n") + "`n"
    $OwnershipMarkerName = ".sfh-installer-owned"
    $ExpectedOwnershipMarker = "sfh installer resource directory v1`n"
    $InventoryName = ".sfh-installer-inventory"

    function Normalize-Version {
        param([Parameter(Mandatory)][string]$Version)

        if ($Version -notmatch '^v?\d+\.\d+\.\d+(?:[-.][0-9A-Za-z.-]+)?$') {
            if ($Version -ne "latest") {
                throw "Invalid SFH_VERSION '$Version' (expected latest, 1.2.3, or v1.2.3)"
            }
        }
        if ($AssetDir) {
            if ($Version -eq "latest") {
                return "latest"
            }
            if ($Version.StartsWith("v", [StringComparison]::Ordinal)) {
                return $Version
            }
            return "v$Version"
        }
        if ($InstallerVersion -notmatch '^\d+\.\d+\.\d+(?:[-.][0-9A-Za-z.-]+)?$') {
            throw "Official installer version is not configured"
        }
        $requestedWithoutV = $Version.TrimStart("v")
        if ($Version -ne "latest" -and $requestedWithoutV -cne $InstallerVersion) {
            throw "This installer is bound to sfh $InstallerVersion"
        }
        return "v$InstallerVersion"
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

        $limit = if ($Name.EndsWith(".sha256", [StringComparison]::Ordinal)) {
            4096L
        } else {
            50MB
        }
        Add-Type -AssemblyName System.Net.Http
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $handler = [Net.Http.HttpClientHandler]::new()
        $client = [Net.Http.HttpClient]::new($handler)
        $response = $null
        $inputStream = $null
        $outputStream = $null
        try {
            $client.DefaultRequestHeaders.UserAgent.ParseAdd("sfh-installer")
            $response = $client.GetAsync(
                "$BaseUrl/$Name",
                [Net.Http.HttpCompletionOption]::ResponseHeadersRead
            ).GetAwaiter().GetResult()
            if (-not $response.IsSuccessStatusCode) {
                throw "Could not download $Name (HTTP $([int]$response.StatusCode))"
            }
            $contentLength = $response.Content.Headers.ContentLength
            if ($null -ne $contentLength -and [long]$contentLength -gt $limit) {
                throw "$Name exceeds its download-size limit"
            }
            $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            $outputStream = [IO.File]::Open(
                $Destination,
                [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write,
                [IO.FileShare]::None
            )
            $buffer = New-Object byte[] 81920
            [long]$written = 0
            while (($read = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                if ($written -gt ($limit - $read)) {
                    throw "$Name exceeds its download-size limit"
                }
                $outputStream.Write($buffer, 0, $read)
                $written += $read
            }
        } catch {
            if ($outputStream) {
                $outputStream.Dispose()
                $outputStream = $null
            }
            Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue
            throw
        } finally {
            if ($outputStream) { $outputStream.Dispose() }
            if ($inputStream) { $inputStream.Dispose() }
            if ($response) { $response.Dispose() }
            $client.Dispose()
            $handler.Dispose()
        }
    }

    function Resolve-CanonicalInstallerPath {
        param([Parameter(Mandatory)][string]$Path)

        $candidate = [IO.Path]::GetFullPath($Path)
        $visited = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        for ($pass = 0; $pass -lt 64; $pass++) {
            if (-not $visited.Add($candidate)) {
                throw "Path contains a reparse-point cycle: $Path"
            }
            $root = [IO.Path]::GetPathRoot($candidate)
            $relative = $candidate.Substring($root.Length)
            $parts = $relative.Split(
                [char[]]@([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar),
                [StringSplitOptions]::RemoveEmptyEntries
            )
            $current = $root
            $rewritten = $false
            for ($index = 0; $index -lt $parts.Count; $index++) {
                $next = Join-Path $current $parts[$index]
                if (-not (Test-Path -LiteralPath $next)) {
                    $remainder = $parts[$index..($parts.Count - 1)] -join [IO.Path]::DirectorySeparatorChar
                    return [IO.Path]::GetFullPath((Join-Path $current $remainder)).TrimEnd("\", "/")
                }

                $item = Get-Item -LiteralPath $next -Force
                if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
                    $current = $item.FullName
                    continue
                }

                $targets = @($item.Target)
                if ($targets.Count -ne 1 -or [string]::IsNullOrWhiteSpace($targets[0])) {
                    throw "Cannot resolve reparse point while checking install paths: $next"
                }
                $target = $targets[0]
                if (-not [IO.Path]::IsPathRooted($target)) {
                    $target = Join-Path $item.DirectoryName $target
                }
                if ($index + 1 -lt $parts.Count) {
                    $remainder = $parts[($index + 1)..($parts.Count - 1)] -join [IO.Path]::DirectorySeparatorChar
                    $target = Join-Path $target $remainder
                }
                $candidate = [IO.Path]::GetFullPath($target)
                $rewritten = $true
                break
            }
            if (-not $rewritten) {
                return [IO.Path]::GetFullPath($current)
            }
        }
        throw "Path contains too many reparse points: $Path"
    }

    function Test-InstallerPathsOverlap {
        param(
            [Parameter(Mandatory)][string]$Left,
            [Parameter(Mandatory)][string]$Right
        )

        $separator = [IO.Path]::DirectorySeparatorChar
        $leftPrefix = $Left.TrimEnd("\", "/") + $separator
        $rightPrefix = $Right.TrimEnd("\", "/") + $separator
        return (
            $leftPrefix.StartsWith($rightPrefix, [StringComparison]::OrdinalIgnoreCase) -or
            $rightPrefix.StartsWith($leftPrefix, [StringComparison]::OrdinalIgnoreCase)
        )
    }

    function Assert-InstallerPathsDoNotOverlap {
        $canonicalInstall = Resolve-CanonicalInstallerPath $InstallDir
        $canonicalData = Resolve-CanonicalInstallerPath $DataDir
        if (Test-InstallerPathsOverlap $canonicalInstall $canonicalData) {
            throw "SFH_INSTALL_DIR and SFH_DATA_DIR must not overlap"
        }
        if ($StateDir) {
            $canonicalState = Resolve-CanonicalInstallerPath $StateDir
            if (Test-InstallerPathsOverlap $canonicalData $canonicalState) {
                throw "SFH_DATA_DIR and the sfh state directory must not overlap"
            }
        }
    }

    function Get-InstallerLockPath {
        $localAppData = [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::LocalApplicationData
        )
        if ([string]::IsNullOrWhiteSpace($localAppData)) {
            throw "Could not resolve the per-user installer lock directory"
        }
        return Join-Path $localAppData "sfh-installer.lock"
    }

    function Assert-SafeArchivePath {
        param([Parameter(Mandatory)][string]$Name)

        if (
            $Name.StartsWith("/", [StringComparison]::Ordinal) -or
            $Name.StartsWith("\", [StringComparison]::Ordinal) -or
            $Name.Contains("\") -or
            $Name.Contains(":") -or
            $Name.EndsWith("//", [StringComparison]::Ordinal)
        ) {
            throw "Archive contains an unsafe member path: $Name"
        }

        $withoutTrailingSlash = $Name.TrimEnd("/")
        if ([string]::IsNullOrEmpty($withoutTrailingSlash)) {
            throw "Archive contains an unsafe member path: $Name"
        }
        $components = $withoutTrailingSlash.Split("/")
        if ($components | Where-Object { $_ -in @("", ".", "..") }) {
            throw "Archive contains an unsafe member path: $Name"
        }

        $topLevel = $components[0]
        $allowedFiles = @(
            "sfh.exe", "release-resources.txt", "AGENTS.md", "CHANGELOG.md",
            "CONTRIBUTING.md", "LICENSE", "README.ja.md", "README.md",
            "SECURITY.md", "SUPPORT.md"
        )
        $allowedDirectories = @("docs", "examples", "schema", "skills", "tests")
        if ($components.Count -eq 1 -and $topLevel -cin $allowedFiles) {
            return
        }
        if ($topLevel -cnotin $allowedDirectories) {
            throw "Archive contains an unexpected member: $Name"
        }
    }

    function Assert-SafeZipArchive {
        param([Parameter(Mandatory)][string]$Path)

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($Path)
        try {
            [long]$totalUncompressed = 0
            $memberCount = 0
            $seen = [Collections.Generic.HashSet[string]]::new(
                [StringComparer]::OrdinalIgnoreCase
            )
            foreach ($entry in $zip.Entries) {
                $memberCount++
                if ($memberCount -gt 2000) {
                    throw "Archive exceeds the 2000-member limit"
                }
                if ($entry.Length -gt 32MB) {
                    throw "Archive member exceeds the 32 MiB size limit: $($entry.FullName)"
                }
                if ($totalUncompressed -gt (256MB - $entry.Length)) {
                    throw "Archive exceeds the 256 MiB uncompressed-size limit"
                }
                $totalUncompressed += $entry.Length
                Assert-SafeArchivePath $entry.FullName
                if (-not $seen.Add($entry.FullName.TrimEnd("/"))) {
                    throw "Archive contains a duplicate member: $($entry.FullName)"
                }

                $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xffff
                $unixType = $unixMode -band 0xf000
                if ($unixType -notin @(0, 0x4000, 0x8000)) {
                    throw "Archive contains a link or special file: $($entry.FullName)"
                }
                if (($entry.ExternalAttributes -band 0x400) -ne 0) {
                    throw "Archive contains a reparse point: $($entry.FullName)"
                }
            }
        } finally {
            $zip.Dispose()
        }
    }

    function Assert-ResourceManifest {
        param([Parameter(Mandatory)][string]$Path)

        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Archive does not contain release-resources.txt"
        }
        $manifestItem = Get-Item -LiteralPath $Path -Force
        if (($manifestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "release-resources.txt must not be a reparse point"
        }
        $expectedBytes = [Text.Encoding]::ASCII.GetBytes($ExpectedManifest)
        $actualBytes = [IO.File]::ReadAllBytes($Path)
        if (
            [Convert]::ToBase64String($actualBytes) -cne
            [Convert]::ToBase64String($expectedBytes)
        ) {
            throw "release-resources.txt does not match the required resource set"
        }
    }

    function Assert-OwnershipMarker {
        param([Parameter(Mandatory)][string]$Path)

        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Resource destination has no installer ownership marker"
        }
        $marker = Get-Item -LiteralPath $Path -Force
        if (($marker.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Resource destination ownership marker must not be a reparse point"
        }
        Assert-NoAlternateDataStreams $Path $OwnershipMarkerName
        $expectedBytes = [Text.Encoding]::ASCII.GetBytes($ExpectedOwnershipMarker)
        $actualBytes = [IO.File]::ReadAllBytes($Path)
        if (
            [Convert]::ToBase64String($actualBytes) -cne
            [Convert]::ToBase64String($expectedBytes)
        ) {
            throw "Resource destination ownership marker is invalid"
        }
    }

    function Assert-NoAlternateDataStreams {
        param(
            [Parameter(Mandatory)][string]$Path,
            [Parameter(Mandatory)][string]$Relative
        )

        $streams = @(Get-Item -LiteralPath $Path -Stream * -ErrorAction Stop)
        foreach ($stream in $streams) {
            if ($stream.Stream -cnotin @(':$DATA', '$DATA')) {
                throw "Resource file contains an alternate data stream: $Relative"
            }
        }
    }

    function Get-ResourceInventoryText {
        param([Parameter(Mandatory)][string]$Root)

        $rootItem = Get-Item -LiteralPath $Root -Force
        if (
            -not $rootItem.PSIsContainer -or
            ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "Resource tree root must be a regular directory: $Root"
        }

        $rootPrefix = $rootItem.FullName.TrimEnd("\", "/") + [IO.Path]::DirectorySeparatorChar
        $paths = New-Object 'System.Collections.Generic.List[string]'
        $records = [Collections.Generic.Dictionary[string, string]]::new(
            [StringComparer]::Ordinal
        )
        $pending = New-Object System.Collections.Stack
        $pending.Push([IO.DirectoryInfo]$rootItem)
        while ($pending.Count -gt 0) {
            $directory = [IO.DirectoryInfo]$pending.Pop()
            foreach ($item in $directory.GetFileSystemInfos()) {
                $relative = $item.FullName.Substring($rootPrefix.Length).Replace("\", "/")
                foreach ($character in $relative.ToCharArray()) {
                    $code = [int]$character
                    if ($code -lt 32 -or $code -gt 126) {
                        throw "Resource tree contains a non-ASCII path: $relative"
                    }
                }
                if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    throw "Resource tree contains a reparse point: $relative"
                }
                if ($item -is [IO.FileInfo]) {
                    Assert-NoAlternateDataStreams $item.FullName $relative
                }
                if ($relative -ceq $OwnershipMarkerName -or $relative -ceq $InventoryName) {
                    continue
                }
                if ($item -is [IO.DirectoryInfo]) {
                    $paths.Add($relative)
                    $records.Add($relative, "d - $relative/")
                    $pending.Push($item)
                } elseif ($item -is [IO.FileInfo]) {
                    $digest = (
                        Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256
                    ).Hash.ToLowerInvariant()
                    $paths.Add($relative)
                    $records.Add($relative, "f $digest $relative")
                } else {
                    throw "Resource tree contains a special entry: $relative"
                }
            }
        }

        $sorted = [string[]]$paths.ToArray()
        [Array]::Sort($sorted, [StringComparer]::Ordinal)
        $lines = foreach ($path in $sorted) {
            $records[$path]
        }
        return ($lines -join "`n") + "`n"
    }

    function Write-ResourceInventory {
        param(
            [Parameter(Mandatory)][string]$Root,
            [Parameter(Mandatory)][string]$Path
        )

        [IO.File]::WriteAllText(
            $Path,
            (Get-ResourceInventoryText $Root),
            [Text.Encoding]::ASCII
        )
    }

    function Assert-ResourceInventory {
        param([Parameter(Mandatory)][string]$Root)

        $path = Join-Path $Root $InventoryName
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Resource destination has no installer inventory: $DataDir"
        }
        $inventory = Get-Item -LiteralPath $path -Force
        if (($inventory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Resource destination inventory must not be a reparse point: $DataDir"
        }
        Assert-NoAlternateDataStreams $path $InventoryName
        $expectedBytes = [Text.Encoding]::ASCII.GetBytes((Get-ResourceInventoryText $Root))
        $actualBytes = [IO.File]::ReadAllBytes($path)
        if (
            [Convert]::ToBase64String($actualBytes) -cne
            [Convert]::ToBase64String($expectedBytes)
        ) {
            throw "Resource destination does not match its installer inventory: $DataDir"
        }
    }

    function Assert-ActivatedResourceTree {
        param(
            [Parameter(Mandatory)][string]$Root,
            [Parameter(Mandatory)][string]$ExpectedInventoryPath
        )

        Assert-ResourceManifest (Join-Path $Root "release-resources.txt")
        Assert-OwnershipMarker (Join-Path $Root $OwnershipMarkerName)
        $inventoryPath = Join-Path $Root $InventoryName
        if (-not (Test-Path -LiteralPath $inventoryPath -PathType Leaf)) {
            throw "Activated resource inventory is missing before binary installation"
        }
        $expectedBytes = [IO.File]::ReadAllBytes($ExpectedInventoryPath)
        $actualBytes = [IO.File]::ReadAllBytes($inventoryPath)
        if (
            [Convert]::ToBase64String($actualBytes) -cne
            [Convert]::ToBase64String($expectedBytes)
        ) {
            throw "Activated resource inventory changed before binary installation"
        }
        Assert-ResourceInventory $Root
    }

    function Remove-InventoriedResourceTree {
        param([Parameter(Mandatory)][string]$Root)

        $inventoryPath = Join-Path $Root $InventoryName
        if (-not (Test-Path -LiteralPath $inventoryPath -PathType Leaf)) {
            throw "Resource destination has no installer inventory: $Root"
        }
        $inventoryItem = Get-Item -LiteralPath $inventoryPath -Force
        if (($inventoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Resource destination inventory must not be a reparse point: $Root"
        }
        $trustedInventory = Get-ResourceInventoryText $Root
        $trustedBytes = [Text.Encoding]::ASCII.GetBytes($trustedInventory)
        $savedBytes = [IO.File]::ReadAllBytes($inventoryPath)
        if (
            [Convert]::ToBase64String($savedBytes) -cne
            [Convert]::ToBase64String($trustedBytes)
        ) {
            throw "Resource destination does not match its installer inventory: $Root"
        }
        $directories = New-Object 'System.Collections.Generic.List[string]'
        foreach ($record in $trustedInventory.TrimEnd("`n").Split("`n")) {
            if ($record -match '^f ([0-9a-f]{64}) (.+)$') {
                $expectedDigest = $Matches[1]
                $relative = $Matches[2]
                $path = Join-Path $Root ($relative.Replace("/", "\"))
                $item = Get-Item -LiteralPath $path -Force
                if (
                    $item -isnot [IO.FileInfo] -or
                    ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
                ) {
                    throw "Inventoried resource changed type before deletion: $relative"
                }
                Assert-NoAlternateDataStreams $path $relative
                $actualDigest = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($actualDigest -cne $expectedDigest) {
                    throw "Inventoried resource changed content before deletion: $relative"
                }
                [IO.File]::Delete($path)
                if (Test-Path -LiteralPath $path) {
                    throw "Could not delete inventoried resource: $relative"
                }
            } elseif ($record -match '^d - (.+)/$') {
                $directories.Add($Matches[1])
            } else {
                throw "Resource inventory contains an invalid record: $record"
            }
        }

        $knownDirectories = @($directories | Sort-Object { $_.Length } -Descending)
        foreach ($relative in $knownDirectories) {
            $path = Join-Path $Root ($relative.Replace("/", "\"))
            $item = Get-Item -LiteralPath $path -Force
            if (
                $item -isnot [IO.DirectoryInfo] -or
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
            ) {
                throw "Inventoried resource directory changed type before deletion: $relative"
            }
            [IO.Directory]::Delete($path, $false)
        }

        foreach ($privateName in @($OwnershipMarkerName, $InventoryName)) {
            $path = Join-Path $Root $privateName
            $item = Get-Item -LiteralPath $path -Force
            if (
                $item -isnot [IO.FileInfo] -or
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
            ) {
                throw "Installer private metadata changed type before deletion: $privateName"
            }
            Assert-NoAlternateDataStreams $path $privateName
            [IO.File]::Delete($path)
        }
        [IO.Directory]::Delete($Root, $false)
    }

    function Restore-PreviousResourceTree {
        param(
            [Parameter(Mandatory)][string]$Root,
            [Parameter(Mandatory)][string]$Previous,
            [Parameter(Mandatory)][string]$Transaction
        )

        $recovery = $null
        if (Test-Path -LiteralPath $Root) {
            try {
                Remove-InventoriedResourceTree $Root
            } catch {
                $recovery = Join-Path $Transaction "activated-recovery"
                if (Test-Path -LiteralPath $recovery) {
                    throw "Activated-resource recovery destination already exists: $recovery"
                }
                Move-Item -LiteralPath $Root -Destination $recovery
            }
        }
        if (Test-Path -LiteralPath $Root) {
            throw "Cannot restore previous resources because the destination exists: $Root"
        }
        Move-Item -LiteralPath $Previous -Destination $Root
        return $recovery
    }

    function Assert-ExtractedResources {
        param([Parameter(Mandatory)][string]$Root)

        Get-ChildItem -LiteralPath $Root -Force -Recurse | ForEach-Object {
            if (($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Archive contains a reparse point: $($_.FullName)"
            }
        }
        foreach ($resource in $RequiredResources) {
            $relative = $resource.TrimEnd("/")
            $path = Join-Path $Root $relative
            $expectedType = if ($resource.EndsWith("/")) { "Container" } else { "Leaf" }
            if (-not (Test-Path -LiteralPath $path -PathType $expectedType)) {
                throw "Archive does not contain required resource: $resource"
            }
        }
    }

    $version = Normalize-Version $RequestedVersion
    # Windows PowerShell 5.1 can expose RuntimeInformation without OSArchitecture.
    # PROCESSOR_ARCHITEW6432 reports the native architecture when running under WOW64.
    $nativeArchitecture = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }
    if ([string]::IsNullOrWhiteSpace($nativeArchitecture)) {
        throw "Could not determine Windows CPU architecture"
    }
    $architecture = switch ($nativeArchitecture.ToUpperInvariant()) {
        "AMD64" { "X64" }
        "ARM64" { "Arm64" }
        default { $nativeArchitecture }
    }
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
    } else {
        $BaseUrl = "https://github.com/$Repository/releases/download/$version"
    }

    Assert-InstallerPathsDoNotOverlap

    $tempDir = Join-Path ([IO.Path]::GetTempPath()) "sfh-install-$([Guid]::NewGuid().ToString('N'))"
    $archive = Join-Path $tempDir $asset
    $checksum = "$archive.sha256"
    $extractDir = Join-Path $tempDir "extract"
    $expectedStagedInventory = Join-Path $tempDir "staged-resource-inventory.txt"
    $staged = $null
    $dataTransaction = $null
    $dataPrevious = $null
    $dataInstalled = $false
    $dataHadPrevious = $false
    $dataCommitted = $false
    $dataRecovery = $null
    $binaryBackup = $null
    $embeddedInventoryBytes = $null
    $installLocks = New-Object 'System.Collections.Generic.List[object]'

    try {
        New-Item -ItemType Directory -Path $tempDir | Out-Null

        $lockPath = Get-InstallerLockPath
        [IO.Directory]::CreateDirectory((Split-Path -Parent $lockPath)) | Out-Null
        try {
            $lockStream = [IO.FileStream]::new(
                $lockPath,
                [IO.FileMode]::CreateNew,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None,
                4096,
                [IO.FileOptions]::DeleteOnClose
            )
        } catch {
            $lockItem = Get-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
            if ($lockItem -and
                ($lockItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Global installer lock is a reparse point: $lockPath"
            }
            $staleProbe = $null
            try {
                $staleProbe = [IO.FileStream]::new(
                    $lockPath,
                    [IO.FileMode]::Open,
                    [IO.FileAccess]::ReadWrite,
                    [IO.FileShare]::None
                )
            } catch {
                throw "Another sfh installer is active (global lock: $lockPath)"
            } finally {
                if ($staleProbe) {
                    $staleProbe.Dispose()
                }
            }
            throw "Stale sfh installer lock requires inspection and manual removal: $lockPath"
        }
        $installLocks.Add([PSCustomObject]@{ Path = $lockPath; Stream = $lockStream })
        $owner = [Text.Encoding]::ASCII.GetBytes("$PID`n")
        $lockStream.Write($owner, 0, $owner.Length)
        $lockStream.Flush()

        Write-Host "Downloading $asset..."
        Copy-ReleaseAsset $asset $archive
        Copy-ReleaseAsset "$asset.sha256" $checksum

        if ((Get-Item -LiteralPath $archive).Length -gt 50MB) {
            throw "$asset exceeds the 50 MiB download-size limit"
        }

        $expected = ((Get-Content -LiteralPath $checksum -Raw) -split '\s+')[0].ToLowerInvariant()
        if ($expected -notmatch '^[0-9a-f]{64}$') {
            throw "Invalid SHA-256 file for $asset"
        }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "SHA-256 mismatch for $asset (expected $expected, got $actual)"
        }
        if (-not $AssetDir) {
            if ($ExpectedWindowsX64Sha256 -notmatch '^[0-9a-f]{64}$') {
                throw "Official archive SHA-256 is not configured in this installer"
            }
            if ($expected -cne $ExpectedWindowsX64Sha256) {
                throw "Release sidecar SHA-256 does not match this installer"
            }
            if ($actual -cne $ExpectedWindowsX64Sha256) {
                throw "Downloaded archive SHA-256 does not match this installer"
            }
        }

        Assert-SafeZipArchive $archive
        New-Item -ItemType Directory -Path $extractDir | Out-Null
        Expand-Archive -LiteralPath $archive -DestinationPath $extractDir
        $executable = Join-Path $extractDir "sfh.exe"
        if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw "Archive does not contain sfh.exe"
        }
        Assert-ResourceManifest (Join-Path $extractDir "release-resources.txt")
        Assert-ExtractedResources $extractDir

        if (Test-Path -LiteralPath $InstallDir) {
            $installDestination = Get-Item -LiteralPath $InstallDir -Force
            if (-not $installDestination.PSIsContainer) {
                throw "Install destination is not a directory: $InstallDir"
            }
            if (($installDestination.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Install destination must not be a reparse point: $InstallDir"
            }
        } else {
            New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        }
        $binaryDestination = Join-Path $InstallDir "sfh.exe"
        if (Test-Path -LiteralPath $binaryDestination) {
            $currentBinary = Get-Item -LiteralPath $binaryDestination -Force
            if ($currentBinary.PSIsContainer) {
                throw "Binary destination is a directory: $binaryDestination"
            }
            if (($currentBinary.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Binary destination must not be a reparse point: $binaryDestination"
            }
        }
        $staged = Join-Path $InstallDir ".sfh.new-$PID-$([Guid]::NewGuid().ToString('N')).exe"
        Copy-Item -LiteralPath $executable -Destination $staged
        Unblock-File -LiteralPath $staged -ErrorAction SilentlyContinue
        if (-not $AssetDir) {
            if ($ExpectedSignerCertificateSha256 -cne "UNSIGNED") {
                if ($ExpectedSignerCertificateSha256 -notmatch '^[0-9a-f]{64}$') {
                    throw "Official Windows signer identity is invalid in this installer"
                }
                $signature = Get-AuthenticodeSignature -LiteralPath $staged
                if ($signature.Status.ToString() -cne "Valid") {
                    throw "Official Windows build failed Authenticode verification: $($signature.Status)"
                }
                if (-not $signature.TimeStamperCertificate) {
                    throw "Official Windows build has no Authenticode timestamp certificate"
                }
                if (-not $signature.SignerCertificate) {
                    throw "Official Windows build has no signer certificate"
                }
                $signerHasher = [Security.Cryptography.SHA256]::Create()
                try {
                    $signerDigest = [BitConverter]::ToString(
                        $signerHasher.ComputeHash($signature.SignerCertificate.RawData)
                    ).Replace("-", "").ToLowerInvariant()
                } finally {
                    $signerHasher.Dispose()
                }
                if ($signerDigest -cne $ExpectedSignerCertificateSha256) {
                    throw "Official Windows build signer identity does not match this release channel"
                }
            }
            $manifestProcessInfo = New-Object Diagnostics.ProcessStartInfo
            $manifestProcessInfo.FileName = $staged
            $manifestProcessInfo.Arguments = "__release-manifest"
            $manifestProcessInfo.UseShellExecute = $false
            $manifestProcessInfo.CreateNoWindow = $true
            $manifestProcessInfo.RedirectStandardOutput = $true
            $manifestProcessInfo.RedirectStandardError = $true
            $manifestProcess = [Diagnostics.Process]::Start($manifestProcessInfo)
            $embeddedInventoryStream = New-Object IO.MemoryStream
            try {
                $manifestProcess.StandardOutput.BaseStream.CopyTo($embeddedInventoryStream)
                $manifestProcess.WaitForExit()
                if ($manifestProcess.ExitCode -ne 0) {
                    throw "Official build did not provide its embedded release manifest"
                }
                $embeddedInventoryBytes = $embeddedInventoryStream.ToArray()
            } finally {
                $embeddedInventoryStream.Dispose()
                $manifestProcess.Dispose()
            }
            $downloadedVersion = & $staged --version
            if ($LASTEXITCODE -ne 0 -or $downloadedVersion -cne "sfh $InstallerVersion") {
                throw "Downloaded binary version does not match installer version $InstallerVersion"
            }
        } else {
            & $staged --version *> $null
            if ($LASTEXITCODE -ne 0) {
                throw "Downloaded binary did not start"
            }
        }

        $dataParent = Split-Path -Parent ([IO.Path]::GetFullPath($DataDir))
        New-Item -ItemType Directory -Force -Path $dataParent | Out-Null
        $dataTransaction = Join-Path $dataParent ".sfh-data-$PID-$([Guid]::NewGuid().ToString('N'))"
        $stagedData = Join-Path $dataTransaction "new"
        $dataPrevious = Join-Path $dataTransaction "previous"
        New-Item -ItemType Directory -Path $stagedData | Out-Null
        foreach ($resource in @($RequiredResources) + "release-resources.txt") {
            $relative = $resource.TrimEnd("/")
            Copy-Item -LiteralPath (Join-Path $extractDir $relative) -Destination $stagedData -Recurse
        }
        [IO.File]::WriteAllText(
            (Join-Path $stagedData $OwnershipMarkerName),
            $ExpectedOwnershipMarker,
            [Text.Encoding]::ASCII
        )
        Write-ResourceInventory $stagedData (Join-Path $stagedData $InventoryName)
        Copy-Item -LiteralPath (Join-Path $stagedData $InventoryName) `
            -Destination $expectedStagedInventory
        if (-not $AssetDir) {
            $stagedBytes = [IO.File]::ReadAllBytes((Join-Path $stagedData $InventoryName))
            if (
                [Convert]::ToBase64String($embeddedInventoryBytes) -cne
                [Convert]::ToBase64String($stagedBytes)
            ) {
                throw "Official build release manifest does not match the downloaded resources"
            }
        }

        if (Test-Path -LiteralPath $DataDir) {
            $destination = Get-Item -LiteralPath $DataDir -Force
            if (($destination.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Resource destination must not be a reparse point: $DataDir"
            }
            if (-not $destination.PSIsContainer) {
                throw "Resource destination is not a directory: $DataDir"
            }
            $ownedManifest = Join-Path $DataDir "release-resources.txt"
            try {
                Assert-ResourceManifest $ownedManifest
                Assert-OwnershipMarker (Join-Path $DataDir $OwnershipMarkerName)
            } catch {
                throw "Refusing to replace a resource directory not owned by the sfh installer: $DataDir"
            }
            Assert-ResourceInventory $DataDir
            Move-Item -LiteralPath $DataDir -Destination $dataPrevious
            $dataHadPrevious = $true
            try {
                Assert-ResourceInventory $dataPrevious
            } catch {
                $inventoryError = $_
                try {
                    Move-Item -LiteralPath $dataPrevious -Destination $DataDir
                    $dataHadPrevious = $false
                } catch {
                    $dataInstalled = $true
                    throw "Resource destination changed while it was being staged and could not be restored: $($inventoryError.Exception.Message)"
                }
                throw "Resource destination changed while it was being staged: $($inventoryError.Exception.Message)"
            }
        }
        try {
            Move-Item -LiteralPath $stagedData -Destination $DataDir
        } catch {
            if (
                (Test-Path -LiteralPath $dataPrevious -PathType Container) -and
                -not (Test-Path -LiteralPath $DataDir)
            ) {
                Move-Item -LiteralPath $dataPrevious -Destination $DataDir
                $dataHadPrevious = $false
            }
            throw "Could not install resources to ${DataDir}: $($_.Exception.Message)"
        }
        $dataInstalled = $true
        Assert-ActivatedResourceTree $DataDir $expectedStagedInventory

        if ($dataHadPrevious) {
            try {
                Assert-ResourceInventory $dataPrevious
            } catch {
                $inventoryError = $_
                try {
                    $dataRecovery = Restore-PreviousResourceTree `
                        -Root $DataDir `
                        -Previous $dataPrevious `
                        -Transaction $dataTransaction
                    $dataInstalled = $false
                    $dataHadPrevious = $false
                } catch {
                    throw "Resource destination changed before binary installation and could not be restored: $($inventoryError.Exception.Message)"
                }
                throw "Resource destination changed before binary installation: $($inventoryError.Exception.Message)"
            }
        }
        Assert-ActivatedResourceTree $DataDir $expectedStagedInventory

        try {
            if (Test-Path -LiteralPath $binaryDestination -PathType Leaf) {
                $binaryBackup = Join-Path $InstallDir ".sfh.previous-$PID-$([Guid]::NewGuid().ToString('N')).exe"
                [IO.File]::Replace($staged, $binaryDestination, $binaryBackup, $true)
            } else {
                [IO.File]::Move($staged, $binaryDestination)
            }
            $staged = $null
            $dataCommitted = $true
        } catch {
            if ($dataHadPrevious -and (Test-Path -LiteralPath $dataPrevious)) {
                $dataRecovery = Restore-PreviousResourceTree `
                    -Root $DataDir `
                    -Previous $dataPrevious `
                    -Transaction $dataTransaction
                $dataHadPrevious = $false
            } elseif (Test-Path -LiteralPath $DataDir) {
                try {
                    Remove-InventoriedResourceTree $DataDir
                } catch {
                    $dataRecovery = Join-Path $dataTransaction "activated-recovery"
                    Move-Item -LiteralPath $DataDir -Destination $dataRecovery
                }
            }
            $dataInstalled = $false
            throw "Could not install the binary to ${binaryDestination}: $($_.Exception.Message)"
        }

        if ($dataHadPrevious -and (Test-Path -LiteralPath $dataPrevious)) {
            try {
                Remove-InventoriedResourceTree $dataPrevious
            } catch {
                $dataRecovery = $dataPrevious
                Write-Warning "Preserved unexpected previous resource data at $dataRecovery"
            }
        }
        $dataHadPrevious = $false
        if (-not $dataRecovery) {
            [IO.Directory]::Delete($dataTransaction, $false)
            $dataTransaction = $null
        }
        $dataPrevious = $null
        $dataInstalled = $false
        if ($binaryBackup -and (Test-Path -LiteralPath $binaryBackup)) {
            Remove-Item -LiteralPath $binaryBackup -Force
        }
        $binaryBackup = $null

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

        $installedVersion = & $binaryDestination --version
        if ($LASTEXITCODE -ne 0) {
            throw "Installed binary did not start"
        }
        Write-Host "Installed $installedVersion to $(Join-Path $InstallDir 'sfh.exe')"
        Write-Host "Installed resources to $DataDir"
    } finally {
        $cleanupFailure = $null
        if ($dataTransaction -and (Test-Path -LiteralPath $dataTransaction)) {
            try {
                if (-not $dataCommitted) {
                    if ($dataInstalled -and (Test-Path -LiteralPath $DataDir)) {
                        if ($dataHadPrevious) {
                            $dataRecovery = Restore-PreviousResourceTree `
                                -Root $DataDir `
                                -Previous $dataPrevious `
                                -Transaction $dataTransaction
                            $dataHadPrevious = $false
                        } else {
                            try {
                                Remove-InventoriedResourceTree $DataDir
                            } catch {
                                $dataRecovery = Join-Path $dataTransaction "activated-recovery"
                                Move-Item -LiteralPath $DataDir -Destination $dataRecovery
                            }
                        }
                        $dataInstalled = $false
                    }
                    if ($dataHadPrevious) {
                        if (Test-Path -LiteralPath $DataDir) {
                            throw "Cannot restore previous resources because the destination exists: $DataDir"
                        }
                        Move-Item -LiteralPath $dataPrevious -Destination $DataDir
                        $dataHadPrevious = $false
                    }
                }
                if (-not $dataHadPrevious -and -not $dataRecovery) {
                    $transactionNew = Join-Path $dataTransaction "new"
                    if (Test-Path -LiteralPath $transactionNew) {
                        try {
                            Remove-InventoriedResourceTree $transactionNew
                        } catch {
                            $dataRecovery = $transactionNew
                        }
                    }
                    if (-not $dataRecovery) {
                        [IO.Directory]::Delete($dataTransaction, $false)
                        $dataTransaction = $null
                    }
                }
            } catch {
                $cleanupFailure = $_
            }
        }
        if ($staged -and (Test-Path -LiteralPath $staged)) {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $tempDir) {
            Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($binaryBackup -and (Test-Path -LiteralPath $binaryBackup)) {
            Remove-Item -LiteralPath $binaryBackup -Force -ErrorAction SilentlyContinue
        }
        if ($dataRecovery) {
            Write-Warning "Preserved unexpected resource data at $dataRecovery"
        }
        $lockFailure = $null
        for ($index = $installLocks.Count - 1; $index -ge 0; $index--) {
            $lock = $installLocks[$index]
            try {
                $lock.Stream.Dispose()
                if (Test-Path -LiteralPath $lock.Path) {
                    throw "Installer lock was not removed when its owner handle closed: $($lock.Path)"
                }
            } catch {
                if (-not $lockFailure) {
                    $lockFailure = $_
                }
            }
        }
        if ($cleanupFailure) {
            throw "Could not restore the previous resources: $($cleanupFailure.Exception.Message)"
        }
        if ($lockFailure) {
            throw "Could not release installer lock: $($lockFailure.Exception.Message)"
        }
    }
}
