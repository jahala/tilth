"""
Authentication and authorization middleware.

This module provides Flask middleware for authentication, permission checking,
and request rate limiting.
"""

import functools
import logging
from collections import defaultdict
from datetime import datetime, timedelta
from typing import Callable, Dict, List, Optional, Set

from flask import request, g, jsonify
from werkzeug.exceptions import Forbidden, Unauthorized

from .tokens import validate_jwt_token, TokenExpiredError, TokenInvalidError


logger = logging.getLogger(__name__)


class AuthMiddleware:
    """Middleware for handling authentication and authorization."""

    def __init__(self, app=None):
        """Initialize the middleware."""
        self.app = app
        self._permission_cache: Dict[str, Set[str]] = {}
        self._cache_timeout = timedelta(minutes=5)
        self._last_cache_clear = datetime.utcnow()

        if app is not None:
            self.init_app(app)

    def init_app(self, app):
        """Initialize the middleware with a Flask app."""
        app.before_request(self.authenticate_request)
        app.after_request(self.add_security_headers)

    def authenticate_request(self):
        """Authenticate incoming requests."""
        # Skip auth for public endpoints
        if request.endpoint in ["api.health_check", "api.login"]:
            return

        # Extract token from Authorization header
        auth_header = request.headers.get("Authorization")
        if not auth_header:
            logger.warning(f"Missing auth header for {request.path}")
            return

        if not auth_header.startswith("Bearer "):
            logger.warning(f"Invalid auth header format for {request.path}")
            return

        token = auth_header.split(" ")[1]

        try:
            payload = validate_jwt_token(token)
            g.user_id = payload["sub"]
            g.token_payload = payload
            g.is_authenticated = True

            logger.debug(f"Authenticated user {g.user_id} for {request.path}")

        except TokenExpiredError:
            logger.info(f"Expired token for {request.path}")
            g.is_authenticated = False
            g.auth_error = "Token expired"

        except TokenInvalidError as e:
            logger.warning(f"Invalid token for {request.path}: {e}")
            g.is_authenticated = False
            g.auth_error = str(e)

    def add_security_headers(self, response):
        """Add security headers to response."""
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["X-Frame-Options"] = "DENY"
        response.headers["X-XSS-Protection"] = "1; mode=block"
        response.headers["Strict-Transport-Security"] = "max-age=31536000; includeSubDomains"
        return response

    def clear_permission_cache(self, user_id: Optional[str] = None):
        """Clear permission cache for a user or all users."""
        if user_id:
            self._permission_cache.pop(user_id, None)
        else:
            self._permission_cache.clear()
            self._last_cache_clear = datetime.utcnow()


def require_permission(permission: str) -> Callable:
    """
    Decorator to require a specific permission.

    Args:
        permission: The required permission string (e.g., "users:write")
    """
    def decorator(func: Callable) -> Callable:
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            if not hasattr(g, "is_authenticated") or not g.is_authenticated:
                return jsonify({"error": "Authentication required"}), 401

            user_permissions = g.token_payload.get("permissions", [])

            if permission not in user_permissions:
                logger.warning(
                    f"User {g.user_id} lacks permission {permission} for {request.path}"
                )
                return jsonify({"error": "Insufficient permissions"}), 403

            return func(*args, **kwargs)
        return wrapper
    return decorator


def require_role(role: str) -> Callable:
    """
    Decorator to require a specific role.

    Args:
        role: The required role (e.g., "admin", "moderator")
    """
    def decorator(func: Callable) -> Callable:
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            if not hasattr(g, "is_authenticated") or not g.is_authenticated:
                return jsonify({"error": "Authentication required"}), 401

            user_roles = g.token_payload.get("roles", [])

            if role not in user_roles:
                logger.warning(
                    f"User {g.user_id} lacks role {role} for {request.path}"
                )
                return jsonify({"error": f"Role '{role}' required"}), 403

            return func(*args, **kwargs)
        return wrapper
    return decorator


def require_any_role(*roles: str) -> Callable:
    """
    Decorator to require any of the specified roles.

    Args:
        roles: Required roles (user must have at least one)
    """
    def decorator(func: Callable) -> Callable:
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            if not hasattr(g, "is_authenticated") or not g.is_authenticated:
                return jsonify({"error": "Authentication required"}), 401

            user_roles = set(g.token_payload.get("roles", []))
            required_roles = set(roles)

            if not user_roles.intersection(required_roles):
                logger.warning(
                    f"User {g.user_id} lacks any of roles {roles} for {request.path}"
                )
                return jsonify({"error": f"One of roles {roles} required"}), 403

            return func(*args, **kwargs)
        return wrapper
    return decorator


def require_all_roles(*roles: str) -> Callable:
    """
    Decorator to require all of the specified roles.

    Args:
        roles: Required roles (user must have all)
    """
    def decorator(func: Callable) -> Callable:
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            if not hasattr(g, "is_authenticated") or not g.is_authenticated:
                return jsonify({"error": "Authentication required"}), 401

            user_roles = set(g.token_payload.get("roles", []))
            required_roles = set(roles)

            if not required_roles.issubset(user_roles):
                logger.warning(
                    f"User {g.user_id} lacks all roles {roles} for {request.path}"
                )
                return jsonify({"error": f"All roles {roles} required"}), 403

            return func(*args, **kwargs)
        return wrapper
    return decorator


class IPWhitelist:
    """Middleware for IP address whitelisting."""

    def __init__(self, allowed_ips: Optional[List[str]] = None):
        """
        Initialize IP whitelist.

        Args:
            allowed_ips: List of allowed IP addresses or CIDR ranges
        """
        self.allowed_ips = set(allowed_ips or [])

    def is_allowed(self, ip_address: str) -> bool:
        """Check if an IP address is whitelisted."""
        if not self.allowed_ips:
            return True

        return ip_address in self.allowed_ips

    def __call__(self, func: Callable) -> Callable:
        """Decorator to enforce IP whitelist."""
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            client_ip = request.headers.get("X-Forwarded-For", request.remote_addr)

            if not self.is_allowed(client_ip):
                logger.warning(f"Blocked request from non-whitelisted IP: {client_ip}")
                return jsonify({"error": "Access denied"}), 403

            return func(*args, **kwargs)
        return wrapper
