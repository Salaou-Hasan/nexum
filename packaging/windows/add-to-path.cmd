@echo off
rem Add the install directory to the user's PATH, leaving the rest of it
rem alone. It runs as a deferred custom action, after the files are in
rem place, as the user - so no administrator rights are needed.

setlocal

set "DIR=%~dp0"
if "%DIR:~-1%"=="\" set "DIR=%DIR:~0,-1%"

for /f "tokens=2,*" %%A in ('reg query HKCU\Environment /v Path 2^>nul') do set "P=%%B"
if not defined P set "P="

echo %P% | find /i "%DIR%" >nul
if not errorlevel 1 exit /b 0

set "NEWPATH=%P%;%DIR%"
powershell -NoProfile -Command "[Environment]::SetEnvironmentVariable('PATH', $env:NEWPATH, 'User')" >nul 2>&1

exit /b 0
