//! Explicit model setup through the controlling terminal. Configuration never probes a provider.
use crate::{
    commands::CmdOutput,
    trust::{Terminal, open_terminal},
};
use std::io::{BufRead, Read, Write};
use zeus_api::methods::admin::{
    LlmAuthCancelParams, LlmAuthSignOutParams, LlmAuthStartParams, LlmAuthState,
    LlmAuthStatusParams, LlmConfigureParams,
};
use zeus_api::{ApiError, ApiResult, Client};
use zeus_llm::provider::{HttpDialect, HttpProviderConfig, Pricing};
use zeus_llm::{PrivacyTier, ProviderKind, SpendCaps};

fn error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "api.human_required",
        "Model setup needs a controlling terminal and valid explicit settings; nothing was configured",
    )
}
fn line(terminal: &mut Terminal, prompt: &str) -> ApiResult<String> {
    terminal
        .output
        .write_all(prompt.as_bytes())
        .map_err(error)?;
    terminal.output.flush().map_err(error)?;
    let mut value = String::new();
    let count = terminal
        .input
        .as_mut()
        .take(4097)
        .read_line(&mut value)
        .map_err(error)?;
    if count == 0 || count > 4096 {
        return Err(error("input length"));
    }
    while value.ends_with(['\r', '\n']) {
        value.pop();
    }
    Ok(value)
}

fn dollars(text: &str) -> ApiResult<u64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 6
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(error("amount"));
    }
    let whole = whole.parse::<u64>().map_err(error)?;
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().map_err(error)? * 10u64.pow(6 - fraction.len() as u32)
    };
    whole
        .checked_mul(1_000_000)
        .and_then(|n| n.checked_add(fractional))
        .ok_or_else(|| error("amount overflow"))
}

fn gather(terminal: &mut Terminal) -> ApiResult<Option<LlmConfigureParams>> {
    terminal.output.write_all(b"Zeus model setup\nConfiguration only: this does not send a model request.\n1. Local Ollama\n2. Local OpenAI-compatible server\n3. Remote OpenAI-compatible API\n4. Remote Anthropic API\n5. Disable model tasks\n").map_err(error)?;
    terminal.discard_pending().map_err(error)?;
    let choice = line(terminal, "Provider [1-5]: ")?;
    let mut params = LlmConfigureParams::default();
    if choice.trim() == "5" {
        params.enabled = Some(false);
    } else {
        let (kind, dialect, local) = match choice.trim() {
            "1" => (ProviderKind::LocalOllama, HttpDialect::Ollama, true),
            "2" => (
                ProviderKind::LocalLlamaCpp,
                HttpDialect::ChatCompletions,
                true,
            ),
            "3" => (
                ProviderKind::ApiKeyOpenaiCompatible,
                HttpDialect::ChatCompletions,
                false,
            ),
            "4" => (
                ProviderKind::ApiKeyAnthropic,
                HttpDialect::AnthropicMessages,
                false,
            ),
            _ => return Err(error("provider")),
        };
        let endpoint = line(
            terminal,
            if local {
                "Full local API endpoint (for example http://127.0.0.1:11434/api/chat): "
            } else {
                "Full HTTPS API endpoint (no query or credentials): "
            },
        )?;
        let model = line(terminal, "Exact model name: ")?;
        if model.trim().is_empty() || model.len() > 200 {
            return Err(error("model"));
        }
        let pricing = if local {
            None
        } else {
            terminal.output.write_all(b"Enter conservative price ceilings from your provider's model pricing. Zeus reserves these limits before requests.\n").map_err(error)?;
            let input = dollars(&line(terminal, "USD per million input tokens: ")?)?;
            let output = dollars(&line(
                terminal,
                "USD per million output/reasoning tokens: ",
            )?)?;
            let max_input_tokens = line(terminal, "Maximum input-context tokens for this model: ")?
                .trim()
                .parse::<u64>()
                .map_err(error)?;
            if max_input_tokens == 0 {
                return Err(error("context limit"));
            }
            Some(Pricing {
                input_micro_usd_per_million: input,
                output_micro_usd_per_million: output,
                max_input_tokens,
            })
        };
        params.spend_caps = Some(if local {
            SpendCaps {
                daily_micro_usd: 0,
                monthly_micro_usd: 0,
            }
        } else {
            SpendCaps {
                daily_micro_usd: dollars(&line(terminal, "Daily USD cap (0 blocks spending): ")?)?,
                monthly_micro_usd: dollars(&line(
                    terminal,
                    "Monthly USD cap (0 blocks spending): ",
                )?)?,
            }
        });
        let adapter = HttpProviderConfig {
            kind,
            endpoint: serde_json::from_value(serde_json::json!(endpoint.trim())).map_err(error)?,
            dialect,
            model: model.trim().into(),
            pricing,
            timeout_seconds: 60,
        };
        if adapter.endpoint.query().is_some()
            || adapter.endpoint.fragment().is_some()
            || !adapter.endpoint.username().is_empty()
            || adapter.endpoint.password().is_some()
        {
            return Err(error("endpoint"));
        }
        if !local && adapter.endpoint.scheme() != "https" {
            return Err(error("HTTPS required"));
        }
        params.adapter = Some(adapter);
        params.provider = Some(kind);
        params.enabled = Some(true);
        params.privacy = Some(if local {
            PrivacyTier::LocalOnly
        } else {
            PrivacyTier::MetadataOnly
        });
        if !local {
            terminal.discard_pending().map_err(error)?;
            let guard = terminal
                .physical_input
                .as_ref()
                .map(crate::terminal_os::EchoGuard::disable)
                .transpose()
                .map_err(error)?;
            let key = line(terminal, "API key (hidden; empty keeps the stored key): ")?;
            drop(guard);
            terminal.output.write_all(b"\n").map_err(error)?;
            if !key.is_empty() {
                params.api_key = Some(zeus_types::SecretString::new(key));
            }
        }
        terminal.output.write_all(if local{b"Privacy: local-only. Model tasks will run only on this machine.\n"}else{b"Privacy: metadata-only. Names, sizes and redacted URLs may go to this provider; file contents remain excluded. API keys go to the OS keyring.\n"}).map_err(error)?;
        let caps = params.spend_caps.expect("caps configured");
        terminal
            .output
            .write_all(
                format!(
                    "Caps: {} micro-USD/day, {} micro-USD/month.\n",
                    caps.daily_micro_usd, caps.monthly_micro_usd
                )
                .as_bytes(),
            )
            .map_err(error)?;
    }
    terminal
        .output
        .write_all(b"Review these settings.\n")
        .map_err(error)?;
    terminal.output.flush().map_err(error)?;
    std::thread::sleep(std::time::Duration::from_millis(750));
    terminal.discard_pending().map_err(error)?;
    if line(terminal, "Type configure to save, or Enter to cancel: ")?.trim() != "configure" {
        return Ok(None);
    }
    Ok(Some(params))
}

