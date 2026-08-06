"""
Database connection pooling and management.

This module provides a connection pool implementation for managing
database connections efficiently across the application.
"""

import logging
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from queue import Queue, Empty, Full
from typing import Any, Iterator, List

import psycopg2
from psycopg2 import pool
from psycopg2.extensions import connection as PgConnection


logger = logging.getLogger(__name__)


@dataclass
class ConnectionConfig:
    """Configuration for database connections."""
    host: str
    port: int
    database: str
    user: str
    password: str
    min_connections: int = 2
    max_connections: int = 10
    connection_timeout: int = 30


class ConnectionPool:
    """
    Thread-safe connection pool for PostgreSQL.

    Manages a pool of database connections that can be reused across
    multiple requests, improving performance and resource utilization.
    """

    def __init__(self, config: ConnectionConfig):
        """
        Initialize the connection pool.

        Args:
            config: Database connection configuration
        """
        self.config = config
        self._pool = None
        self._lock = threading.RLock()
        self._initialized = False
        self._connection_count = 0
        self._stats = {
            "total_connections": 0,
            "active_connections": 0,
            "failed_connections": 0,
            "pool_exhausted_count": 0,
        }

    def initialize(self) -> None:
        """Initialize the connection pool."""
        with self._lock:
            if self._initialized:
                return

            try:
                self._pool = psycopg2.pool.ThreadedConnectionPool(
                    self.config.min_connections,
                    self.config.max_connections,
                    host=self.config.host,
                    port=self.config.port,
                    database=self.config.database,
                    user=self.config.user,
                    password=self.config.password,
                    connect_timeout=self.config.connection_timeout,
                )
                self._initialized = True
                logger.info(
                    f"Connection pool initialized with "
                    f"{self.config.min_connections}-{self.config.max_connections} connections"
                )
            except Exception as e:
                logger.error(f"Failed to initialize connection pool: {e}")
                raise

    @contextmanager
    def get_connection(self) -> Iterator[PgConnection]:
        """
        Get a connection from the pool.

        Yields:
            A database connection

        Raises:
            RuntimeError: If pool is not initialized
            TimeoutError: If no connection available within timeout
        """
        if not self._initialized:
            raise RuntimeError("Connection pool not initialized")

        conn = None
        try:
            conn = self._pool.getconn()
            if conn is None:
                self._stats["pool_exhausted_count"] += 1
                raise TimeoutError("Failed to get connection from pool")

            self._stats["active_connections"] += 1
            yield conn

        except Exception as e:
            if conn:
                conn.rollback()
            self._stats["failed_connections"] += 1
            raise
        finally:
            if conn:
                self._pool.putconn(conn)
                self._stats["active_connections"] -= 1

    def close_all(self) -> None:
        """Close all connections in the pool."""
        with self._lock:
            if self._pool:
                self._pool.closeall()
                logger.info("All connections closed")
            self._initialized = False

    def get_stats(self) -> dict:
        """Get connection pool statistics."""
        return self._stats.copy()

    def health_check(self) -> bool:
        """
        Perform a health check on the connection pool.

        Returns:
            True if pool is healthy, False otherwise
        """
        try:
            with self.get_connection() as conn:
                with conn.cursor() as cur:
                    cur.execute("SELECT 1")
                    return cur.fetchone()[0] == 1
        except Exception as e:
            logger.error(f"Health check failed: {e}")
            return False


# Global connection pool instance
_pool_instance = None
_pool_lock = threading.Lock()


def initialize_pool(config: ConnectionConfig) -> None:
    """
    Initialize the global connection pool.

    Args:
        config: Database connection configuration
    """
    global _pool_instance

    with _pool_lock:
        if _pool_instance is None:
            _pool_instance = ConnectionPool(config)
            _pool_instance.initialize()


def get_pool() -> ConnectionPool:
    """
    Get the global connection pool instance.

    Returns:
        The global ConnectionPool instance

    Raises:
        RuntimeError: If pool has not been initialized
    """
    if _pool_instance is None:
        raise RuntimeError("Connection pool not initialized. Call initialize_pool() first.")
    return _pool_instance


def close_pool() -> None:
    """Close the global connection pool."""
    global _pool_instance

    with _pool_lock:
        if _pool_instance:
            _pool_instance.close_all()
            _pool_instance = None
