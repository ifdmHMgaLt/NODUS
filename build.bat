@echo off
setlocal EnableExtensions
cd /d "%~dp0"
title NODUS build

set "RUN_TESTS="
set "PAUSE_AT_END=1"
for %%A in (%*) do (
    if /i "%%~A"=="test" set "RUN_TESTS=1"
    if /i "%%~A"=="nopause" set "PAUSE_AT_END=0"
)

rem Build in a temp folder: the project never gets a target\ directory.
set "BUILD_DIR=%TEMP%\nodus-build"
set "CARGO_TARGET_DIR=%BUILD_DIR%"
set "OUT=dist"

echo === NODUS build ===
echo.

where cargo >nul 2>nul
if errorlevel 1 goto no_rust
where rustc >nul 2>nul
if errorlevel 1 goto no_rust

if exist "%BUILD_DIR%" rd /s /q "%BUILD_DIR%"

echo [1/3] Building release binaries...
cargo build --release --workspace
if errorlevel 1 goto build_failed

echo.
echo [2/3] Collecting exe, dll and pdb into %OUT%\ ...
if exist "%OUT%" rd /s /q "%OUT%"
mkdir "%OUT%"
copy /y "%BUILD_DIR%\release\nodus.exe" "%OUT%\" >nul
if errorlevel 1 goto copy_failed
copy /y "%BUILD_DIR%\release\nodus_hook.dll" "%OUT%\" >nul
if errorlevel 1 goto copy_failed
copy /y "%BUILD_DIR%\release\*.pdb" "%OUT%\" >nul 2>nul

echo.
if defined RUN_TESTS (
    echo [3/3] Running tests...
    cargo build --workspace
    if errorlevel 1 goto test_failed
    cargo test --workspace
    if errorlevel 1 goto test_failed
) else (
    echo [3/3] Tests skipped. Run "build.bat test" to build and test.
)

call :cleanup
echo.
echo Done. Ready to run:
echo   %OUT%\nodus.exe
echo   %OUT%\nodus_hook.dll
echo Keep both files together: the bot loads the dll from next to itself.
goto success

:no_rust
echo.
echo Rust is not installed or not in PATH.
echo Install it from https://rustup.rs (default options are fine),
echo then close this window and run build.bat again.
goto fail

:build_failed
echo.
echo Build failed.
echo If cargo says it cannot find "link.exe", install Visual Studio Build Tools
echo with the "Desktop development with C++" workload and try again.
goto fail

:copy_failed
echo.
echo Build finished, but copying into %OUT%\ failed.
echo Make sure %OUT%\ is writable and the build produced the binaries.
goto fail

:test_failed
echo.
echo Tests failed. The binaries in %OUT%\ were built anyway.
goto fail

:cleanup
if exist "%BUILD_DIR%" rd /s /q "%BUILD_DIR%"
exit /b 0

:success
if "%PAUSE_AT_END%"=="1" pause
exit /b 0

:fail
if exist "%BUILD_DIR%" rd /s /q "%BUILD_DIR%"
if "%PAUSE_AT_END%"=="1" pause
exit /b 1