/// Gather an explicit local configuration and send it over authenticated human IPC.
pub async fn run<C: Client + ?Sized>(client: &C) -> ApiResult<CmdOutput> {
    let mut terminal = open_terminal().map_err(error)?;
    terminal.output.write_all(b"Model access\n1. Configure a model or disable model tasks\n2. Sign in with ChatGPT\n3. Sign in with OpenRouter\n4. List saved ChatGPT accounts\n5. Sign out of a provider\n").map_err(error)?;
    terminal.discard_pending().map_err(error)?;
    match line(&mut terminal, "Action [1-5]: ")?.trim() {
        "1" => {}
        "2" => return sign_in(client, &mut terminal, ProviderKind::ChatgptOauth).await,
        "3" => return sign_in(client, &mut terminal, ProviderKind::OpenrouterOauth).await,
        "4" => {
            let status = zeus_api::client::call::<zeus_api::methods::LlmAuthStatus, _>(
                client,
                LlmAuthStatusParams::default(),
            )
            .await?;
            for account in &status.accounts {
                let text = format!(
                    "{}  {}  selected={}  plan_enabled={}\n",
                    account.account_id,
                    account.email.as_deref().unwrap_or("No verified email"),
                    account.active,
                    account.plan_enabled
                );
                terminal
                    .output
                    .write_all(crate::output::sanitize(&text).as_bytes())
                    .map_err(error)?;
                terminal.output.write_all(b"\n").map_err(error)?;
            }
            return Ok(CmdOutput::new(
                format!("{} saved account(s).", status.accounts.len()),
                &serde_json::json!({"accounts":status.accounts}),
            ));
        }
        "5" => return sign_out(client, &mut terminal).await,
        _ => return Err(error("action")),
    }
    let Some(params) = gather(&mut terminal)? else {
        return Ok(CmdOutput::new(
            "No model settings changed.",
            &serde_json::json!({"changed":false}),
        ));
    };
    let result =
        zeus_api::client::call::<zeus_api::methods::LlmConfigure, _>(client, params).await?;
    Ok(CmdOutput::new(
        "Model settings saved. No model request was sent.",
        &result,
    ))
}

