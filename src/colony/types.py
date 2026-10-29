"""Colony type definitions."""

from enum import Enum
from typing import List, Optional


class AgentRole:
    """Agent role definition."""
    def __init__(self, role_id: str, role_type: str, capabilities: Optional[List[str]] = None):
        self.id = role_id
        self.role_type = role_type
        self.capabilities = capabilities or []


class TaskPriority(Enum):
    """Task priority levels."""
    LOW = 1
    NORMAL = 2
    HIGH = 3
    CRITICAL = 4
