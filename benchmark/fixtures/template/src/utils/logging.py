"""
Structured logging utilities.

This module provides structured logging with JSON output for production environments.
"""

import logging
import sys
from datetime import datetime
from typing import Any, Dict


class JSONFormatter(logging.Formatter):
    """Format log records as JSON."""

    def format(self, record: logging.LogRecord) -> str:
        """Format the log record as JSON."""
        log_data: Dict[str, Any] = {
            "timestamp": datetime.utcnow().isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "message": record.getMessage(),
        }

        if record.exc_info:
            log_data["exception"] = self.formatException(record.exc_info)

        return str(log_data)


def setup_logging(level: str = "INFO") -> None:
    """Configure logging for the application."""
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(JSONFormatter())

    logging.basicConfig(
        level=getattr(logging, level.upper()),
        handlers=[handler]
    )