async fn sign_in<C: Client + ?Sized>(
    client: &C,
    terminal: &mut Terminal,
    provider: ProviderKind,
) -> ApiResult<CmdOutput> {
    terminal.output.write_all(b"Sign-in saves access in the OS keyring. It does not enable model inference or change spending limits.\n").map_err(error)?;
    let account_id = if provider == ProviderKind::ChatgptOauth {
        let id = line(
            terminal,
            "Existing account registration ID to renew (Enter creates a new registration): ",
        )?;
        if id.trim().is_empty() {
            None
        } else {
            Some(id.trim().to_owned())
        }
    } else {
        None
    };
    std::thread::sleep(std::time::Duration::from_millis(750));
    terminal.discard_pending().map_err(error)?;
    if line(terminal, "Type sign in to begin, or Enter to cancel: ")?.trim() != "sign in" {
        return Ok(CmdOutput::new(
            "Sign-in canceled.",
            &serde_json::json!({"connected":false}),
        ));
    }
    let started = zeus_api::client::call::<zeus_api::methods::LlmAuthStart, _>(
        client,
        LlmAuthStartParams {
            provider,
            account_id,
        },
    )
    .await?;
    let result=async {
        let url=zeus_types::HttpUrl::parse(&started.authorization_url).map_err(error)?;
        let expected=if provider==ProviderKind::ChatgptOauth {"https://auth.openai.com"}else{"https://openrouter.ai"};
        if url.origin().to_string()!=expected || !started.authorization_url.is_ascii() || !url.url().username().is_empty() || url.url().password().is_some() {return Err(error("unexpected authorization origin"));}
        terminal.output.write_all(b"Open this one-use URL in your browser. Keep it private; it contains the login state.\n").map_err(error)?;
        terminal.output.write_all(url.expose().as_bytes()).map_err(error)?;
        terminal.output.write_all(b"\nWaiting for the browser. Ctrl-C cancels sign-in.\n").map_err(error)?;terminal.output.flush().map_err(error)?;
        loop {
            tokio::select! {
                _=tokio::signal::ctrl_c()=>return Ok(None),
                _=tokio::time::sleep(std::time::Duration::from_secs(1))=>{},
            }
            let status=zeus_api::client::call::<zeus_api::methods::LlmAuthStatus,_>(client,LlmAuthStatusParams{flow_id:Some(started.flow_id.clone())}).await?;
            let flow=status.flow.ok_or_else(||error("missing sign-in flow"))?;
            if flow.state!=LlmAuthState::Waiting {return Ok(Some(flow));}
            if zeus_types::Timestamp::now()>=started.expires_at {return Ok(None);}
        }
    }.await;
    match result {
        Ok(Some(flow)) if flow.state == LlmAuthState::Connected => Ok(CmdOutput::new(
            "Sign-in saved. Model inference and spending settings were not changed.",
            &flow,
        )),
        Ok(Some(flow)) => Err(ApiError::new(
            "api.authentication_failed",
            format!(
                "Sign-in ended: {}. No model request was sent.",
                serde_json::to_value(flow.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "failed".into())
            ),
        )),
        other => {
            let canceled = zeus_api::client::call::<zeus_api::methods::LlmAuthCancel, _>(
                client,
                LlmAuthCancelParams {
                    flow_id: started.flow_id,
                },
            )
            .await;
            match other {
                Err(e) => Err(e),
                _ => {
                    let flow = canceled?;
                    Ok(CmdOutput::new("Sign-in ended.", &flow))
                }
            }
        }
    }
}

async fn sign_out<C: Client + ?Sized>(client: &C, terminal: &mut Terminal) -> ApiResult<CmdOutput> {
    let provider = match line(terminal, "Provider: 1 ChatGPT, 2 OpenRouter: ")?.trim() {
        "1" => ProviderKind::ChatgptOauth,
        "2" => ProviderKind::OpenrouterOauth,
        _ => return Err(error("provider")),
    };
    let account_id = if provider == ProviderKind::ChatgptOauth {
        let id = line(
            terminal,
            "Account registration ID (Enter uses the selected account): ",
        )?;
        if id.trim().is_empty() {
            None
        } else {
            Some(id.trim().to_owned())
        }
    } else {
        None
    };
    std::thread::sleep(std::time::Duration::from_millis(750));
    terminal.discard_pending().map_err(error)?;
    if line(
        terminal,
        "Type sign out to remove these saved credentials: ",
    )?
    .trim()
        != "sign out"
    {
        return Ok(CmdOutput::new(
            "No credentials removed.",
            &serde_json::json!({"cleared":false}),
        ));
    }
    let result = zeus_api::client::call::<zeus_api::methods::LlmAuthSignOut, _>(
        client,
        LlmAuthSignOutParams {
            provider,
            account_id,
        },
    )
    .await?;
    let text = if result.remote_revocation_confirmed {
        "Local credentials removed; provider revocation confirmed."
    } else if result.cleared {
        "Local credentials removed. Provider revocation was not confirmed; manage remaining access on the provider website."
    } else {
        "No local credentials were removed; provider revocation was not confirmed."
    };
    Ok(CmdOutput::new(text, &result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn currency_is_exact_and_rejects_negative_scientific_or_overflow() {
        assert_eq!(dollars("1.25").unwrap(), 1_250_000);
        assert_eq!(dollars("0.000001").unwrap(), 1);
        assert_eq!(dollars("0").unwrap(), 0);
        for bad in ["-1", "1e6", "1.0000001", "18446744073709551615", "NaN"] {
            assert!(dollars(bad).is_err());
        }
    }
    #[test]
    fn local_wizard_requires_explicit_model_and_confirmation_and_has_no_key() {
        let mut terminal = Terminal {
            input: Box::new(std::io::Cursor::new(
                b"1\nhttp://127.0.0.1:11434/api/chat\nuser-model\nconfigure\n".to_vec(),
            )),
            output: Box::new(Vec::new()),
            physical_input: None,
        };
        let config = gather(&mut terminal).unwrap().unwrap();
        assert_eq!(config.enabled, Some(true));
        assert_eq!(config.privacy, Some(PrivacyTier::LocalOnly));
        assert!(config.api_key.is_none());
        assert_eq!(config.adapter.unwrap().model, "user-model");
    }
}
