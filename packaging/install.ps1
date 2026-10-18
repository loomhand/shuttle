# Installs cocoon-shuttle on Windows from a GitHub release, after checking the archive against the release's SHA256SUMS.
# usage: irm https://github.com/loomhand/shuttle/releases/latest/download/install.ps1 | iex
# env: SHUTTLE_VERSION (a tag, default the latest release), SHUTTLE_HOME (default %LOCALAPPDATA%\Programs\cocoon-shuttle)
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $Name = 'cocoon-shuttle'
    $Repo = 'loomhand/shuttle'
    $InstallDir = $(if ($env:SHUTTLE_HOME) { $env:SHUTTLE_HOME } else { Join-Path $env:LOCALAPPDATA "Programs\$Name" }).TrimEnd('\', '/')

    $Version = $env:SHUTTLE_VERSION
    if (-not $Version) {
        $Version = (Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$Repo/releases/latest").tag_name
    }
    $Asset = "$Name-$Version-windows-x64.zip"
    $Base = "https://github.com/$Repo/releases/download/$Version"

    $Tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
    New-Item -ItemType Directory -Path $Tmp | Out-Null
    try {
        Write-Host "downloading $Asset"
        Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile (Join-Path $Tmp $Asset)
        Invoke-WebRequest -UseBasicParsing "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp 'SHA256SUMS')
        $Line = Get-Content -LiteralPath (Join-Path $Tmp 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -eq $Asset }
        if (-not $Line) { throw "SHA256SUMS lists no $Asset" }
        $Want = ($Line -split '\s+')[0]
        $Got = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Tmp $Asset)).Hash.ToLower()
        if ($Got -ne $Want) { throw "sha256 mismatch for $Asset" }
        Write-Host 'sha256 ok'

        $Old = "$InstallDir.old"
        $New = "$InstallDir.new"
        $Running = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and ($_.ExecutablePath.StartsWith("$InstallDir\", 'OrdinalIgnoreCase') -or $_.ExecutablePath.StartsWith("$Old\", 'OrdinalIgnoreCase')) }
        if ($Running) { throw "close the running $Name first ($(($Running.Name | Sort-Object -Unique) -join ', ')); nothing was changed" }
        New-Item -ItemType Directory -Force -Path (Split-Path $InstallDir) | Out-Null
        if ((Test-Path -LiteralPath $Old) -and -not (Test-Path -LiteralPath $InstallDir)) { Move-Item -LiteralPath $Old -Destination $InstallDir }
        if (Test-Path -LiteralPath $New) { Remove-Item -Recurse -Force -LiteralPath $New }
        Expand-Archive -LiteralPath (Join-Path $Tmp $Asset) -DestinationPath $New
        if (Test-Path -LiteralPath $Old) { Remove-Item -Recurse -Force -LiteralPath $Old }
        if (Test-Path -LiteralPath $InstallDir) {
            try { Move-Item -LiteralPath $InstallDir -Destination $Old } catch { Remove-Item -Recurse -Force -LiteralPath $New; throw "close the running $Name first; nothing was changed" }
        }
        try { Move-Item -LiteralPath (Join-Path $New $Name) -Destination $InstallDir } catch { if (Test-Path -LiteralPath $Old) { Move-Item -LiteralPath $Old -Destination $InstallDir }; throw }
        Remove-Item -Recurse -Force -LiteralPath $New
        $Installed = try { & (Join-Path $InstallDir "$Name.exe") --version } catch { $null }
        if ($LASTEXITCODE -ne 0 -or -not $Installed) {
            $Code = $LASTEXITCODE
            Remove-Item -Recurse -Force -LiteralPath $InstallDir
            if (Test-Path -LiteralPath $Old) { Move-Item -LiteralPath $Old -Destination $InstallDir }
            throw "the new $Name does not start (--version exit code $Code); the previous install, if any, is back in place"
        }
        if (Test-Path -LiteralPath $Old) { Remove-Item -Recurse -Force -LiteralPath $Old }
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $Tmp
    }

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($UserPath -split ';') -contains $InstallDir)) {
        [Environment]::SetEnvironmentVariable('Path', (@($UserPath, $InstallDir) | Where-Object { $_ }) -join ';', 'User')
        Write-Host "added $InstallDir to the user PATH; open a new terminal to use it"
    }
    $env:Path = "$env:Path;$InstallDir"

    Write-Host "installed $Installed in $InstallDir"
    Write-Host "register it with an agent, e.g.: claude mcp add shuttle -- `"$(Join-Path $InstallDir "$Name.exe")`" mcp"
    Write-Host 'The binaries are unsigned: SmartScreen may warn about a browser-downloaded copy (More info > Run anyway).'
}
