"""
Database migration management.

This module provides utilities for running and tracking database schema migrations.
"""

import hashlib
import logging
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import List, Optional

from .connection import get_pool


logger = logging.getLogger(__name__)


@dataclass
class Migration:
    """Represents a database migration."""
    version: str
    name: str
    up_sql: str
    down_sql: str
    checksum: str

    @classmethod
    def from_file(cls, filepath: Path) -> "Migration":
        """Load migration from a file."""
        content = filepath.read_text()

        # Parse up and down sections
        parts = content.split("-- DOWN")
        up_sql = parts[0].replace("-- UP", "").strip()
        down_sql = parts[1].strip() if len(parts) > 1 else ""

        checksum = hashlib.sha256(content.encode()).hexdigest()

        return cls(
            version=filepath.stem.split("_")[0],
            name=filepath.stem,
            up_sql=up_sql,
            down_sql=down_sql,
            checksum=checksum
        )


class MigrationRunner:
    """Manages database migrations."""

    def __init__(self, migrations_dir: Path):
        """Initialize the migration runner."""
        self.migrations_dir = migrations_dir

    def ensure_migrations_table(self) -> None:
        """Create migrations tracking table if it doesn't exist."""
        pool = get_pool()
        with pool.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute("""
                    CREATE TABLE IF NOT EXISTS schema_migrations (
                        version VARCHAR(255) PRIMARY KEY,
                        name VARCHAR(255) NOT NULL,
                        checksum VARCHAR(64) NOT NULL,
                        applied_at TIMESTAMP NOT NULL DEFAULT NOW()
                    )
                """)
                conn.commit()

    def get_applied_migrations(self) -> List[str]:
        """Get list of applied migration versions."""
        pool = get_pool()
        with pool.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute("SELECT version FROM schema_migrations ORDER BY version")
                return [row[0] for row in cur.fetchall()]

    def get_pending_migrations(self) -> List[Migration]:
        """Get list of pending migrations."""
        applied = set(self.get_applied_migrations())

        migrations = []
        for filepath in sorted(self.migrations_dir.glob("*.sql")):
            migration = Migration.from_file(filepath)
            if migration.version not in applied:
                migrations.append(migration)

        return migrations

    def apply_migration(self, migration: Migration) -> None:
        """Apply a single migration."""
        logger.info(f"Applying migration {migration.version}: {migration.name}")

        pool = get_pool()
        with pool.get_connection() as conn:
            with conn.cursor() as cur:
                # Execute migration SQL
                cur.execute(migration.up_sql)

                # Record migration
                cur.execute("""
                    INSERT INTO schema_migrations (version, name, checksum)
                    VALUES (%s, %s, %s)
                """, (migration.version, migration.name, migration.checksum))

                conn.commit()

    def rollback_migration(self, migration: Migration) -> None:
        """Rollback a single migration."""
        logger.info(f"Rolling back migration {migration.version}: {migration.name}")

        pool = get_pool()
        with pool.get_connection() as conn:
            with conn.cursor() as cur:
                # Execute rollback SQL
                cur.execute(migration.down_sql)

                # Remove migration record
                cur.execute("DELETE FROM schema_migrations WHERE version = %s", (migration.version,))

                conn.commit()
