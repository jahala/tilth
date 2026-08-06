"""
Configuration loading from environment variables.

This module provides utilities for loading and validating configuration.
"""

import os
from dataclasses import dataclass
from typing import Optional


@dataclass
class Config:
    """Application configuration."""
    database_url: str
    secret_key: str
    redis_url: Optional[str] = None
    log_level: str = "INFO"
    debug: bool = False

    @classmethod
    def from_env(cls) -> "Config":
        """Load configuration from environment variables."""
        return cls(
            database_url=os.getenv("DATABASE_URL", "postgresql://localhost/dev"),
            secret_key=os.getenv("SECRET_KEY", "dev-secret-key"),
            redis_url=os.getenv("REDIS_URL"),
            log_level=os.getenv("LOG_LEVEL", "INFO"),
            debug=os.getenv("DEBUG", "false").lower() == "true",
        )
