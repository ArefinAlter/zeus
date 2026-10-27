//! The Zeus app's in-app agent: model providers with the person's own keys, and the thirteen Zeus tools.
//!
//! Provider traffic is made here, by the Rust process, directly to the provider the person chose; the page has no
//! network access and never sees a key. Keys live in the operating system's credential store (or an encrypted file).
//! A provider is called only after the person allowed it, never while the global offline switch is on, never beyond its
//! cap, and a thread's tool calls pass the closed table and the thread's access level before they reach the daemon.
//! Approvals are not a tool: nothing here can decide one.
//!
//! Routes (all under `/native/v1/agent`): `GET config`, `PUT config`, `PUT|DELETE keys/<provider>`,
//! `POST keystore/{initialize,unlock,lock}`, `POST providers/<id>/{test,models}`, `POST preview`, `POST send`,
//! `POST runs/<id>/events`, `POST runs/<id>/stop`, `DELETE threads/<thread>`, `POST usage`, `POST approvals` (read the
//! approvals the agent itself raised; there is no route that decides one), `GET tools`.

#![forbid(unsafe_code)]

pub mod config;
pub mod keys;
pub mod ledger;
pub mod provider;
pub mod runs;
pub mod tools;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use zeus_app::App;
use zeus_app::ext::{Extension, Reply};
use zeus_app::failure::{Failure, Result};

use crate::config::AgentConfig;
use crate::keys::KeyError;
use crate::provider::{ChatRequest, Kind, Msg};
use crate::runs::{Core, SendBody};

/// The module registered by `zeus-app-ext`.
pub struct Module {
    core: Arc<Core>,
}

impl Module {
    /// Build the module (inside a Tokio runtime) for `app`.
    pub fn new(app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self {
            core: Core::new(app.home.clone(), app.daemon.paths().clone()),
        })
    }

    /// The shared state, for tests.
    pub fn core(&self) -> &Arc<Core> {
        &self.core
    }
}

fn key_failure(e: KeyError) -> Failure {
    let code = match e {
        KeyError::Locked => "agent.keystore_locked",
        KeyError::NotInitialized => "agent.keystore_uninitialized",
        KeyError::WrongPassphrase => "agent.wrong_passphrase",
        KeyError::WeakPassphrase => "agent.weak_passphrase",
        _ => "agent.keystore",
    };
    Failure::new(code, e.to_string())
}

impl Core {
    /// The configuration as the page sees it: no keys, only whether one is stored.
    pub fn config_view(&self) -> Value {
        let config = self.config.load();
        let providers: Vec<Value> = config
            .providers
            .iter()
            .map(|p| {
                let (stored, last4) = self.keys.peek(&p.id);
                let month = self.ledger.month(&p.id);
                let (pi, po) = config.prices(p);
                json!({
                    "id": p.id, "name": p.name, "kind": p.kind, "enabled": p.enabled,
                    "base_url": p.base_url, "effective_base_url": p.base(), "model": p.model, "consent": p.consent,
                    "monthly_cap_usd": p.monthly_cap_usd, "tools_mode": p.tools_mode,
                    "price_in_per_mtok": p.price_in_per_mtok, "price_out_per_mtok": p.price_out_per_mtok,
                    "effective_price_in_per_mtok": pi, "effective_price_out_per_mtok": po,
                    "needs_key": p.kind.needs_key(), "local": p.is_local(),
                    "key": {"stored": stored, "last4": last4},
                    "month": {"input_tokens": month.input, "output_tokens": month.output, "usd": month.micro_usd as f64 / 1e6, "calls": month.calls},
                })
            })
            .collect();
        json!({
            "providers": providers,
            "offline": config.offline,
            "daily_cap_usd": config.daily_cap_usd,
            "thread_cap_usd": config.thread_cap_usd,
            "default_price_in_per_mtok": config.default_price_in_per_mtok,
            "default_price_out_per_mtok": config.default_price_out_per_mtok,
            "max_tokens": config.max_tokens,
            "keystore": self.keys.status(),
            "kinds": Kind::ALL.iter().map(|k| json!({"kind": k, "label": k.label(), "default_base_url": k.default_base_url(), "needs_key": k.needs_key()})).collect::<Vec<_>>(),
            "usage": self.ledger.view(&config, None),
        })
    }

