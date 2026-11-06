"""InstruktAI logging standard library.

See `README.md` for usage and `docs/design.md` for design intent.
"""

from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as _pkg_version

from instrukt_ai_logging.logging import (
    TRACE,
    InstruktAILogger,
    InstruktAILoggerProtocol,
    configure_logging,
    failure_count,
    get_logger,
    resolve_log_file,
    resolve_log_files,
)

__all__ = [
    "TRACE",
    "InstruktAILogger",
    "InstruktAILoggerProtocol",
    "__version__",
    "configure_logging",
    "failure_count",
    "get_logger",
    "resolve_log_file",
    "resolve_log_files",
]

_DISTRIBUTION_NAME = "instruktai-python-logger"

try:
    __version__ = _pkg_version(_DISTRIBUTION_NAME)
except PackageNotFoundError:
    __version__ = "0.0.0"
