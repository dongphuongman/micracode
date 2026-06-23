//! Runtime configuration, mirroring `micracode_core.config.CoreConfig` plus the
//! web-transport settings from `micracode_api.config.Settings`.
//!
//! Values are read from the process environment. A repo-root `.env` and an
//! `apps/api/.env` are loaded first (best-effort) so this server picks up the
//! same configuration the Python service uses.

use std::env;
use std::path::PathBuf;

/// Supported LLM providers (matches `CoreConfig.llm_provider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Gemini,
    OpenAi,
    Ollama,
}

impl LlmProvider {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai" => LlmProvider::OpenAi,
            "ollama" => LlmProvider::Ollama,
            _ => LlmProvider::Gemini,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LlmProvider::Gemini => "gemini",
            LlmProvider::OpenAi => "openai",
            LlmProvider::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Kept for parity with `CoreConfig`; not yet surfaced by any endpoint.
    #[allow(dead_code)]
    pub app_name: String,
    pub environment: String,
    pub log_level: String,

    pub llm_provider: LlmProvider,

    pub google_api_key: String,
    pub gemini_model: String,

    pub openai_api_key: String,
    pub openai_model: String,

    pub ollama_base_url: String,
    pub ollama_model: String,

    /// Anthropic credentials for direct (non-conversational) text generation —
    /// commit messages / summaries (PRD FR `ClaudeTextGeneration` / D1).
    pub anthropic_api_key: String,
    pub anthropic_model: String,
    /// Override the Anthropic API host (the SDK's `ANTHROPIC_BASE_URL`
    /// convention); also lets tests point at a mock server.
    pub anthropic_base_url: String,

    /// Where generated projects live on disk (defaults to `~/opener-apps`).
    pub opener_apps_dir: PathBuf,

    /// Comma-separated list of allowed CORS origins (from `APP_WEB_ORIGIN`).
    pub app_web_origin: String,

    /// Address the HTTP server binds to.
    pub host: String,
    pub port: u16,
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn default_data_dir() -> PathBuf {
    // Mirrors `Path.home() / "opener-apps"`.
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("opener-apps")
}

impl Config {
    pub fn from_env() -> Self {
        let provider = LlmProvider::parse(&env_or("LLM_PROVIDER", "gemini"));

        let opener_apps_dir = env::var_os("OPENER_APPS_DIR")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_data_dir);

        let port = env_or("MICRACODE_API_PORT", &env_or("PORT", "8000"))
            .parse::<u16>()
            .unwrap_or(8000);

        Config {
            app_name: env_or("APP_NAME", "micracode"),
            environment: env_or("ENVIRONMENT", "development"),
            log_level: env_or("LOG_LEVEL", "INFO"),
            llm_provider: provider,
            google_api_key: env_or("GOOGLE_API_KEY", ""),
            gemini_model: env_or("GEMINI_MODEL", "gemini-2.5-flash"),
            openai_api_key: env_or("OPENAI_API_KEY", ""),
            openai_model: env_or("OPENAI_MODEL", ""),
            ollama_base_url: env_or("OLLAMA_BASE_URL", "http://localhost:11434"),
            ollama_model: env_or("OLLAMA_MODEL", ""),
            anthropic_api_key: env_or("ANTHROPIC_API_KEY", ""),
            anthropic_model: env_or("ANTHROPIC_MODEL", crate::text_generation::DEFAULT_MODEL),
            anthropic_base_url: env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
            opener_apps_dir,
            app_web_origin: env_or("APP_WEB_ORIGIN", "http://localhost:3000"),
            host: env_or("MICRACODE_API_HOST", "127.0.0.1"),
            port,
        }
    }

    /// The model id for the configured provider (mirrors `CoreConfig.active_model`).
    pub fn active_model(&self) -> &str {
        match self.llm_provider {
            LlmProvider::OpenAi => &self.openai_model,
            LlmProvider::Ollama => &self.ollama_model,
            LlmProvider::Gemini => &self.gemini_model,
        }
    }

    /// CORS origins, split on commas (mirrors `Settings.cors_allow_origins`).
    pub fn cors_allow_origins(&self) -> Vec<String> {
        self.app_web_origin
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
