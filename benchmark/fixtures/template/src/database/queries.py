"""
Database query builder and utilities.

This module provides a fluent query builder interface for constructing
and executing database queries safely with parameterization.
"""

import logging
from typing import Any, Dict, List, Optional, Union

from .connection import get_pool


logger = logging.getLogger(__name__)


class QueryBuilder:
    """
    Fluent interface for building SQL queries.

    Provides a safe way to construct queries with automatic parameterization
    to prevent SQL injection vulnerabilities.
    """

    def __init__(self):
        """Initialize a new query builder."""
        self._table: Optional[str] = None
        self._operation: Optional[str] = None
        self._select_fields: List[str] = ["*"]
        self._where_conditions: List[tuple] = []
        self._order_by_fields: List[tuple] = []
        self._limit_value: Optional[int] = None
        self._offset_value: Optional[int] = None
        self._update_data: Dict[str, Any] = {}
        self._insert_data: Dict[str, Any] = {}

    def select(self, table: str, *fields: str) -> "QueryBuilder":
        """
        Start a SELECT query.

        Args:
            table: The table to select from
            fields: Optional field names (defaults to all fields)
        """
        self._operation = "SELECT"
        self._table = table
        if fields:
            self._select_fields = list(fields)
        return self

    def insert(self, table: str, data: Dict[str, Any]) -> "QueryBuilder":
        """
        Start an INSERT query.

        Args:
            table: The table to insert into
            data: Dictionary of field names to values
        """
        self._operation = "INSERT"
        self._table = table
        self._insert_data = data
        return self

    def update(self, table: str, data: Dict[str, Any]) -> "QueryBuilder":
        """
        Start an UPDATE query.

        Args:
            table: The table to update
            data: Dictionary of field names to new values
        """
        self._operation = "UPDATE"
        self._table = table
        self._update_data = data
        return self

    def delete(self, table: str) -> "QueryBuilder":
        """
        Start a DELETE query.

        Args:
            table: The table to delete from
        """
        self._operation = "DELETE"
        self._table = table
        return self

    def where(self, field: str, value: Any, operator: str = "=") -> "QueryBuilder":
        """
        Add a WHERE condition.

        Args:
            field: The field name
            value: The value to compare against
            operator: The comparison operator (=, !=, <, >, <=, >=, LIKE, IN)
        """
        self._where_conditions.append((field, value, operator))
        return self

    def where_in(self, field: str, values: List[Any]) -> "QueryBuilder":
        """
        Add a WHERE IN condition.

        Args:
            field: The field name
            values: List of values to match
        """
        self._where_conditions.append((field, values, "IN"))
        return self

    def order_by(self, field: str, direction: str = "ASC") -> "QueryBuilder":
        """
        Add an ORDER BY clause.

        Args:
            field: The field to order by
            direction: ASC or DESC
        """
        self._order_by_fields.append((field, direction.upper()))
        return self

    def limit(self, count: int) -> "QueryBuilder":
        """
        Add a LIMIT clause.

        Args:
            count: Maximum number of rows to return
        """
        self._limit_value = count
        return self

    def offset(self, count: int) -> "QueryBuilder":
        """
        Add an OFFSET clause.

        Args:
            count: Number of rows to skip
        """
        self._offset_value = count
        return self

    def paginate(self, page: int, per_page: int) -> List[Dict[str, Any]]:
        """
        Paginate results.

        Args:
            page: Page number (1-indexed)
            per_page: Number of items per page
        """
        offset = (page - 1) * per_page
        return self.limit(per_page).offset(offset).all()

    def build(self) -> tuple[str, List[Any]]:
        """
        Build the SQL query and parameters.

        Returns:
            A tuple of (query_string, parameters)
        """
        params = []

        if self._operation == "SELECT":
            query = f"SELECT {', '.join(self._select_fields)} FROM {self._table}"

        elif self._operation == "INSERT":
            fields = list(self._insert_data.keys())
            placeholders = ", ".join(["%s"] * len(fields))
            query = f"INSERT INTO {self._table} ({', '.join(fields)}) VALUES ({placeholders})"
            params.extend(self._insert_data.values())

        elif self._operation == "UPDATE":
            set_clause = ", ".join([f"{k} = %s" for k in self._update_data.keys()])
            query = f"UPDATE {self._table} SET {set_clause}"
            params.extend(self._update_data.values())

        elif self._operation == "DELETE":
            query = f"DELETE FROM {self._table}"

        else:
            raise ValueError(f"Unknown operation: {self._operation}")

        # Add WHERE clause
        if self._where_conditions:
            where_parts = []
            for field, value, operator in self._where_conditions:
                if operator == "IN":
                    placeholders = ", ".join(["%s"] * len(value))
                    where_parts.append(f"{field} IN ({placeholders})")
                    params.extend(value)
                else:
                    where_parts.append(f"{field} {operator} %s")
                    params.append(value)
            query += " WHERE " + " AND ".join(where_parts)

        # Add ORDER BY clause
        if self._order_by_fields:
            order_parts = [f"{field} {direction}" for field, direction in self._order_by_fields]
            query += " ORDER BY " + ", ".join(order_parts)

        # Add LIMIT and OFFSET
        if self._limit_value is not None:
            query += f" LIMIT {self._limit_value}"
        if self._offset_value is not None:
            query += f" OFFSET {self._offset_value}"

        return query, params

    def execute(self) -> Any:
        """Execute the query and return the result."""
        query, params = self.build()
        logger.debug(f"Executing query: {query} with params: {params}")

        pool = get_pool()
        with pool.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute(query, params)

                if self._operation == "SELECT":
                    return cur.fetchall()
                elif self._operation == "INSERT":
                    conn.commit()
                    return cur.lastrowid
                else:
                    conn.commit()
                    return cur.rowcount

    def first(self) -> Optional[Dict[str, Any]]:
        """Execute and return the first result."""
        results = self.limit(1).all()
        return results[0] if results else None

    def all(self) -> List[Dict[str, Any]]:
        """Execute and return all results."""
        return self.execute()
