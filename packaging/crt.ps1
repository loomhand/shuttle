$ErrorActionPreference = 'Stop'
$Files = @($args | ForEach-Object { $_ })
if (-not $Files) { throw 'usage: crt.ps1 EXE...' }
$Vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
$Dumpbin = (Get-ChildItem "$Vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Select-Object -First 1).FullName
if (-not $Dumpbin) { throw 'dumpbin.exe not found' }
$Bad = @()
foreach ($Exe in $Files) {
    $Deps = & $Dumpbin /nologo /dependents $Exe
    if ($LASTEXITCODE -ne 0) { throw "dumpbin /dependents $Exe failed" }
    if (-not ($Deps -match 'Image has the following dependencies') -or -not ($Deps -match '^\s+\S+\.dll\s*$')) { throw "dumpbin /dependents $Exe listed no dependencies" }
    $Hits = $Deps | Select-String -Pattern '^\s+((vcruntime|msvcp|api-ms-win-crt)\S*\.dll)\s*$' | ForEach-Object { $_.Matches[0].Groups[1].Value }
    if ($Hits) { $Bad += "$Exe imports $($Hits -join ', ')" } else { Write-Host "${Exe}: no Visual C++ runtime imports" }
}
if ($Bad) { $Bad | ForEach-Object { Write-Host $_ }; exit 1 }
