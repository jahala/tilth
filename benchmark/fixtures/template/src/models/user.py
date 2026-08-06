"""
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
