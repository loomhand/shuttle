$ErrorActionPreference = 'Stop'
$Vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
$Dumpbin = (Get-ChildItem "$Vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Select-Object -First 1).FullName
if (-not $Dumpbin) { throw 'dumpbin.exe not found' }
$Bad = @()
foreach ($Exe in @($args | ForEach-Object { $_ })) {
    $Deps = & $Dumpbin /nologo /dependents $Exe
    if ($LASTEXITCODE -ne 0) { throw "dumpbin /dependents $Exe failed" }
    $Hits = $Deps | Select-String -Pattern '^\s+((vcruntime|msvcp|api-ms-win-crt)\S*\.dll)\s*$' | ForEach-Object { $_.Matches[0].Groups[1].Value }
    if ($Hits) { $Bad += "$Exe imports $($Hits -join ', ')" } else { Write-Host "${Exe}: no Visual C++ runtime imports" }
}
if ($Bad) { $Bad | ForEach-Object { Write-Host $_ }; exit 1 }
