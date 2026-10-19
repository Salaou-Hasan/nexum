@echo off
rem Install the Nexum extension into Visual Studio Code.
rem
rem Run by the installer after the files are in place. It is deliberately
rem quiet and never fails the installation: a developer who does not have
rem the `code` command on their PATH (or who uses a different editor) still
rem gets the compiler, the icon and the file association.

setlocal

where code >nul 2>&1
if errorlevel 1 (
  rem No `code` command: nothing to do.
  exit /b 0
)

code --install-extension "%~dp0nexum-language.vsix" --force >nul 2>&1
exit /b 0
