@echo off
setlocal
rem Select the supported pre-Cargo ownership runtime without touching CARGO_TARGET_DIR.

if defined FLUX_PYTHON (
  "%FLUX_PYTHON%" -c "import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>nul
  if not errorlevel 1 (
    "%FLUX_PYTHON%" %*
    exit /b %errorlevel%
  )
  echo Flux build ownership requires Python 3.10+; set PYTHON to a supported Python 3 executable 1>&2
  exit /b 69
)

python -c "import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>nul
if not errorlevel 1 (
  python %*
  exit /b %errorlevel%
)

py -3 -c "import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>nul
if not errorlevel 1 (
  py -3 %*
  exit /b %errorlevel%
)

echo Flux build ownership requires Python 3.10+; install Python 3 or set PYTHON to its executable 1>&2
exit /b 69
