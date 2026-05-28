"""Unit tests for the provider/model catalog + resolver."""

from __future__ import annotations

import pytest

from micracode_api.config import Settings
from micracode_core import model_catalog


def _settings(**overrides: object) -> Settings:
    base = {
        "google_api_key": "",
        "openai_api_key": "",
        "openai_model": "",
        "gemini_model": "gemini-2.5-flash",
        "llm_provider": "gemini",
    }
    base.update(overrides)
    return Settings(**base)  # type: ignore[arg-type]


async def test_list_catalog_flags_availability_per_key() -> None:
    catalog = await model_catalog.list_catalog(
        _settings(openai_api_key="sk-x", google_api_key="")
    )
    providers = {p["id"]: p for p in catalog["providers"]}
    assert providers["openai"]["available"] is True
    assert providers["gemini"]["available"] is False
    # All registered models are present with id+label.
    assert all({"id", "label"} <= m.keys() for m in providers["openai"]["models"])


async def test_list_catalog_default_prefers_settings_when_valid() -> None:
    catalog = await model_catalog.list_catalog(
        _settings(
            llm_provider="openai",
            openai_api_key="sk-x",
            openai_model="gpt-4.1",
        )
    )
    assert catalog["default"] == {"provider": "openai", "model": "gpt-4.1"}


async def test_list_catalog_default_falls_back_when_env_model_unregistered() -> None:
    catalog = await model_catalog.list_catalog(
        _settings(
            llm_provider="openai",
            openai_api_key="sk-x",
            openai_model="gpt-something-custom",
        )
    )
    # env model is not in the registry; fall back to first available provider's
    # first model. Only openai has a key here.
    assert catalog["default"]["provider"] == "openai"
    assert catalog["default"]["model"] == "gpt-5.4"


def test_resolve_returns_default_when_both_missing() -> None:
    settings = _settings(
        llm_provider="gemini",
        google_api_key="gk",
        gemini_model="gemini-2.5-flash",
    )
    assert model_catalog.resolve(None, None, settings) == (
        "gemini",
        "gemini-2.5-flash",
        "gemini",
    )


def test_resolve_rejects_partial_selection() -> None:
    settings = _settings(openai_api_key="sk-x")
    with pytest.raises(ValueError, match="together"):
        model_catalog.resolve("openai", None, settings)
    with pytest.raises(ValueError, match="together"):
        model_catalog.resolve(None, "gpt-5.4", settings)


def test_resolve_rejects_unknown_provider() -> None:
    with pytest.raises(ValueError, match="Unknown provider"):
        model_catalog.resolve(
            "anthropic", "claude", _settings(openai_api_key="sk-x")
        )


def test_resolve_rejects_unknown_model() -> None:
    with pytest.raises(ValueError, match="Unknown model"):
        model_catalog.resolve(
            "openai", "gpt-9-turbo", _settings(openai_api_key="sk-x")
        )


def test_resolve_rejects_provider_without_key() -> None:
    with pytest.raises(ValueError, match="OPENAI_API_KEY"):
        model_catalog.resolve("openai", "gpt-5.4", _settings(openai_api_key=""))


def test_resolve_accepts_valid_selection() -> None:
    settings = _settings(openai_api_key="sk-x")
    assert model_catalog.resolve("openai", "gpt-4.1", settings) == (
        "openai",
        "gpt-4.1",
        "openai-chat",
    )


# ---------------------------------------------------------------------------
# Family resolution — Slice 1
# ---------------------------------------------------------------------------


def test_resolve_returns_openai_chat_family() -> None:
    provider, model, family = model_catalog.resolve(
        "openai", "gpt-4.1", _settings(openai_api_key="sk-x")
    )
    assert provider == "openai"
    assert model == "gpt-4.1"
    assert family == "openai-chat"


def test_resolve_returns_gemini_family() -> None:
    _, _, family = model_catalog.resolve(
        "gemini", "gemini-2.5-flash", _settings(google_api_key="gk")
    )
    assert family == "gemini"


def test_resolve_returns_ollama_family() -> None:
    provider, model, family = model_catalog.resolve("ollama", "llama3", _settings())
    assert provider == "ollama"
    assert model == "llama3"
    assert family == "ollama"


def test_resolve_all_openai_models_return_openai_chat_family() -> None:
    settings = _settings(openai_api_key="sk-x")
    for model_id in ("gpt-5.4", "gpt-5-mini", "gpt-4.1"):
        _, _, family = model_catalog.resolve("openai", model_id, settings)
        assert family == "openai-chat", f"expected openai-chat for {model_id}"


def test_resolve_all_gemini_models_return_gemini_family() -> None:
    settings = _settings(google_api_key="gk")
    for model_id in ("gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.5-flash-lite"):
        _, _, family = model_catalog.resolve("gemini", model_id, settings)
        assert family == "gemini", f"expected gemini for {model_id}"
