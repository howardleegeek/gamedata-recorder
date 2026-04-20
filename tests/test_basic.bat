@echo off
REM Simple wrapper to run the PowerShell test
REM Usage: test_basic.bat [game_exe] [duration_seconds]

set GAME_EXE=%1
set DURATION=%2

if "%GAME_EXE%"=="" set GAME_EXE=notepad.exe
if "%DURATION%"=="" set DURATION=10

echo Running test with %GAME_EXE% for %DURATION% seconds...
powershell -ExecutionPolicy Bypass -File "%~dp0test_basic_recording.ps1" -GameExe "%GAME_EXE%" -RecordingDuration %DURATION%

pause
