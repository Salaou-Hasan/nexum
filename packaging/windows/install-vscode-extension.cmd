@echo off
rem Install the Nexum extension into Visual Studio Code.
rem
rem Run by the installer after the files are in place. It is deliberately
rem quiet and never fails the installation: a developer who does not have
rem Visual Studio Code still gets the compiler, the icon and the file
rem association.
rem
rem The installer runs this with the machine PATH, which does not contain
rem the per-user program directories, so Visual Studio Code is looked for in
rem the usual places as well.

setlocal

set "VSIX=%~dp0nexum-language.vsix"
set "CODE=code"

where code >nul 2>&1
if errorlevel 1 (
  set "CODE="
  for %%D in (
    "%LOCALAPPDATA%\Programs\Microsoft VS Code\bin\code.cmd"
    "%ProgramFiles%\Microsoft VS Code\bin\code.cmd"
    "%ProgramFiles(x86)%\Microsoft VS Code\bin\code.cmd"
  ) do (
    if not defined CODE if exist %%D set "CODE=%%~D"
  )
)

if not defined CODE exit /b 0

"%CODE%" --install-extension "%VSIX%" --force >nul 2>&1
exit /b 0