    fn provider_or_fail(&self, id: &str) -> Result<(AgentConfig, config::ProviderCfg)> {
        let config = self.config.load();
        let p = config.provider(id).cloned().ok_or_else(|| {
            Failure::new("agent.no_provider", "That provider is not in the ladder.")
        })?;
        Ok((config, p))
    }

    async fn preview(&self, body: SendBody) -> Result<Value> {
        let (config, provider) = self.provider_or_fail(&body.provider_id)?;
        let mut history = self.transcript(&body.thread_id);
        if body.regenerate
            && let Some(i) = history.iter().rposition(|m| matches!(m, Msg::User(_)))
        {
            history.truncate(i + 1);
        }
        let text = (!body.regenerate).then(|| body.text.clone().unwrap_or_default());
        let (req, policy) = self.assemble(&config, &provider, &body, &history, text.as_deref());
        let model = req.model.clone();
        let blocker = self.blocker(&config, &provider, &model);
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| match m {
                Msg::User(t) => json!({"role": "you", "text": t}),
                Msg::Assistant { text, calls, .. } => json!({"role": "model", "text": text, "tool_calls": calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()}),
                Msg::ToolResult { name, content, is_error, .. } => json!({"role": "tool result", "tool": name, "text": content, "is_error": is_error}),
            })
            .collect();
        let sys_tokens = provider::estimate_tokens(&req.system);
        let tool_tokens: u64 = req
            .tools
            .iter()
            .map(|t| {
                provider::estimate_tokens(&t.schema.to_string())
                    + provider::estimate_tokens(&t.description)
            })
            .sum();
        let msg_tokens: u64 = req
            .messages
            .iter()
            .map(|m| match m {
                Msg::User(t) => provider::estimate_tokens(t),
                Msg::Assistant { text, .. } => provider::estimate_tokens(text),
                Msg::ToolResult { content, .. } => provider::estimate_tokens(content),
            })
            .sum();
        let host = url::Url::parse(&provider.base())
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        Ok(json!({
            "provider": {"id": provider.id, "name": provider.name, "kind": provider.kind, "host": host, "model": model, "local": provider.is_local(), "consent": provider.consent},
            "blocker": blocker.map(|(c, m)| json!({"code": c, "message": m})),
            "system": req.system,
            "messages": messages,
            "tools": req.tools.iter().map(|t| json!({"name": t.name, "description": t.description})).collect::<Vec<_>>(),
            "tool_access": policy.access.label(),
            "estimate": {"system_tokens": sys_tokens, "tool_tokens": tool_tokens, "message_tokens": msg_tokens, "total_input_tokens": sys_tokens + tool_tokens + msg_tokens, "max_output_tokens": config.max_tokens},
            "sends": ["the system prompt above", "this thread's messages and tool results, as listed", "the tool definitions listed", if provider.kind.is_subscription() { "the installed agent uses its own sign-in; Zeus supplies no API key" } else { "your API key, only in the request header to the provider" }],
            "never_sends": ["file contents", "cookies, passwords or session tokens", "your other threads"],
        }))
    }

    async fn test_provider(&self, id: &str, model_override: Option<String>) -> Result<Value> {
        let (config, provider) = self.provider_or_fail(id)?;
        let model = model_override
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| provider.model.clone());
        if let Some((code, message)) = self.blocker(&config, &provider, &model) {
            return Ok(json!({"ok": false, "code": code, "message": message}));
        }
        let target = match provider.target() {
            Ok(t) => t,
            Err(e) => return Ok(json!({"ok": false, "code": "bad_url", "message": e.message})),
        };
        if let Err(cap) = self.ledger.check(&config, &provider, "_test", 40) {
            return Ok(json!({"ok": false, "code": "cap", "message": cap.message()}));
        }
        let key = self
            .keys
            .for_provider(provider.kind, &provider.id)
            .map_err(key_failure)?;
        let req = ChatRequest {
            model: model.clone(),
            system: "Reply with the single word OK.".into(),
            messages: vec![Msg::User("ping".into())],
            tools: vec![],
            max_tokens: 16,
        };
        let t0 = Instant::now();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut sink = |_: &str| ();
        match provider::chat(&self.http, &target, key.as_ref(), &req, &mut sink, &cancel).await {
            Ok((o, _)) => {
                let (pi, po) = config.prices(&provider);
                let micro = ledger::cost_micro(o.usage, pi, po);
                self.ledger.charge(&provider.id, "_test", o.usage, micro);
                Ok(
                    json!({"ok": true, "ms": t0.elapsed().as_millis() as u64, "model": model, "text": o.text.chars().take(40).collect::<String>(), "usage": o.usage, "usd": micro as f64 / 1e6}),
                )
            }
            Err(e) => Ok(
                json!({"ok": false, "code": format!("{:?}", e.kind).to_lowercase(), "message": e.message}),
            ),
        }
    }

    async fn list_models(&self, id: &str) -> Result<Value> {
        let (config, provider) = self.provider_or_fail(id)?;
        if provider.kind.is_subscription() {
            return Ok(
                json!({"ok": true, "models": ["default"], "note": "Use the installed CLI default, or enter a model available to your account."}),
            );
        }
        if config.offline {
            return Ok(
                json!({"ok": false, "code": "offline", "message": "Offline is on: no model provider is contacted."}),
            );
        }
        if !provider.consent {
            return Ok(
                json!({"ok": false, "code": "needs_consent", "message": format!("Allow Zeus to contact {} first.", provider.name)}),
            );
        }
        let target = match provider.target() {
            Ok(t) => t,
            Err(e) => return Ok(json!({"ok": false, "code": "bad_url", "message": e.message})),
        };
        let key = self
            .keys
            .for_provider(provider.kind, &provider.id)
            .map_err(key_failure)?;
        if provider.kind.needs_key() && key.is_none() {
            return Ok(
                json!({"ok": false, "code": "needs_key", "message": format!("{} needs an API key first.", provider.name)}),
            );
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        match provider::list_models(&self.http, &target, key.as_ref(), &cancel).await {
            Ok(models) => Ok(json!({"ok": true, "models": models})),
            Err(e) => Ok(
                json!({"ok": false, "code": format!("{:?}", e.kind).to_lowercase(), "message": e.message}),
            ),
        }
    }

    async fn put_config(&self, body: &Value) -> Result<Value> {
        let incoming: AgentConfig = serde_json::from_value(body.clone())?;
        self.config.save(&incoming)?;
        Ok(self.config_view())
    }

    async fn put_key(&self, id: &str, body: &Value) -> Result<Value> {
        self.provider_or_fail(id)?;
        let key = body
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if key.len() < 8
            || key.len() > 512
            || key.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Failure::invalid(
                "That does not look like an API key (8 to 512 characters, no spaces).",
            ));
        }
        let last4 = self.keys.set(id, &key).map_err(key_failure)?;
        Ok(json!({"stored": true, "last4": last4}))
    }
}

