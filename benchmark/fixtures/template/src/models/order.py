"""
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
