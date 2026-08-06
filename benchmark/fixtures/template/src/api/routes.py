"""
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
