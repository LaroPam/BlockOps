$ErrorActionPreference = 'Stop'
$installer = Get-ChildItem 'apps/desktop/src-tauri/target/release/bundle/nsis/*-setup.exe' | Select-Object -First 1
if (-not $installer) { throw 'NSIS installer was not produced.' }
$installDir = Join-Path $env:RUNNER_TEMP 'BlockOpsInstallerSmoke'
if (Test-Path $installDir) { throw 'Smoke install destination already exists.' }
$install = Start-Process -FilePath $installer.FullName -ArgumentList '/S', "/D=$installDir" -PassThru -Wait
if ($install.ExitCode -ne 0) { throw "Silent installation failed with exit code $($install.ExitCode)." }
$exe = Get-ChildItem -Path $installDir -Filter '*.exe' -File | Where-Object { $_.Name -ne 'uninstall.exe' } | Select-Object -First 1
if (-not $exe) { throw 'Application executable missing after installation.' }
$bundledMod = Get-ChildItem -Path (Join-Path $installDir 'mod') -Filter 'blockops-fabric-client-*.jar' -File -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $bundledMod) { throw 'Bundled BlockOps mod missing after installation.' }
Write-Host "Installed application carries the mod $($bundledMod.Name)."
# No server is created and no EULA is accepted in this test. Verify the installed
# binary opens a native window and handles a normal close request.
$app = Start-Process -FilePath $exe.FullName -WorkingDirectory $installDir -PassThru
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        $app.Refresh()
        if ($app.HasExited) { throw "Application exited before opening a window (exit code $($app.ExitCode))." }
        if ($app.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($app.MainWindowHandle -eq [IntPtr]::Zero) { throw 'Application did not open a window within 45 seconds.' }
    Write-Host 'Installed application opened a native window.'
    if (-not $app.CloseMainWindow()) { throw 'Could not send a normal window close request.' }
    if (-not $app.WaitForExit(30000)) { throw 'Application did not close within 30 seconds.' }
    if ($app.ExitCode -ne 0) { throw "Application closed with exit code $($app.ExitCode)." }
    Write-Host 'Application closed successfully.'
} finally {
    $app.Refresh()
    if (-not $app.HasExited) {
        Stop-Process -Id $app.Id -Force
        $app.WaitForExit()
    }
}
$uninstaller = Join-Path $installDir 'uninstall.exe'
if (-not (Test-Path $uninstaller)) { throw 'Uninstaller missing after installation.' }
$uninstall = Start-Process -FilePath $uninstaller -ArgumentList '/S' -PassThru -Wait
if ($uninstall.ExitCode -ne 0) { throw "Silent uninstallation failed with exit code $($uninstall.ExitCode)." }
if (Test-Path $exe.FullName) { throw 'Application executable remains after uninstallation.' }
Write-Host 'NSIS installation, native window startup, close and uninstallation completed.'
