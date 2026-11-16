# Prints the next installer version: the newest FeatherPlayer_Setup_X.Y.Z.exe in installers\
# (and the Python app's %USERPROFILE%\FeatherPlayer\installer\, read only) with Z + 1.
# Nothing found: 1.0.0.
$root = Split-Path -Parent $PSScriptRoot
$dirs = @((Join-Path $root 'installers'), (Join-Path $env:USERPROFILE 'FeatherPlayer\installer'))
$versions = foreach ($d in $dirs) {
    if (Test-Path -LiteralPath $d) {
        Get-ChildItem -LiteralPath $d -Filter 'FeatherPlayer_Setup_*.exe' | ForEach-Object {
            if ($_.BaseName -match '^FeatherPlayer_Setup_(\d+)\.(\d+)\.(\d+)$') {
                [version]"$($matches[1]).$($matches[2]).$($matches[3])"
            }
        }
    }
}
$latest = $versions | Sort-Object -Descending | Select-Object -First 1
if ($latest) { "{0}.{1}.{2}" -f $latest.Major, $latest.Minor, ($latest.Build + 1) } else { '1.0.0' }
