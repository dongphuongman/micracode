//! Catalog of provider/model pairs the API accepts at runtime. Ports
//! `micracode_core.model_catalog.list_catalog`, including the optional Ollama
//! probe and the default-selection logic.

use serde_json::{json, Value};

use crate::config::{Config, LlmProvider};

struct Model {
    id: &'static str,
    label: &'static str,
}

struct Provider {
    id: &'static str,
    label: &'static str,
    models: &'static [Model],
}

const PROVIDERS: &[Provider] = &[
    Provider {
        id: "openai",
        label: "OpenAI",
        models: &[
            Model { id: "gpt-5.4", label: "GPT-5.4" },
            Model { id: "gpt-5-mini", label: "GPT-5 Mini" },
            Model { id: "gpt-4.1", label: "GPT-4.1" },
        ],
    },
    Provider {
        id: "gemini",
        label: "Google Gemini",
        models: &[
            Model { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
            Model { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
            Model { id: "gemini-2.5-flash-lite", label: "Gemini 2.5 Flash Lite" },
        ],
    },
];

fn provider(pid: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == pid)
}

fn has_model(p: &Provider, model_id: &str) -> bool {
    p.models.iter().any(|m| m.id == model_id)
}

fn provider_available(config: &Config, pid: &str) -> bool {
    match pid {
        "openai" => !config.openai_api_key.is_empty(),
        "gemini" => !config.google_api_key.is_empty(),
        _ => false,
    }
}

async fn fetch_ollama_models(base_url: &str) -> Vec<String> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let url = format!("{base_url}/api/tags");
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    body.get("models")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Serialise the registry for `GET /v1/models`.
pub async fn list_catalog(config: &Config) -> Value {
    let mut providers: Vec<Value> = PROVIDERS
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "label": p.label,
                "available": provider_available(config, p.id),
                "models": p.models.iter().map(|m| json!({"id": m.id, "label": m.label})).collect::<Vec<_>>(),
            })
        })
        .collect();

    let ollama_models = fetch_ollama_models(&config.ollama_base_url).await;
    if !ollama_models.is_empty() {
        providers.push(json!({
            "id": "ollama",
            "label": "Ollama (local)",
            "available": true,
            "models": ollama_models.iter().map(|n| json!({"id": n, "label": n})).collect::<Vec<_>>(),
        }));
    }

    let (default_provider, default_model) = default_selection(config, &ollama_models);

    json!({
        "providers": providers,
        "default": { "provider": default_provider, "model": default_model },
    })
}

fn default_selection(config: &Config, ollama_models: &[String]) -> (String, String) {
    let env_model = config.active_model();

    match config.llm_provider {
        LlmProvider::Ollama => {
            if !env_model.is_empty() {
                return ("ollama".to_string(), env_model.to_string());
            }
            if let Some(first) = ollama_models.first() {
                return ("ollama".to_string(), first.clone());
            }
        }
        other => {
            if let Some(p) = provider(other.as_str()) {
                if !env_model.is_empty() && has_model(p, env_model) {
                    return (other.as_str().to_string(), env_model.to_string());
                }
            }
        }
    }

    for p in PROVIDERS {
        if provider_available(config, p.id) {
            if let Some(m) = p.models.first() {
                return (p.id.to_string(), m.id.to_string());
            }
        }
    }

    if let Some(first) = ollama_models.first() {
        return ("ollama".to_string(), first.clone());
    }

    let first = &PROVIDERS[0];
    (first.id.to_string(), first.models[0].id.to_string())
}
