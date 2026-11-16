# Prints the path of Inno Setup's ISCC.exe (compiler), or nothing if it isn't installed.
$cands = @()
$keys = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
Get-ItemProperty $keys -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -like 'Inno Setup*' -and $_.InstallLocation } |
    Sort-Object DisplayName -Descending |
    ForEach-Object { $cands += Join-Path $_.InstallLocation 'ISCC.exe' }
$cands += "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe", "$env:ProgramFiles\Inno Setup 7\ISCC.exe",
          "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
          "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
$found = $cands | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if ($found) { $found }
