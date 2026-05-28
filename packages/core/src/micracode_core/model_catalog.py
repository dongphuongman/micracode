"""Catalog of provider/model pairs the API will accept at runtime."""

from __future__ import annotations

from dataclasses import dataclass

import httpx

from .config import CoreConfig

ProviderId = str


@dataclass(frozen=True)
class _Model:
    id: str
    label: str
    family: str


@dataclass(frozen=True)
class _Provider:
    id: str
    label: str
    models: tuple[_Model, ...]


_PROVIDERS: tuple[_Provider, ...] = (
    _Provider(
        id="openai",
        label="OpenAI",
        models=(
            _Model(id="gpt-5.4", label="GPT-5.4", family="openai-chat"),
            _Model(id="gpt-5-mini", label="GPT-5 Mini", family="openai-chat"),
            _Model(id="gpt-4.1", label="GPT-4.1", family="openai-chat"),
        ),
    ),
    _Provider(
        id="gemini",
        label="Google Gemini",
        models=(
            _Model(id="gemini-2.5-flash", label="Gemini 2.5 Flash", family="gemini"),
            _Model(id="gemini-2.5-pro", label="Gemini 2.5 Pro", family="gemini"),
            _Model(id="gemini-2.5-flash-lite", label="Gemini 2.5 Flash Lite", family="gemini"),
        ),
    ),
)


def _provider(pid: str) -> _Provider | None:
    for p in _PROVIDERS:
        if p.id == pid:
            return p
    return None


def _has_model(provider: _Provider, model_id: str) -> bool:
    return any(m.id == model_id for m in provider.models)


def _provider_available(config: CoreConfig, pid: str) -> bool:
    if pid == "openai":
        return bool(config.openai_api_key)
    if pid == "gemini":
        return bool(config.google_api_key)
    return False


async def _fetch_ollama_models(base_url: str) -> list[str]:
    try:
        async with httpx.AsyncClient(timeout=3.0) as client:
            resp = await client.get(f"{base_url}/api/tags")
            resp.raise_for_status()
            data = resp.json()
            return [m["name"] for m in data.get("models", [])]
    except Exception:
        return []


async def list_catalog(config: CoreConfig) -> dict:
    """Serialise the registry for the public ``GET /v1/models`` endpoint."""
    providers = [
        {
            "id": p.id,
            "label": p.label,
            "available": _provider_available(config, p.id),
            "models": [{"id": m.id, "label": m.label} for m in p.models],
        }
        for p in _PROVIDERS
    ]

    ollama_models = await _fetch_ollama_models(config.ollama_base_url)
    if ollama_models:
        providers.append(
            {
                "id": "ollama",
                "label": "Ollama (local)",
                "available": True,
                "models": [{"id": name, "label": name} for name in ollama_models],
            }
        )

    default = _default_selection(config, ollama_models)

    return {
        "providers": providers,
        "default": {"provider": default[0], "model": default[1]},
    }


def _default_selection(
    config: CoreConfig, ollama_models: list[str] | None = None
) -> tuple[str, str]:
    env_provider = config.llm_provider
    env_model = config.active_model

    if env_provider == "ollama":
        if env_model:
            return ("ollama", env_model)
        if ollama_models:
            return ("ollama", ollama_models[0])
    else:
        env = _provider(env_provider)
        if env is not None and env_model and _has_model(env, env_model):
            return (env_provider, env_model)

    for p in _PROVIDERS:
        if _provider_available(config, p.id) and p.models:
            return (p.id, p.models[0].id)

    if ollama_models:
        return ("ollama", ollama_models[0])

    first = _PROVIDERS[0]
    return (first.id, first.models[0].id)


def _model_family(provider: str, model_id: str) -> str:
    """Return the family string for a validated (provider, model_id) pair."""
    if provider == "ollama":
        return "ollama"
    p = _provider(provider)
    if p is not None:
        for m in p.models:
            if m.id == model_id:
                return m.family
    return "openai-chat"


def resolve(
    provider: str | None,
    model: str | None,
    config: CoreConfig,
) -> tuple[str, str, str]:
    """Validate a requested ``(provider, model)`` pair, filling in defaults.

    Returns a ``(provider, model, family)`` triple.
    """
    if provider is None and model is None:
        prov, mdl = _default_selection(config)
        return (prov, mdl, _model_family(prov, mdl))

    if provider is None or model is None:
        raise ValueError(
            "Both 'provider' and 'model' must be supplied together; got "
            f"provider={provider!r} model={model!r}."
        )

    if provider == "ollama":
        if not model:
            raise ValueError("model must be non-empty for provider 'ollama'.")
        return ("ollama", model, "ollama")

    p = _provider(provider)
    if p is None:
        known = ", ".join(pp.id for pp in _PROVIDERS) + ", ollama"
        raise ValueError(
            f"Unknown provider {provider!r}; supported providers: {known}."
        )

    if not _has_model(p, model):
        allowed = ", ".join(m.id for m in p.models)
        raise ValueError(
            f"Unknown model {model!r} for provider {provider!r}; "
            f"supported models: {allowed}."
        )

    if not _provider_available(config, p.id):
        env_var = "OPENAI_API_KEY" if p.id == "openai" else "GOOGLE_API_KEY"
        raise ValueError(
            f"Provider {p.id!r} is selected but {env_var} is not configured "
            "on the server."
        )

    return (p.id, model, _model_family(p.id, model))
