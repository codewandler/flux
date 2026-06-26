"""A terminal-bench custom agent that runs the `flux` CLI inside the task container.

Used by flux-eval's TerminalBenchAdapter:

    tb run --agent-import-path flux_agent:FluxAgent \
           --model anthropic/claude-sonnet-4-6 \
           --agent-kwarg flux_binary=/abs/path/to/static/flux \
           --task-id <id> --n-attempts 1 --output-path <dir>

(with the directory of this file on PYTHONPATH). The static `flux` binary is copied into the
container; the agent then runs `flux --yes -m <model> -p <instruction>` and the task's own test grades
the result. All flux logic stays in the binary — this shim is just install + run-command glue, modeled
on terminal-bench's built-in `codex`/`claude_code` installed agents.
"""

import os
import shlex
from pathlib import Path

from terminal_bench.agents.installed_agents.abstract_installed_agent import (
    AbstractInstalledAgent,
)
from terminal_bench.terminal.models import TerminalCommand


class FluxAgent(AbstractInstalledAgent):
    @staticmethod
    def name() -> str:
        return "flux"

    def __init__(self, model_name: str, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._model_name = model_name
        # Host path to the static flux binary (passed via `--agent-kwarg flux_binary=...`,
        # or the FLUX_BINARY env var); defaults to `flux` on PATH inside the container.
        self._flux_binary = kwargs.get("flux_binary") or os.environ.get("FLUX_BINARY", "flux")

    @property
    def _env(self) -> dict[str, str]:
        # Forward whatever provider keys are present so `flux -m <model>` can authenticate.
        keys = ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY", "FLUX_SECRET")
        return {k: os.environ[k] for k in keys if k in os.environ}

    @property
    def _install_agent_script_path(self) -> Path:
        return Path(__file__).parent / "flux-setup.sh"

    def _run_agent_commands(self, instruction: str) -> list[TerminalCommand]:
        return [
            TerminalCommand(
                command=(
                    f"flux --yes -m {shlex.quote(self._model_name)} "
                    f"-p {shlex.quote(instruction)}"
                ),
                min_timeout_sec=0.0,
                max_timeout_sec=float("inf"),
                block=True,
                append_enter=True,
            )
        ]

    def perform_task(self, instruction, session, logging_dir=None):
        # Copy the static flux binary into the container before the standard install/run flow.
        # (AbstractInstalledAgent only copies the install *script*; the binary must be added too.)
        if self._flux_binary != "flux":
            session.copy_to_container(
                Path(self._flux_binary),
                container_dir="/usr/local/bin",
                container_filename="flux",
            )
            session.container.exec_run(["chmod", "+x", "/usr/local/bin/flux"])
        return super().perform_task(instruction, session, logging_dir)
