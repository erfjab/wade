# Installs or updates Wade on Windows:
#   irm https://raw.githubusercontent.com/erfjab/wade/master/install.ps1 | iex
# Only the program file is replaced. The "Start with Windows" choice is kept.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$dir = "$env:LOCALAPPDATA\Programs\Wade"
$exe = "$dir\wade.exe"
$tmp = Join-Path $env:TEMP "wade-install-$PID"
$update = Test-Path $exe

# Download and unpack first, so a failed download leaves the current install as it is.
Write-Host $(if ($update) { 'Updating Wade...' } else { 'Installing Wade...' })
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    Invoke-WebRequest 'https://github.com/erfjab/wade/releases/latest/download/wade-windows-x64.zip' -OutFile "$tmp\wade.zip"
    Expand-Archive "$tmp\wade.zip" "$tmp\new" -Force
    if (-not (Test-Path "$tmp\new\wade.exe")) { throw 'The download did not contain wade.exe.' }

    # Close the running copy and wait until the file is free. A copy set to
    # "Run as administrator" runs from its task, which can end it without rights.
    schtasks /End /TN Wade 2>$null | Out-Null
    Get-Process wade -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process wade -ErrorAction SilentlyContinue | Wait-Process -Timeout 10

    # Swap the program file, putting the old one back if anything goes wrong.
    New-Item -ItemType Directory -Force $dir | Out-Null
    if ($update) { Move-Item $exe "$exe.old" -Force }
    try {
        Copy-Item "$tmp\new\wade.exe" $exe
    } catch {
        if ($update) { Move-Item "$exe.old" $exe -Force }
        throw
    }
    Remove-Item "$exe.old" -ErrorAction SilentlyContinue
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Wade.lnk")
$lnk.TargetPath = $exe
$lnk.Save()

Start-Process $exe
if ($update) { Write-Host 'Wade updated.' }
else { Write-Host "Wade installed to $dir. Press Alt+Tab to use it." }
