"""
Tests for authentication functionality.
"""

import pytest
from src.auth.tokens import (
    create_access_token,
    validate_jwt_token,
    refresh_token,
    TokenExpiredError,
    TokenInvalidError,
)


def test_create_access_token():
    """Test creating an access token."""
    token = create_access_token("user_123")
    assert token is not None
    assert isinstance(token, str)


def test_validate_jwt_token():
    """Test validating a JWT token."""
    token = create_access_token("user_123")
    payload = validate_jwt_token(token)

    assert payload["sub"] == "user_123"
    assert payload["type"] == "access"


def test_validate_invalid_token():
    """Test validating an invalid token."""
    with pytest.raises(TokenInvalidError):
        validate_jwt_token("invalid.token.here")


def test_refresh_token():
    """Test creating a refresh token."""
    token = refresh_token("user_123")
    payload = validate_jwt_token(token)

    assert payload["sub"] == "user_123"
    assert payload["type"] == "refresh"
