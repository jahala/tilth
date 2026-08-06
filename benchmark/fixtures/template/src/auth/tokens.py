"""
JWT token generation and validation utilities.

This module provides functions for creating and validating JWT tokens
used for authentication and authorization in the API.
"""

import hashlib
import hmac
import secrets
from datetime import datetime, timedelta
from typing import Dict, Optional, Any

import jwt
from jwt.exceptions import (
    ExpiredSignatureError,
    InvalidTokenError,
    DecodeError,
)


SECRET_KEY = "your-secret-key-here"
ALGORITHM = "HS256"
ACCESS_TOKEN_EXPIRE_MINUTES = 30
REFRESH_TOKEN_EXPIRE_DAYS = 7


class TokenError(Exception):
    """Base exception for token-related errors."""
    pass


class TokenExpiredError(TokenError):
    """Raised when a token has expired."""
    pass


class TokenInvalidError(TokenError):
    """Raised when a token is invalid or malformed."""
    pass


def validate_jwt_token(token: str) -> Dict[str, Any]:
    """
    Validate a JWT token and return its payload.

    Args:
        token: The JWT token string to validate

    Returns:
        The decoded token payload as a dictionary

    Raises:
        TokenExpiredError: If the token has expired
        TokenInvalidError: If the token is invalid or malformed
    """
    try:
        payload = jwt.decode(
            token,
            SECRET_KEY,
            algorithms=[ALGORITHM]
        )

        # Additional validation checks
        if "exp" not in payload:
            raise TokenInvalidError("Token missing expiration claim")

        if "sub" not in payload:
            raise TokenInvalidError("Token missing subject claim")

        return payload

    except ExpiredSignatureError:
        raise TokenExpiredError("Token has expired")
    except (InvalidTokenError, DecodeError) as e:
        raise TokenInvalidError(f"Invalid token: {str(e)}")


def create_access_token(
    subject: str,
    additional_claims: Optional[Dict[str, Any]] = None
) -> str:
    """
    Create a new JWT access token.

    Args:
        subject: The subject identifier (usually user ID)
        additional_claims: Optional additional claims to include

    Returns:
        The encoded JWT token string
    """
    now = datetime.utcnow()
    expire = now + timedelta(minutes=ACCESS_TOKEN_EXPIRE_MINUTES)

    payload = {
        "sub": subject,
        "iat": now,
        "exp": expire,
        "type": "access",
    }

    if additional_claims:
        payload.update(additional_claims)

    token = jwt.encode(payload, SECRET_KEY, algorithm=ALGORITHM)
    return token


def refresh_token(
    subject: str,
    additional_claims: Optional[Dict[str, Any]] = None
) -> str:
    """
    Create a new JWT refresh token.

    Args:
        subject: The subject identifier (usually user ID)
        additional_claims: Optional additional claims to include

    Returns:
        The encoded JWT refresh token string
    """
    now = datetime.utcnow()
    expire = now + timedelta(days=REFRESH_TOKEN_EXPIRE_DAYS)

    payload = {
        "sub": subject,
        "iat": now,
        "exp": expire,
        "type": "refresh",
    }

    if additional_claims:
        payload.update(additional_claims)

    token = jwt.encode(payload, SECRET_KEY, algorithm=ALGORITHM)
    return token


def verify_refresh_token(token: str) -> Dict[str, Any]:
    """
    Verify a refresh token and return its payload.

    Args:
        token: The refresh token to verify

    Returns:
        The decoded token payload

    Raises:
        TokenInvalidError: If token is not a refresh token
    """
    payload = validate_jwt_token(token)

    if payload.get("type") != "refresh":
        raise TokenInvalidError("Not a refresh token")

    return payload
