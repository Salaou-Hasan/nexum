@echo off
rem Check that the LLVM toolchain is installed.
rem
rem Nexum hands the generated LLVM IR to clang to build a program, so clang
rem has to be there. The installer cannot install a system-wide toolchain
rem itself, so it records what is missing and the setup program asks about
rem it when it is run.
rem
rem The custom action data carries the MSI user interface level: a quiet
rem install (msiexec /quiet, which is what `nx update` runs) stays quiet.

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

> "%~dp0llvm-required.txt" echo The LLVM toolchain (clang) is not installed.
>> "%~dp0llvm-required.txt" echo Nexum needs it to build programs. Run:
>> "%~dp0llvm-required.txt" echo   nx setup
>> "%~dp0llvm-required.txt" echo or install it with:
>> "%~dp0llvm-required.txt" echo   winget install LLVM.LLVM
exit /b 0
