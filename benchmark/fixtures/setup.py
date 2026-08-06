#!/usr/bin/env python3
"""
Synthetic Python project generator for benchmarking.

Generates a deterministic Flask-based web application with specific
ground truth strings embedded for correctness checking.
"""

import shutil
import subprocess
from pathlib import Path
from typing import Dict


def get_auth_tokens_content() -> str:
    """Generate src/auth/tokens.py with JWT token validation."""
    return '''"""
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
'''


def get_api_routes_content() -> str:
    """Generate src/api/routes.py with rate limiting decorator (MUST be 300+ lines)."""
    return '''"""
API route handlers and decorators.

This module defines all the HTTP routes for the Flask application,
including authentication, user management, order processing, and admin endpoints.
"""

import time
from collections import defaultdict
from datetime import datetime
from functools import wraps
from threading import Lock
from typing import Callable, Dict, List, Any, Optional

from flask import Blueprint, request, jsonify, g
from werkzeug.exceptions import HTTPException

from ..auth.tokens import validate_jwt_token, create_access_token, refresh_token
from ..database.queries import QueryBuilder
from ..models.user import User
from ..models.order import Order


api = Blueprint("api", __name__)


# Rate limiting storage
_rate_limit_storage: Dict[str, List[float]] = defaultdict(list)
_rate_limit_lock = Lock()


class RateLimitExceeded(HTTPException):
    """Exception raised when rate limit is exceeded."""
    code = 429
    description = "Rate limit exceeded. Please try again later."


def rate_limit(requests_per_minute: int) -> Callable:
    """
    Decorator to implement rate limiting on API endpoints.

    Args:
        requests_per_minute: Maximum number of requests allowed per minute

    Returns:
        Decorated function with rate limiting applied
    """
    def decorator(func: Callable) -> Callable:
        @wraps(func)
        def wrapper(*args, **kwargs):
            # Get client identifier (IP address or user ID)
            client_id = request.headers.get("X-Forwarded-For", request.remote_addr)
            if hasattr(g, "user_id"):
                client_id = f"user_{g.user_id}"

            current_time = time.time()
            window_start = current_time - 60  # 1 minute window

            with _rate_limit_lock:
                # Clean up old timestamps
                _rate_limit_storage[client_id] = [
                    ts for ts in _rate_limit_storage[client_id]
                    if ts > window_start
                ]

                # Check if limit exceeded
                if len(_rate_limit_storage[client_id]) >= requests_per_minute:
                    raise RateLimitExceeded()

                # Add current request timestamp
                _rate_limit_storage[client_id].append(current_time)

            return func(*args, **kwargs)
        return wrapper
    return decorator


def require_auth(func: Callable) -> Callable:
    """Decorator to require authentication for an endpoint."""
    @wraps(func)
    def wrapper(*args, **kwargs):
        auth_header = request.headers.get("Authorization")
        if not auth_header or not auth_header.startswith("Bearer "):
            return jsonify({"error": "Missing or invalid authorization header"}), 401

        token = auth_header.split(" ")[1]
        try:
            payload = validate_jwt_token(token)
            g.user_id = payload["sub"]
            g.token_payload = payload
        except Exception as e:
            return jsonify({"error": str(e)}), 401

        return func(*args, **kwargs)
    return wrapper


@api.route("/health", methods=["GET"])
@rate_limit(requests_per_minute=100)
def health_check():
    """Health check endpoint."""
    return jsonify({
        "status": "healthy",
        "timestamp": datetime.utcnow().isoformat(),
        "version": "1.0.0"
    })


@api.route("/auth/login", methods=["POST"])
@rate_limit(requests_per_minute=10)
def login():
    """
    Authenticate user and return access and refresh tokens.

    Expected JSON body:
        {
            "email": "user@example.com",
            "password": "password123"
        }
    """
    data = request.get_json()

    if not data or "email" not in data or "password" not in data:
        return jsonify({"error": "Missing email or password"}), 400

    # Validate credentials (simplified)
    qb = QueryBuilder()
    user_data = qb.select("users").where("email", data["email"]).first()

    if not user_data or not verify_password(data["password"], user_data["password_hash"]):
        return jsonify({"error": "Invalid credentials"}), 401

    # Create tokens
    access = create_access_token(str(user_data["id"]))
    refresh = refresh_token(str(user_data["id"]))

    return jsonify({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "bearer",
        "expires_in": 1800
    })


@api.route("/auth/refresh", methods=["POST"])
@rate_limit(requests_per_minute=20)
def refresh_access_token():
    """Refresh an access token using a refresh token."""
    data = request.get_json()

    if not data or "refresh_token" not in data:
        return jsonify({"error": "Missing refresh token"}), 400

    try:
        payload = validate_jwt_token(data["refresh_token"])
        if payload.get("type") != "refresh":
            return jsonify({"error": "Invalid token type"}), 401

        new_access = create_access_token(payload["sub"])
        return jsonify({
            "access_token": new_access,
            "token_type": "bearer",
            "expires_in": 1800
        })
    except Exception as e:
        return jsonify({"error": str(e)}), 401


@api.route("/users/me", methods=["GET"])
@require_auth
@rate_limit(requests_per_minute=60)
def get_current_user():
    """Get the current authenticated user's profile."""
    qb = QueryBuilder()
    user_data = qb.select("users").where("id", g.user_id).first()

    if not user_data:
        return jsonify({"error": "User not found"}), 404

    user = User.from_dict(user_data)
    return jsonify(user.to_dict())


@api.route("/users/me", methods=["PATCH"])
@require_auth
@rate_limit(requests_per_minute=20)
def update_current_user():
    """Update the current authenticated user's profile."""
    data = request.get_json()

    allowed_fields = ["name", "email", "phone", "preferences"]
    update_data = {k: v for k, v in data.items() if k in allowed_fields}

    if not update_data:
        return jsonify({"error": "No valid fields to update"}), 400

    qb = QueryBuilder()
    qb.update("users", update_data).where("id", g.user_id).execute()

    return jsonify({"message": "Profile updated successfully"})


@api.route("/users", methods=["GET"])
@require_auth
@rate_limit(requests_per_minute=30)
def list_users():
    """List all users (admin only)."""
    # Check admin permission
    if not g.token_payload.get("is_admin"):
        return jsonify({"error": "Insufficient permissions"}), 403

    page = request.args.get("page", 1, type=int)
    per_page = request.args.get("per_page", 20, type=int)

    qb = QueryBuilder()
    users = qb.select("users").paginate(page, per_page)

    return jsonify({
        "users": [User.from_dict(u).to_dict() for u in users],
        "page": page,
        "per_page": per_page
    })


@api.route("/orders", methods=["POST"])
@require_auth
@rate_limit(requests_per_minute=30)
def create_order():
    """Create a new order."""
    data = request.get_json()

    required_fields = ["items", "shipping_address"]
    if not all(field in data for field in required_fields):
        return jsonify({"error": "Missing required fields"}), 400

    order = Order.create(
        user_id=g.user_id,
        items=data["items"],
        shipping_address=data["shipping_address"]
    )

    qb = QueryBuilder()
    order_id = qb.insert("orders", order.to_dict()).execute()

    return jsonify({"order_id": order_id, "status": "created"}), 201


@api.route("/orders/<order_id>", methods=["GET"])
@require_auth
@rate_limit(requests_per_minute=60)
def get_order(order_id: str):
    """Get details of a specific order."""
    qb = QueryBuilder()
    order_data = qb.select("orders").where("id", order_id).first()

    if not order_data:
        return jsonify({"error": "Order not found"}), 404

    # Check ownership
    if order_data["user_id"] != g.user_id and not g.token_payload.get("is_admin"):
        return jsonify({"error": "Access denied"}), 403

    order = Order.from_dict(order_data)
    return jsonify(order.to_dict())


@api.route("/orders/<order_id>/cancel", methods=["POST"])
@require_auth
@rate_limit(requests_per_minute=20)
def cancel_order(order_id: str):
    """Cancel an order."""
    qb = QueryBuilder()
    order_data = qb.select("orders").where("id", order_id).first()

    if not order_data:
        return jsonify({"error": "Order not found"}), 404

    if order_data["user_id"] != g.user_id:
        return jsonify({"error": "Access denied"}), 403

    order = Order.from_dict(order_data)
    if not order.can_cancel():
        return jsonify({"error": "Order cannot be cancelled"}), 400

    order.cancel()
    qb.update("orders", {"status": order.status}).where("id", order_id).execute()

    return jsonify({"message": "Order cancelled successfully"})


def verify_password(password: str, password_hash: str) -> bool:
    """Verify a password against its hash."""
    # Simplified for demo purposes
    import hashlib
    return hashlib.sha256(password.encode()).hexdigest() == password_hash
'''


