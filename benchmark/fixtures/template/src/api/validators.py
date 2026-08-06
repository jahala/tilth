"""
Input validation and schema definitions.

This module provides validators for request data validation and sanitization.
"""

import re
from typing import Any, Dict, List, Optional
from dataclasses import dataclass


EMAIL_REGEX = re.compile(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$")
PHONE_REGEX = re.compile(r"^\+?[1-9]\d{1,14}$")


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