impl Extension for Module {
    fn call<'a>(
        &'a self,
        _app: &'a Arc<App>,
        route: &'a str,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async move {
            let (method, path) = route.split_once(' ')?;
            let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
            if parts.len() < 4 || parts[..3] != ["native", "v1", "agent"] {
                return None;
            }
            let core = &self.core;
            Some(match (method, &parts[3..]) {
                ("GET", ["config"]) => Ok(core.config_view()),
                ("PUT", ["config"]) => core.put_config(body).await,
                ("PUT", ["keys", id]) => core.put_key(id, body).await,
                ("DELETE", ["keys", id]) => core
                    .keys
                    .delete(id)
                    .map(|()| json!({"stored": false}))
                    .map_err(key_failure),
                ("POST", ["keystore", "initialize"]) => core
                    .keys
                    .initialize(
                        body.get("passphrase")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                    .map(|()| json!(core.keys.status()))
                    .map_err(key_failure),
                ("POST", ["keystore", "unlock"]) => core
                    .keys
                    .unlock(
                        body.get("passphrase")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                    .map(|()| json!(core.keys.status()))
                    .map_err(key_failure),
                ("POST", ["keystore", "lock"]) => {
                    core.keys.lock();
                    Ok(json!(core.keys.status()))
                }
                ("POST", ["providers", id, "test"]) => {
                    core.test_provider(
                        id,
                        body.get("model").and_then(Value::as_str).map(str::to_owned),
                    )
                    .await
                }
                ("POST", ["providers", id, "models"]) => core.list_models(id).await,
                ("POST", ["preview"]) => match serde_json::from_value::<SendBody>(body.clone()) {
                    Ok(b) => core.preview(b).await,
                    Err(_) => Err(Failure::invalid("The preview request is not valid.")),
                },
                ("POST", ["send"]) => match serde_json::from_value::<SendBody>(body.clone()) {
                    Ok(b) => core.start(b).map(|id| json!({"run_id": id})),
                    Err(_) => Err(Failure::invalid("The message is not valid.")),
                },
                ("POST", ["runs", id, "events"]) => match core.run(id) {
                    Some(r) => Ok(r
                        .log
                        .since(body.get("after").and_then(Value::as_u64).unwrap_or(0))),
                    None => Err(Failure::new(
                        "agent.no_run",
                        "That run is no longer available.",
                    )),
                },
                ("POST", ["runs", id, "stop"]) => Ok(json!({"stopped": core.stop(id)})),
                ("DELETE", ["threads", id]) => {
                    core.forget_thread(id);
                    Ok(json!({"ok": true}))
                }
                ("POST", ["usage"]) => {
                    let thread = body.get("thread_id").and_then(Value::as_str);
                    Ok(core.ledger.view(&core.config.load(), thread))
                }
                ("POST", ["approvals"]) => {
                    let ids: Vec<String> = body
                        .get("ids")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok(json!({"items": core.exec.own_approvals(&ids).await}))
                }
                ("GET", ["tools"]) => Ok(
                    json!({"tools": tools::TABLE.iter().map(|(n, l, a)| json!({"name": n, "level": l.label(), "actions": a.iter().map(|(x, y)| json!({"name": x, "level": y.label()})).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
                ),
                _ => return None,
            })
        })
    }
}

#[cfg(test)]
mod live_subscription_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "real Claude subscription request; VERNE_TEST_SUBSCRIPTION=claude"]
    async fn native_subscription_provider_live() {
        assert_eq!(
            std::env::var("VERNE_TEST_SUBSCRIPTION").as_deref(),
            Ok("claude")
        );
        let d = tempfile::tempdir().unwrap();
        let home = zeus_app::home::AppHome::at(d.path().to_path_buf());
        let paths = zeus_api::endpoint::ZeusPaths {
            state_dir: home.state_dir(),
            config_dir: home.config_dir(),
            runtime_dir: home.run_dir(),
            user_identity: "verne-acceptance".into(),
            profile: None,
        };
        let mut core = Core::new(home.clone(), paths);
        Arc::get_mut(&mut core).unwrap().keys = crate::keys::KeyStore::file_for_tests(&home);
        let mut config = AgentConfig::default();
        config.providers=vec![serde_json::from_value(json!({"id":"p-claude","name":"Claude Code","kind":"claude","enabled":true,"consent":true,"base_url":"","model":"default","price_in_per_mtok":0.0,"price_out_per_mtok":0.0})).unwrap()];
        core.config.save(&config).unwrap();
        let result = core.test_provider("p-claude", None).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["text"].as_str().unwrap().trim(), "OK");
        assert!(!core.keys.status().initialized);
    }
}