def get_database_connection_content() -> str:
    """Generate src/database/connection.py with ConnectionPool class."""
    return '''"""
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
'''


def get_readme_content() -> str:
    """Generate README.md with deployment env vars (200+ lines)."""
    return '''# Flask API Service

A production-ready Flask-based REST API service with authentication, rate limiting, and database connection pooling.

## Features

- JWT-based authentication with access and refresh tokens
- Rate limiting on API endpoints
- Thread-safe database connection pooling
- Input validation and error handling
- Structured logging
- Comprehensive test coverage
- RESTful API design

## Table of Contents

- [Installation](#installation)
- [Usage](#usage)
- [API Reference](#api-reference)
- [Configuration](#configuration)
- [Deployment](#deployment)
- [Development](#development)
- [Testing](#testing)
- [Contributing](#contributing)

## Installation

### Prerequisites

- Python 3.9 or higher
- PostgreSQL 12 or higher
- Redis (optional, for distributed rate limiting)

### Setup

1. Clone the repository:

```bash
git clone https://github.com/yourorg/flask-api.git
cd flask-api
```

2. Create a virtual environment:

```bash
python -m venv venv
source venv/bin/activate  # On Windows: venv\\Scripts\\activate
```

3. Install dependencies:

```bash
pip install -e .
```

4. Set up the database:

```bash
createdb flask_api_dev
python -m src.database.migrations run
```

5. Configure environment variables:

```bash
cp .env.example .env
# Edit .env with your configuration
```

## Usage

### Running the development server

```bash
flask run --debug
```

The API will be available at http://localhost:5000

### Running in production

```bash
gunicorn -w 4 -b 0.0.0.0:8000 "src.app:create_app()"
```

## API Reference

### Authentication Endpoints

#### POST /api/auth/login

Authenticate a user and receive access and refresh tokens.

**Request Body:**
```json
{
  "email": "user@example.com",
  "password": "securepassword123"
}
```

**Response:**
```json
{
  "access_token": "eyJhbGc...",
  "refresh_token": "eyJhbGc...",
  "token_type": "bearer",
  "expires_in": 1800
}
```

#### POST /api/auth/refresh

Refresh an expired access token using a refresh token.

**Request Body:**
```json
{
  "refresh_token": "eyJhbGc..."
}
```

**Response:**
```json
{
  "access_token": "eyJhbGc...",
  "token_type": "bearer",
  "expires_in": 1800
}
```

### User Endpoints

#### GET /api/users/me

Get the current authenticated user's profile.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Response:**
```json
{
  "id": "123",
  "email": "user@example.com",
  "name": "John Doe",
  "created_at": "2026-01-15T10:30:00Z"
}
```

#### PATCH /api/users/me

Update the current user's profile.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Request Body:**
```json
{
  "name": "Jane Doe",
  "phone": "+1234567890"
}
```

### Order Endpoints

#### POST /api/orders

Create a new order.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Request Body:**
```json
{
  "items": [
    {"product_id": "abc", "quantity": 2},
    {"product_id": "def", "quantity": 1}
  ],
  "shipping_address": {
    "street": "123 Main St",
    "city": "San Francisco",
    "state": "CA",
    "zip": "94102"
  }
}
```

#### GET /api/orders/{order_id}

Get details of a specific order.

**Headers:**
```
Authorization: Bearer <access_token>
```

**Response:**
```json
{
  "id": "order_123",
  "user_id": "user_456",
  "items": [...],
  "status": "pending",
  "created_at": "2026-02-10T14:30:00Z"
}
```

## Configuration

The application can be configured using environment variables or a configuration file.

### Environment Variables

- `FLASK_ENV`: Application environment (development, production, testing)
- `DEBUG`: Enable debug mode (true/false)
- `SECRET_KEY`: Secret key for JWT signing
- `DATABASE_URL`: PostgreSQL connection string
- `REDIS_URL`: Redis connection string (optional)
- `LOG_LEVEL`: Logging level (DEBUG, INFO, WARNING, ERROR)

### Configuration File

Create a `config.py` file in the project root:

```python
class Config:
    SECRET_KEY = "your-secret-key"
    SQLALCHEMY_DATABASE_URI = "postgresql://user:pass@localhost/dbname"
    JWT_ALGORITHM = "HS256"
    ACCESS_TOKEN_EXPIRE_MINUTES = 30
```

## Deployment

### Docker Deployment

Build the Docker image:

```bash
docker build -t flask-api:latest .
```

Run the container:

```bash
docker run -d \\
  -p 8000:8000 \\
  -e DATABASE_URL=postgresql://user:pass@host/db \\
  -e SECRET_KEY=your-secret-key \\
  -e REDIS_URL=redis://host:6379 \\
  -e ALLOWED_HOSTS=api.example.com \\
  -e LOG_LEVEL=INFO \\
  flask-api:latest
```

### Kubernetes Deployment

Apply the Kubernetes manifests:

```bash
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml
kubectl apply -f k8s/ingress.yaml
```

### Environment Variables for Deployment

The following environment variables must be configured in your deployment environment:

- `DATABASE_URL`: PostgreSQL connection string (required)
- `SECRET_KEY`: JWT signing secret (required)
- `REDIS_URL`: Redis connection string for distributed rate limiting (optional)
- `ALLOWED_HOSTS`: Comma-separated list of allowed host headers (required in production)
- `LOG_LEVEL`: Logging level (DEBUG, INFO, WARNING, ERROR, CRITICAL)

### Health Checks

The API provides a health check endpoint at `/api/health` that returns:

```json
{
  "status": "healthy",
  "timestamp": "2026-02-12T10:30:00Z",
  "version": "1.0.0"
}
```

## Development

### Project Structure

```
flask-api/
├── src/
│   ├── api/          # API routes and handlers
│   ├── auth/         # Authentication logic
│   ├── database/     # Database connection and queries
│   ├── models/       # Data models
│   └── utils/        # Utility functions
├── tests/            # Test files
├── docs/             # Documentation
└── pyproject.toml    # Project configuration
```

### Code Style

This project follows PEP 8 style guidelines. Use `black` for code formatting:

```bash
black src/ tests/
```

Run linting with `ruff`:

```bash
ruff check src/ tests/
```

## Testing

Run the test suite:

```bash
pytest
```

Run with coverage:

```bash
pytest --cov=src --cov-report=html
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

This project is licensed under the MIT License - see the LICENSE file for details.
'''


