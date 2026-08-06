"""
Tests for database functionality.
"""

import pytest
from src.database.queries import QueryBuilder


def test_query_builder_select():
    """Test SELECT query building."""
    qb = QueryBuilder()
    query, params = qb.select("users", "id", "email").where("id", "123").build()

    assert "SELECT id, email FROM users" in query
    assert "WHERE id = %s" in query
    assert params == ["123"]


def test_query_builder_insert():
    """Test INSERT query building."""
    qb = QueryBuilder()
    data = {"name": "John", "email": "john@example.com"}
    query, params = qb.insert("users", data).build()

    assert "INSERT INTO users" in query
    assert "VALUES" in query
    assert params == ["John", "john@example.com"]
