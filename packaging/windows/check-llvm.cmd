@echo off
rem Check that the LLVM toolchain is installed.
rem
rem Nexum hands the generated LLVM IR to clang to build a program, so clang
rem has to be there. When it is missing, tell the developer how to install
rem it - the installer cannot do it itself, because the LLVM toolchain is a
rem system-wide package.
rem
rem The custom action data carries the MSI user interface level: a quiet
rem install (msiexec /quiet, which is what `nx update` runs) stays quiet.

if not "%CustomActionData%"=="2" goto interactive

rem A quiet install: record the state and say nothing.
where clang >nul 2>&1
if errorlevel 1 (
  echo clang is missing > "%~dp0llvm-required.txt"
)
exit /b 0

:interactive
where clang >nul 2>&1
if not errorlevel 1 exit /b 0

rem Look for clang where it is usually installed, not only on the PATH.
set "CLANG="
for %%D in (
  "%ProgramFiles%\LLVM\bin\clang.exe"
  "%ProgramFiles(x86)%\LLVM\bin\clang.exe"
  "%LOCALAPPDATA%\Programs\LLVM\bin\clang.exe"
) do (
  if not defined CLANG if exist %%D set "CLANG=%%~D"
)
if defined CLANG exit /b 0

echo clang is missing > "%~dp0llvm-required.txt"
powershell -NoProfile -Command "Add-Type -AssemblyName System.Windows.Forms; [void][System.Windows.Forms.MessageBox]::Show('Nexum needs the LLVM toolchain (clang) to build programs.' + [Environment]::NewLine + [Environment]::NewLine + 'Install it with:' + [Environment]::NewLine + '    winget install LLVM.LLVM' + [Environment]::NewLine + 'or download it from https://llvm.org', 'Nexum setup')" >nul 2>&1
exit /b 0