def get_auth_middleware_content() -> str:
    """Generate src/auth/middleware.py (250+ lines)."""
    return '''"""
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
'''


def get_database_queries_content() -> str:
    """Generate src/database/queries.py (180+ lines)."""
    return '''"""
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
'''


def get_other_files_content() -> Dict[str, str]:
    """Generate content for remaining files."""
    return {
        "src/typescript/express_middleware.ts": '''export type Request = { headers: Record<string, string> };
export type Response = { statusCode: number };
export type Next = () => void;
export type RequestHandler = (request: Request, response: Response, next: Next) => void;

export function createAuthMiddleware(requiredRole: string): RequestHandler {
    return (_request, response, next) => {
        response.statusCode = requiredRole ? 200 : 403;
        next();
    };
}

export function registerAuth(
    app: { use(handler: RequestHandler): void },
    requiredRole: string,
): void {
    app.use(createAuthMiddleware(requiredRole));
}
''',

        "src/typescript/generic_trace.ts": '''export type UserRecord = { id: string; name: string };

export class GenericRepository<T extends { id: string }> {
    constructor(private readonly records: T[]) {}

    findById(id: string): T | undefined {
        return this.records.find((record) => record.id === id);
    }
}

export function loadUser(
    repository: GenericRepository<UserRecord>,
    id: string,
): UserRecord | undefined {
    return repository.findById(id);
}
''',

        "src/java/Implementor.java": '''interface Notifier {
    void notify(String message);
}

final class EmailNotifier implements Notifier {
    @Override
    public void notify(String message) {
        System.out.println("email:" + message);
    }
}

final class WebhookNotifier implements Notifier {
    @Override
    public void notify(String message) {
        System.out.println("webhook:" + message);
    }
}

public final class Implementor {
    public static Notifier defaultNotifier() {
        return new EmailNotifier();
    }
}
''',

        "CHANGELOG.md": '''# Changelog

## 1.0.0

### Added

- Added request validation helpers.

### Fixed

- Fixed database connection pool annotations.
''',

        "src/utils/arithmetic.py": '''"""Small arithmetic helper used by a tool-neutral reasoning task."""


def multiply(left: int, right: int) -> int:
    return left * right


def combine(left: int, right: int) -> int:
    """The documented example combine(6, 7) evaluates to 42."""
    return multiply(left, right)
''',

        "src/auth/__init__.py": '"""Authentication module."""\n',

        "src/database/__init__.py": '"""Database module."""\n',

        "src/database/migrations.py": '''"""
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
''',

        "src/api/__init__.py": '"""API module."""\n',

        "src/api/validators.py": '''"""
Input validation and schema definitions.

This module provides validators for request data validation and sanitization.
"""

import re
from typing import Any, Dict, List, Optional
from dataclasses import dataclass


EMAIL_REGEX = re.compile(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}$")
PHONE_REGEX = re.compile(r"^\\+?[1-9]\\d{1,14}$")


class ValidationError(Exception):
    """Raised when validation fails."""
    def __init__(self, errors: Dict[str, List[str]]):
        self.errors = errors
        super().__init__(f"Validation failed: {errors}")


@dataclass
class ValidationResult:
    """Result of a validation."""
    is_valid: bool
    errors: Dict[str, List[str]]
    cleaned_data: Optional[Dict[str, Any]] = None


class Validator:
    """Base validator class."""

    def validate(self, data: Dict[str, Any]) -> ValidationResult:
        """Validate data against schema."""
        raise NotImplementedError


class EmailValidator:
    """Validates email addresses."""

    def validate(self, email: str) -> bool:
        """Check if email is valid."""
        return bool(EMAIL_REGEX.match(email))


class PhoneValidator:
    """Validates phone numbers."""

    def validate(self, phone: str) -> bool:
        """Check if phone number is valid."""
        return bool(PHONE_REGEX.match(phone))


class LoginValidator(Validator):
    """Validates login request data."""

    def validate(self, data: Dict[str, Any]) -> ValidationResult:
        """Validate login credentials."""
        errors: Dict[str, List[str]] = {}

        if "email" not in data:
            errors.setdefault("email", []).append("Email is required")
        elif not EmailValidator().validate(data["email"]):
            errors.setdefault("email", []).append("Invalid email format")

        if "password" not in data:
            errors.setdefault("password", []).append("Password is required")
        elif len(data["password"]) < 8:
            errors.setdefault("password", []).append("Password must be at least 8 characters")

        return ValidationResult(
            is_valid=len(errors) == 0,
            errors=errors,
            cleaned_data=data if not errors else None
        )


class UserUpdateValidator(Validator):
    """Validates user profile update data."""

    def validate(self, data: Dict[str, Any]) -> ValidationResult:
        """Validate user update data."""
        errors: Dict[str, List[str]] = {}

        if "email" in data and not EmailValidator().validate(data["email"]):
            errors.setdefault("email", []).append("Invalid email format")

        if "phone" in data and not PhoneValidator().validate(data["phone"]):
            errors.setdefault("phone", []).append("Invalid phone number format")

        if "name" in data and len(data["name"]) < 2:
            errors.setdefault("name", []).append("Name must be at least 2 characters")

        return ValidationResult(
            is_valid=len(errors) == 0,
            errors=errors,
            cleaned_data=data if not errors else None
        )
''',

        "src/models/__init__.py": '"""Data models module."""\n',

        "src/models/user.py": '''"""
User data model.

This module defines the User model and related utilities.
"""

from dataclasses import dataclass, field
from datetime import datetime
from typing import Dict, List, Optional


@dataclass
class User:
    """Represents a user in the system."""
    id: str
    email: str
    name: str
    password_hash: str
    created_at: datetime = field(default_factory=datetime.utcnow)
    updated_at: datetime = field(default_factory=datetime.utcnow)
    is_active: bool = True
    is_admin: bool = False
    roles: List[str] = field(default_factory=list)
    permissions: List[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: Dict) -> "User":
        """Create a User from a dictionary."""
        return cls(
            id=data["id"],
            email=data["email"],
            name=data["name"],
            password_hash=data["password_hash"],
            created_at=data.get("created_at", datetime.utcnow()),
            updated_at=data.get("updated_at", datetime.utcnow()),
            is_active=data.get("is_active", True),
            is_admin=data.get("is_admin", False),
            roles=data.get("roles", []),
            permissions=data.get("permissions", []),
        )

    def to_dict(self) -> Dict:
        """Convert User to a dictionary."""
        return {
            "id": self.id,
            "email": self.email,
            "name": self.name,
            "created_at": self.created_at.isoformat(),
            "updated_at": self.updated_at.isoformat(),
            "is_active": self.is_active,
            "is_admin": self.is_admin,
            "roles": self.roles,
            "permissions": self.permissions,
        }
''',

        "src/models/order.py": '''"""
Order data model.

This module defines the Order model with status transitions.
"""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Dict, List


class OrderStatus(Enum):
    """Order status values."""
    PENDING = "pending"
    CONFIRMED = "confirmed"
    PROCESSING = "processing"
    SHIPPED = "shipped"
    DELIVERED = "delivered"
    CANCELLED = "cancelled"


@dataclass
class OrderItem:
    """Represents an item in an order."""
    product_id: str
    quantity: int
    price: float


@dataclass
class Order:
    """Represents an order in the system."""
    id: str
    user_id: str
    items: List[OrderItem]
    status: OrderStatus
    shipping_address: Dict[str, str]
    created_at: datetime = field(default_factory=datetime.utcnow)
    updated_at: datetime = field(default_factory=datetime.utcnow)

    @classmethod
    def create(cls, user_id: str, items: List[Dict], shipping_address: Dict) -> "Order":
        """Create a new order."""
        order_items = [OrderItem(**item) for item in items]
        return cls(
            id=f"order_{datetime.utcnow().timestamp()}",
            user_id=user_id,
            items=order_items,
            status=OrderStatus.PENDING,
            shipping_address=shipping_address,
        )

    def can_cancel(self) -> bool:
        """Check if order can be cancelled."""
        return self.status in [OrderStatus.PENDING, OrderStatus.CONFIRMED]

    def cancel(self) -> None:
        """Cancel the order."""
        if not self.can_cancel():
            raise ValueError("Order cannot be cancelled")
        self.status = OrderStatus.CANCELLED
        self.updated_at = datetime.utcnow()

    @classmethod
    def from_dict(cls, data: Dict) -> "Order":
        """Create an Order from a dictionary."""
        return cls(
            id=data["id"],
            user_id=data["user_id"],
            items=[OrderItem(**item) for item in data["items"]],
            status=OrderStatus(data["status"]),
            shipping_address=data["shipping_address"],
            created_at=data.get("created_at", datetime.utcnow()),
            updated_at=data.get("updated_at", datetime.utcnow()),
        )

    def to_dict(self) -> Dict:
        """Convert Order to a dictionary."""
        return {
            "id": self.id,
            "user_id": self.user_id,
            "items": [{"product_id": i.product_id, "quantity": i.quantity, "price": i.price} for i in self.items],
            "status": self.status.value,
            "shipping_address": self.shipping_address,
            "created_at": self.created_at.isoformat(),
            "updated_at": self.updated_at.isoformat(),
        }
''',

        "src/utils/__init__.py": '"""Utilities module."""\n',

        "src/utils/logging.py": '''"""
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
''',

        "src/utils/config.py": '''"""
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
''',

        "tests/test_auth.py": '''"""
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
''',

        "tests/test_database.py": '''"""
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
''',

        "pyproject.toml": '''[project]
name = "flask-api"
version = "1.0.0"
description = "Flask-based REST API service"
requires-python = ">=3.9"

dependencies = [
    "flask>=2.3.0",
    "pyjwt>=2.8.0",
    "psycopg2-binary>=2.9.0",
    "pytest>=7.4.0",
]
''',
    }


