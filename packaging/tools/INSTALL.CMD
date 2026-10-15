@echo off
setlocal
if not exist "%~dp0TOOLS.EXE" (
  echo LightVM Tools executable is missing from this medium.
  exit /b 1
)
"%~dp0TOOLS.EXE" --install
exit /b %ERRORLEVEL%
