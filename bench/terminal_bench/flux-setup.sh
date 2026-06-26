#!/usr/bin/env bash
# Install step for the flux terminal-bench agent. The static `flux` binary is copied into the
# container by FluxAgent.perform_task before this runs, so here we only verify it's executable.
# The base AbstractInstalledAgent treats the string INSTALL_FAIL_STATUS in the pane as a failure.
flux --version || echo INSTALL_FAIL_STATUS