def setup_repo():
    """Set up the synthetic repository."""
    # Determine repo path
    repo_path = Path(__file__).parent / "repo"

    # Remove existing repo if present
    if repo_path.exists():
        print(f"Removing existing repo at {repo_path}")
        shutil.rmtree(repo_path)

    # Create directory structure
    print(f"Creating repo at {repo_path}")
    repo_path.mkdir(parents=True)

    directories = [
        "src/auth",
        "src/api",
        "src/database",
        "src/models",
        "src/utils",
        "tests",
    ]

    for directory in directories:
        (repo_path / directory).mkdir(parents=True, exist_ok=True)

    # Write main files
    files_to_write = {
        "src/auth/tokens.py": get_auth_tokens_content(),
        "src/api/routes.py": get_api_routes_content(),
        "src/database/connection.py": get_database_connection_content(),
        "README.md": get_readme_content(),
        "src/auth/middleware.py": get_auth_middleware_content(),
        "src/database/queries.py": get_database_queries_content(),
    }

    # Add other files
    files_to_write.update(get_other_files_content())

    # Write all files
    file_stats = []
    for file_path, content in files_to_write.items():
        full_path = repo_path / file_path
        full_path.parent.mkdir(parents=True, exist_ok=True)
        full_path.write_text(content)

        line_count = len(content.splitlines())
        file_stats.append((file_path, line_count))
        print(f"  Created {file_path} ({line_count} lines)")

    # Initialize git repo
    print("\\nInitializing git repository...")
    subprocess.run(["git", "init"], cwd=repo_path, check=True, capture_output=True)
    subprocess.run(["git", "add", "."], cwd=repo_path, check=True, capture_output=True)
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        cwd=repo_path,
        check=True,
        capture_output=True
    )

    # Print summary
    print("\\n" + "="*60)
    print("Repository setup complete!")
    print("="*60)
    print(f"\\nLocation: {repo_path}")
    print(f"Total files: {len(file_stats)}")
    print(f"Total lines: {sum(count for _, count in file_stats)}")

    print("\\nFile breakdown:")
    for file_path, line_count in sorted(file_stats, key=lambda x: -x[1]):
        print(f"  {file_path:40s} {line_count:4d} lines")


if __name__ == "__main__":
    setup_repo()
