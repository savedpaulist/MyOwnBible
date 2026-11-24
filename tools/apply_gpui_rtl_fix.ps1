# ASCII-only source for compatibility with Windows PowerShell 5.1 default file decoding.
[CmdletBinding()]
param(
    [switch]$KeepTemporaryFiles
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Port the two immutable commits that implement the current upstream GPUI
# Windows RTL fix (#67281) onto the exact GPUI PRE 0.3.12 sources used here.
# Both patches are dry-run checked against staging copies before the project
# vendor folders or Cargo.toml are changed.

$root = (Get-Location).Path
$manifest = Join-Path $root 'Cargo.toml'
$lock = Join-Path $root 'Cargo.lock'
if (-not (Test-Path -LiteralPath $manifest)) {
    throw "Cargo.toml was not found in the project root: $manifest"
}
if (-not (Test-Path -LiteralPath $lock)) {
    throw "Cargo.lock was not found in the project root: $lock"
}

$lockText = Get-Content -LiteralPath $lock -Raw -Encoding UTF8
function Assert-LockedVersion([string]$Package, [string]$Version) {
    $escapedPackage = [regex]::Escape($Package)
    $escapedVersion = [regex]::Escape($Version)
    $pattern = "(?ms)^\[\[package\]\]\s+name\s*=\s*`"$escapedPackage`"\s+version\s*=\s*`"$escapedVersion`""
    if (-not [regex]::IsMatch($lockText, $pattern)) {
        throw "Cargo.lock does not contain the exact version $Package $Version. This installer targets GPUI PRE 0.3.12. No project files were changed."
    }
}
Assert-LockedVersion 'gpui-pre' '0.3.12'
Assert-LockedVersion 'gpui-pre-windows' '0.3.12'

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$registrySourceRoot = Join-Path $cargoHome 'registry\src'
if (-not (Test-Path -LiteralPath $registrySourceRoot)) {
    throw "Cargo registry sources were not found: $registrySourceRoot. Run cargo fetch first."
}

$registryRoots = @(Get-ChildItem -LiteralPath $registrySourceRoot -Directory -ErrorAction Stop)
$coreCandidates = @(
    foreach ($registryRoot in $registryRoots) {
        $candidate = Join-Path $registryRoot.FullName 'gpui-pre-0.3.12'
        if ((Test-Path (Join-Path $candidate 'src\text_system\line.rs')) -and
            (Test-Path (Join-Path $candidate 'src\text_system\line_layout.rs'))) { $candidate }
    }
)
$windowsCandidates = @(
    foreach ($registryRoot in $registryRoots) {
        $candidate = Join-Path $registryRoot.FullName 'gpui-pre-windows-0.3.12'
        if (Test-Path (Join-Path $candidate 'src\direct_write.rs')) { $candidate }
    }
)
if ($coreCandidates.Count -eq 0 -or $windowsCandidates.Count -eq 0) {
    throw 'Could not find gpui-pre 0.3.12 and gpui-pre-windows 0.3.12 sources. Run cargo fetch first.'
}
if ($coreCandidates.Count -gt 1 -or $windowsCandidates.Count -gt 1) {
    throw 'Multiple copies of GPUI PRE 0.3.12 were found in the Cargo registry. Remove duplicates or set CARGO_HOME explicitly.'
}
$coreRegistry = $coreCandidates[0]
$windowsRegistry = $windowsCandidates[0]

$coreVendor = Join-Path $root 'vendor\gpui-pre'
$windowsVendor = Join-Path $root 'vendor\gpui-pre-windows'
if ((Test-Path -LiteralPath $coreVendor) -or (Test-Path -LiteralPath $windowsVendor)) {
    throw 'vendor\gpui-pre or vendor\gpui-pre-windows already exists. The installer will not overwrite an existing vendor directory.'
}

$gitCommand = Get-Command git -ErrorAction SilentlyContinue
if (-not $gitCommand) {
    throw 'Git for Windows is required to apply the upstream diffs, but git was not found. Install Git and run this script again.'
}

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ('BibleOWN-GPUI-RTL-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $stage 'crates\gpui\src\text_system') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stage 'crates\gpui_windows\src') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $coreRegistry 'src\text_system\line.rs') -Destination (Join-Path $stage 'crates\gpui\src\text_system\line.rs')
Copy-Item -LiteralPath (Join-Path $coreRegistry 'src\text_system\line_layout.rs') -Destination (Join-Path $stage 'crates\gpui\src\text_system\line_layout.rs')
Copy-Item -LiteralPath (Join-Path $windowsRegistry 'src\direct_write.rs') -Destination (Join-Path $stage 'crates\gpui_windows\src\direct_write.rs')

try {
    $patchOne = Join-Path $stage '0027a56.diff'
    $patchTwo = Join-Path $stage '3aad1f8.diff'
    $urlOne = 'https://github.com/zed-industries/zed/commit/0027a56b212c9a89b2ab3e8ae94d019e20460d1d.diff'
    $urlTwo = 'https://github.com/zed-industries/zed/commit/3aad1f873332d67c9360b33ec47b2409a165a515.diff'

    Invoke-WebRequest -Uri $urlOne -OutFile $patchOne
    Invoke-WebRequest -Uri $urlTwo -OutFile $patchTwo
    foreach ($patchFile in @($patchOne, $patchTwo)) {
        $patchText = Get-Content -LiteralPath $patchFile -Raw -Encoding UTF8
        if ($patchText -notmatch '(?m)^diff --git ') {
            throw "GitHub returned an invalid diff: $patchFile"
        }
    }

    # Verify both commits against staging before modifying project files.
    & $gitCommand.Source -C $stage apply --check $patchOne
    if ($LASTEXITCODE -ne 0) {
        throw 'The first upstream diff does not match the GPUI PRE 0.3.12 sources. No project files were changed.'
    }
    & $gitCommand.Source -C $stage apply $patchOne
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the first upstream diff to the staging copy.' }

    & $gitCommand.Source -C $stage apply --check $patchTwo
    if ($LASTEXITCODE -ne 0) {
        throw 'The second upstream diff does not match the GPUI PRE 0.3.12 sources. No project files were changed.'
    }
    & $gitCommand.Source -C $stage apply $patchTwo
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the second upstream diff to the staging copy.' }

    $patchedDirectWrite = Get-Content -LiteralPath (Join-Path $stage 'crates\gpui_windows\src\direct_write.rs') -Raw -Encoding UTF8
    $patchedLineLayout = Get-Content -LiteralPath (Join-Path $stage 'crates\gpui\src\text_system\line_layout.rs') -Raw -Encoding UTF8
    $patchedLine = Get-Content -LiteralPath (Join-Path $stage 'crates\gpui\src\text_system\line.rs') -Raw -Encoding UTF8
    if ($patchedDirectWrite -notmatch 'glyphrun\.bidiLevel\s*%\s*2\s*==\s*1' -or
        $patchedDirectWrite -notmatch 'glyph_utf16_start_indices' -or
        $patchedDirectWrite -notmatch 'utf8_ix_for_utf16_ix' -or
        $patchedLineLayout -notmatch 'left_glyphs, right_glyphs' -or
        $patchedLine -notmatch 'test_split_at_descending_glyph_indices') {
        throw 'The upstream RTL patch verification failed. No project files were changed.'
    }

    $manifestText = Get-Content -LiteralPath $manifest -Raw -Encoding UTF8

    # A workspace dependency declaration is NOT a Cargo patch override.
    # Inspect only the [patch.crates-io] table; the previous installer scanned
    # the whole manifest and incorrectly rejected normal [dependencies] or
    # [workspace.dependencies] entries with the same package key.
    $patchTableMatch = [regex]::Match(
        $manifestText,
        '(?ms)^\[patch\.crates-io\][ \t]*\r?\n(?<body>.*?)(?=^\[[^\r\n]+\][ \t]*$|\z)'
    )
    if ($patchTableMatch.Success) {
        $patchBody = $patchTableMatch.Groups['body'].Value
        foreach ($packageKey in @('gpui-pre', 'gpui-pre-windows')) {
            $keyPattern = '(?m)^\s*"?' + [regex]::Escape($packageKey) + '"?\s*='
            if ([regex]::IsMatch($patchBody, $keyPattern)) {
                throw "The [patch.crates-io] table already contains '$packageKey'. The installer will not replace an existing Cargo patch. Keep the existing source and merge the RTL changes into that source manually. No project files were changed."
            }
        }
    }

    # Only now create vendor copies and overlay the validated upstream files.
    New-Item -ItemType Directory -Path $coreVendor -Force | Out-Null
    Get-ChildItem -LiteralPath $coreRegistry -Force | Copy-Item -Destination $coreVendor -Recurse -Force
    New-Item -ItemType Directory -Path $windowsVendor -Force | Out-Null
    Get-ChildItem -LiteralPath $windowsRegistry -Force | Copy-Item -Destination $windowsVendor -Recurse -Force

    Copy-Item -LiteralPath (Join-Path $stage 'crates\gpui\src\text_system\line.rs') -Destination (Join-Path $coreVendor 'src\text_system\line.rs') -Force
    Copy-Item -LiteralPath (Join-Path $stage 'crates\gpui\src\text_system\line_layout.rs') -Destination (Join-Path $coreVendor 'src\text_system\line_layout.rs') -Force
    Copy-Item -LiteralPath (Join-Path $stage 'crates\gpui_windows\src\direct_write.rs') -Destination (Join-Path $windowsVendor 'src\direct_write.rs') -Force

    $coreEntry = 'gpui-pre = { path = "vendor/gpui-pre" }'
    $windowsEntry = 'gpui-pre-windows = { path = "vendor/gpui-pre-windows" }'
    $entries = "$coreEntry`r`n$windowsEntry"
    $patchHeader = [regex]::Match($manifestText, '(?m)^\[patch\.crates-io\][ \t]*$')
    if ($patchHeader.Success) {
        $insertAt = $patchHeader.Index + $patchHeader.Length
        $manifestText = $manifestText.Insert($insertAt, "`r`n$entries")
    } else {
        $manifestText = $manifestText.TrimEnd() + "`r`n`r`n[patch.crates-io]`r`n$entries`r`n"
    }

    $backup = "$manifest.bibleown-rtl.bak"
    if (Test-Path -LiteralPath $backup) { throw "A backup file already exists: $backup" }
    Copy-Item -LiteralPath $manifest -Destination $backup
    [System.IO.File]::WriteAllText($manifest, $manifestText, ([System.Text.UTF8Encoding]::new($false)))

    Write-Host ''
    Write-Host 'Done: the two-commit GPUI RTL patch #67281 has been applied.' -ForegroundColor Green
    Write-Host 'Only DirectWrite RTL glyph runs and GPUI shaped-line splitting were changed.'
    Write-Host "Vendor GPUI: $coreVendor"
    Write-Host "Vendor Windows backend: $windowsVendor"
    Write-Host "Cargo.toml backup: $backup"
    Write-Host ''
    Write-Host 'Run these commands from the project root:'
    Write-Host '  cargo check'
    Write-Host '  cargo test -p gpui-pre-windows rtl_runs_are_laid_out_in_visual_order'
    Write-Host '  cargo test -p gpui-pre test_split_at_descending_glyph_indices'
    Write-Host ''
    Write-Host 'Then verify Hebrew, mixed Hebrew/English/numbers, Greek, and Russian sample text.'
}
finally {
    if (-not $KeepTemporaryFiles -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    } elseif ($KeepTemporaryFiles) {
        Write-Host "Temporary diffs and staging files were kept here: $stage" -ForegroundColor Yellow
    }
}
