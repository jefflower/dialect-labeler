"""Runtime configuration, sourced from environment variables / .env."""

from __future__ import annotations

import secrets
from pathlib import Path

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    db_url: str = "sqlite:///./data/dispatcher.db"
    storage_dir: Path = Path("./storage")

    # Auth
    jwt_secret: str = ""
    jwt_algorithm: str = "HS256"
    jwt_ttl_hours: int = 168  # 7 days
    allow_open_registration: bool = False
    admin_email: str = ""
    admin_password: str = ""

    # Queue / lease
    lease_ttl_seconds: int = 300  # 5 minutes

    # File retention. The cleaner deletes files past these windows but
    # never touches the metadata row.
    output_ttl_hours: float = 168.0  # 7 days
    failed_input_ttl_hours: float = 72.0  # 3 days
    clean_interval_seconds: int = 300

    # Soft upload cap; nginx/reverse-proxy is the real enforcer.
    max_upload_bytes: int = 5 * 1024 * 1024 * 1024  # 5 GB


_settings: Settings | None = None


def get_settings() -> Settings:
    """Lazy singleton — lets tests monkeypatch env before first access."""
    global _settings
    if _settings is None:
        _settings = Settings()
        if not _settings.jwt_secret:
            # Dev-only fallback. Logged loudly so prod misconfigs are obvious.
            _settings.jwt_secret = secrets.token_urlsafe(48)
            import logging

            logging.getLogger("dispatcher").warning(
                "JWT_SECRET not set; generated an ephemeral one. "
                "All sessions die on restart. Set JWT_SECRET in production."
            )
    return _settings


def reset_settings_for_tests() -> None:
    """Drop the cached Settings so the next get_settings() re-reads env."""
    global _settings
    _settings = None
