//! Native cloud sign-in through a controlling terminal and a CLI-owned loopback callback.
use crate::{
    commands::CmdOutput,
    trust::{Terminal, open_terminal},
};
use clap::Subcommand;
use std::io::Write;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;
use zeus_api::methods::cloud::{CloudAccountsListParams, ProviderConfig, ProviderKind};
use zeus_api::methods::cloud_oauth::*;
use zeus_api::methods::{
    CloudAccountsList, CloudOAuthBegin, CloudOAuthCancel, CloudOAuthDisconnect, CloudOAuthFinish,
    CloudOAuthRefresh, CloudOAuthStatusMethod,
};
use zeus_api::{ApiError, ApiResult, Client, call};
use zeus_types::SecretString;

/// Human-only OAuth account lifecycle. Provider application registrations remain explicit.
#[derive(Debug, Clone, Subcommand)]
pub enum CloudAuthCommand {
    /// Sign in with a registered native app. App secrets use hidden terminal input only.
    Login {
        /// Fixed registered callback port for Yandex; Google defaults to an ephemeral port.
        #[arg(long, default_value_t = 0)]
        redirect_port: u16,
    },
    /// Inspect redacted account authorization status.
    Status {
        /// Configured account alias.
        account: String,
    },
    /// Refresh the selected OAuth account using its private keyring credential.
    Refresh {
        /// Configured account alias.
        account: String,
    },
    /// Disable the local account; optional remote revocation has provider-specific scope.
    Disconnect {
        /// Configured account alias.
        account: String,
        /// Ask the provider to revoke this application/device token after local disable.
        #[arg(long)]
        revoke_remote: bool,
    },
}
fn error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "cloud.oauth_failed",
        "Cloud sign-in failed or canceled; no credentials were printed",
    )
}
fn output<T: serde::Serialize>(v: &T) -> CmdOutput {
    let json = serde_json::to_value(v).unwrap_or_default();
    CmdOutput::new(
        crate::output::sanitize(&serde_json::to_string_pretty(&json).unwrap_or_default()),
        &json,
    )
}
fn write(t: &mut Terminal, text: &str) -> ApiResult<()> {
    t.output.write_all(text.as_bytes()).map_err(error)?;
    t.output.flush().map_err(error)
}
fn secret(t: &mut Terminal, prompt: &str) -> ApiResult<Option<SecretString>> {
    t.discard_pending().map_err(error)?;
    let guard = crate::terminal_os::EchoGuard::disable(
        t.physical_input.as_ref().ok_or_else(|| error("terminal"))?,
    )
    .map_err(error)?;
    let value = super::cloud::line(t, prompt)?;
    drop(guard);
    write(t, "\n")?;
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SecretString::new(value)))
    }
}
/// Dispatch after the connection and controlling terminal are independently verified.
pub async fn run<C: Client + ?Sized>(
    command: &CloudAuthCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    if !client.session().kind.is_human_capable() || client.session().downgraded {
        return Err(ApiError::human_required("verified human terminal"));
    }
    if dry_run && !matches!(command, CloudAuthCommand::Status { .. }) {
        return Err(ApiError::invalid_params(
            "OAuth account changes cannot run with --dry-run",
        ));
    }
    let mut terminal = open_terminal().map_err(error)?;
    match command {
        CloudAuthCommand::Login { redirect_port } => {
            login(client, &mut terminal, *redirect_port).await
        }
        CloudAuthCommand::Status { account } => Ok(output(
            &call::<CloudOAuthStatusMethod, _>(
                client,
                CloudOAuthStatusParams {
                    flow_id: None,
                    account_id: Some(account.clone()),
                },
            )
            .await?,
        )),
        CloudAuthCommand::Refresh { account } => {
            let current = account_view(client, account).await?;
            write(
                &mut terminal,
                &format!(
                    "Refresh OAuth for account {} at revision {}.\n",
                    crate::output::sanitize(account),
                    current.revision
                ),
            )?;
            if !super::cloud::confirm(&mut terminal, "REFRESH")? {
                return Ok(super::cloud::canceled());
            }
            Ok(output(
                &call::<CloudOAuthRefresh, _>(
                    client,
                    CloudOAuthRefreshParams {
                        account_id: account.clone(),
                        expected_revision: current.revision,
                        idempotency_key: format!("cli-oauth-refresh-{}", ulid::Ulid::generate()),
                    },
                )
                .await?,
            ))
        }
        CloudAuthCommand::Disconnect {
            account,
            revoke_remote,
        } => {
            let current = account_view(client, account).await?;
            write(
                &mut terminal,
                &format!(
                    "Disable local account {} at revision {} and stop its local transfers.\n",
                    crate::output::sanitize(account),
                    current.revision
                ),
            )?;
            if *revoke_remote {
                write(
                    &mut terminal,
                    "Remote revocation was selected. Google revokes the app/project's granted scopes, including other folders; Yandex revokes this device's token. Unsupported or uncertain remote revocation stays visible.\n",
                )?;
            }
            if !super::cloud::confirm(&mut terminal, "DISCONNECT")? {
                return Ok(super::cloud::canceled());
            }
            Ok(output(
                &call::<CloudOAuthDisconnect, _>(
                    client,
                    CloudOAuthDisconnectParams {
                        account_id: account.clone(),
                        expected_revision: current.revision,
                        remote: *revoke_remote,
                        idempotency_key: format!("cli-oauth-{}", ulid::Ulid::generate()),
                    },
                )
                .await?,
            ))
        }
    }
}
async fn account_view<C: Client + ?Sized>(
    client: &C,
    id: &str,
) -> ApiResult<zeus_api::methods::cloud::CloudAccountView> {
    call::<CloudAccountsList, _>(client, CloudAccountsListParams::default())
        .await?
        .accounts
        .into_iter()
        .find(|a| a.account_id == id)
        .ok_or_else(|| ApiError::new("api.not_found", "cloud account not found"))
}
async fn login<C: Client + ?Sized>(
    client: &C,
    t: &mut Terminal,
    mut port: u16,
) -> ApiResult<CmdOutput> {
    write(
        t,
        "Cloud sign-in\nRegister a desktop/native application with the selected provider first. Zeus uses the system browser or the provider's device page; it never reads browser profiles. Every delivery still needs its own approval.\n1. Google Drive (PKCE)\n2. Yandex Disk (PKCE, exact registered callback port required)\n3. Baidu Netdisk (device authorization)\n",
    )?;
    t.discard_pending().map_err(error)?;
    let provider = match super::cloud::line(t, "Provider [1-3]: ")?.trim() {
        "1" => ProviderKind::GoogleDrive,
        "2" => ProviderKind::YandexDisk,
        "3" => ProviderKind::BaiduNetdisk,
        _ => return Err(error("provider")),
    };
    if provider == ProviderKind::YandexDisk && port == 0 {
        port = super::cloud::line(t, "Exact registered loopback callback port: ")?
            .parse::<u16>()
            .ok()
            .filter(|v| *v != 0)
            .ok_or_else(|| {
                ApiError::invalid_params(
                    "Enter the port from the registered http://127.0.0.1:<port>/oauth/callback URI",
                )
            })?;
    }
    let account_id = super::cloud::line(t, "Account alias: ")?;
    let existing = call::<CloudAccountsList, _>(client, CloudAccountsListParams::default()).await?;
    let expected_revision = existing
        .accounts
        .iter()
        .find(|a| a.account_id == account_id)
        .map(|a| a.revision);
    let label = super::cloud::line(t, "Display label: ")?;
    let client_id = super::cloud::line(t, "Registered client/application ID: ")?;
    let client_secret = secret(
        t,
        "Registered application password (hidden; Google public clients may leave empty): ",
    )?;
    let mut registration = OAuthRegistration::defaults(provider, client_id).map_err(error)?;
    if provider == ProviderKind::GoogleDrive {
        write(
            t,
            "Drive defaults to drive.file (app-authorized files/folders). Full Drive access is broader and may require provider verification.\n",
        )?;
        if super::cloud::line(t, "Scope [app_files/full_drive, default app_files]: ")?.trim()
            == "full_drive"
        {
            registration.scopes = vec!["https://www.googleapis.com/auth/drive".into()];
        }
    }
    let config = match provider {
        ProviderKind::GoogleDrive => ProviderConfig::GoogleDrive {
            parent_id: super::cloud::line(t, "Existing authorized Drive folder ID (or root): ")?,
        },
        ProviderKind::YandexDisk => ProviderConfig::YandexDisk {
            root_path: super::cloud::line(t, "Existing Yandex folder (disk:/Zeus): ")?,
        },
        ProviderKind::BaiduNetdisk => ProviderConfig::BaiduNetdisk {
            root_path: super::cloud::line(t, "Baidu application folder (/apps/<appname>/...): ")?,
        },
        _ => return Err(error("provider")),
    };
    let max_bytes_per_delivery = super::cloud::line(t, "Maximum bytes per delivery: ")?
        .parse()
        .map_err(error)?;
    write(
        t,
        &format!(
            "Reviewed account: {}\nDestination: {}\nPermissions: {}\nBytes per delivery: {}\n",
            crate::output::sanitize(&account_id),
            crate::output::sanitize(&serde_json::to_string(&config).map_err(error)?),
            crate::output::sanitize(&registration.scopes.join(", ")),
            max_bytes_per_delivery
        ),
    )?;
    if !super::cloud::confirm(t, "CONNECT")? {
        return Ok(super::cloud::canceled());
    }
    let listener = if provider != ProviderKind::BaiduNetdisk {
        Some(
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .map_err(error)?,
        )
    } else {
        None
    };
    let redirect_uri = listener
        .as_ref()
        .map(|l| {
            l.local_addr()
                .map(|a| format!("http://127.0.0.1:{}/oauth/callback", a.port()))
        })
        .transpose()
        .map_err(error)?;
    let start = call::<CloudOAuthBegin, _>(
        client,
        CloudOAuthBeginParams {
            account_id,
            label,
            config,
            max_bytes_per_delivery,
            expected_revision,
            registration,
            client_secret,
            redirect_uri: redirect_uri.clone(),
            idempotency_key: format!("cli-oauth-{}", ulid::Ulid::generate()),
        },
    )
    .await?;
    let flow_id = start.status.flow_id.clone().ok_or_else(|| error("flow"))?;
    let result=async{
        let auth=zeus_types::HttpUrl::parse(start.authorization_url.expose()).map_err(error)?;
        let expected=match provider{ProviderKind::GoogleDrive=>"https://accounts.google.com",ProviderKind::YandexDisk=>"https://oauth.yandex.com",ProviderKind::BaiduNetdisk=>"https://openapi.baidu.com",_=>return Err(error("provider"))};
        if auth.origin().to_string()!=expected||!auth.url().username().is_empty()||auth.url().password().is_some(){return Err(error("authorization origin"));}
        write(t,"Open this private one-use URL in your system browser:\n")?;write(t,start.authorization_url.expose())?;write(t,"\n")?;
        if let Some(code)=&start.user_code{write(t,"Enter this code on the official page: ")?;write(t,code.expose())?;write(t,"\n")?;}
        write(t,"Waiting for browser consent. Ctrl-C cancels this sign-in.\n")?;
        if let(Some(listener),Some(redirect))=(listener,redirect_uri){
            let state=auth.url().query_pairs().find(|(k,_)|k=="state").map(|(_,v)|SecretString::new(v.into_owned())).ok_or_else(||error("state"))?;
            let (code,state)=tokio::select!{_=tokio::signal::ctrl_c()=>return Err(error("canceled")),value=receive_callback(listener,&redirect,&state)=>value?};
            tokio::select!{biased;_=tokio::signal::ctrl_c()=>Err(error("canceled")), value=call::<CloudOAuthFinish,_>(client,CloudOAuthFinishParams{flow_id:flow_id.clone(),code:Some(code),state:Some(state)})=>value}
        }else{
            let deadline=tokio::time::Instant::now()+std::time::Duration::from_secs(600);
            loop{
                tokio::select!{_=tokio::signal::ctrl_c()=>return Err(error("canceled")),_=tokio::time::sleep(std::time::Duration::from_millis(start.poll_interval_ms.max(1000)))=>{}}
                if tokio::time::Instant::now()>=deadline{return Err(error("expired"));}
                let status=tokio::select!{biased;_=tokio::signal::ctrl_c()=>return Err(error("canceled")), value=call::<CloudOAuthFinish,_>(client,CloudOAuthFinishParams{flow_id:flow_id.clone(),code:None,state:None})=>value?};
                if status.state!=CloudOAuthState::Waiting{return Ok(status);}
            }
        }
    }.await;
    match result {
        Ok(status) if status.state == CloudOAuthState::Connected => Ok(output(&status)),
        other => {
            let _ = call::<CloudOAuthCancel, _>(client, CloudOAuthCancelParams { flow_id }).await;
            match other {
                Ok(status) => Ok(output(&status)),
                Err(e) => Err(e),
            }
        }
    }
}
async fn receive_callback(
    listener: tokio::net::TcpListener,
    redirect: &str,
    state: &SecretString,
) -> ApiResult<(SecretString, SecretString)> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(error)?
            .map_err(error)?;
        let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut bytes = Zeroizing::new(Vec::new());
            let mut chunk = [0u8; 1024];
            while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).await.map_err(error)?;
                if count == 0 {
                    return Err(error("closed"));
                }
                if bytes.len() + count > 16384 {
                    return Err(error("oversize"));
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            String::from_utf8(std::mem::take(&mut *bytes))
                .map(SecretString::new)
                .map_err(error)
        })
        .await;
        let parsed = request
            .ok()
            .and_then(Result::ok)
            .and_then(|text| callback(text.expose(), redirect, state));
        let response = if parsed.is_some() {
            b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\nAuthorization received. Return to Zeus.\n".as_slice()
        } else {
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.write_all(response),
        )
        .await;
        if let Some(result) = parsed {
            return result.map_err(|_| {
                ApiError::new(
                    "cloud.oauth_denied",
                    "Provider consent was declined; no credentials stored",
                )
            });
        }
    }
}
type CallbackResult = Result<(SecretString, SecretString), ()>;
fn callback(text: &str, redirect: &str, state: &SecretString) -> Option<CallbackResult> {
    let base = zeus_types::HttpUrl::parse(redirect).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    if first.next()? != "GET" {
        return None;
    }
    let target = first.next()?;
    if first.next()? != "HTTP/1.1" || first.next().is_some() {
        return None;
    }
    let hosts: Vec<_> = lines
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .collect();
    if hosts != vec![format!("127.0.0.1:{}", base.url().port()?).as_str()] {
        return None;
    }
    let url = base.url().join(target).ok()?;
    if url.origin() != base.url().origin()
        || url.path() != base.url().path()
        || url.fragment().is_some()
    {
        return None;
    }
    let codes = Zeroizing::new(
        url.query_pairs()
            .filter(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .collect::<Vec<_>>(),
    );
    let states = Zeroizing::new(
        url.query_pairs()
            .filter(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .collect::<Vec<_>>(),
    );
    if states.len() != 1 || states[0] != state.expose() {
        return None;
    }
    if url.query_pairs().any(|(k, _)| k == "error") {
        return Some(Err(()));
    }
    if codes.len() != 1
        || codes[0].is_empty()
        || codes[0].len() > 8192
        || codes[0].chars().any(char::is_control)
    {
        return None;
    }
    Some(Ok((
        SecretString::new(codes[0].clone()),
        SecretString::new(states[0].clone()),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_rejects_rebinding_forgery_duplicates_and_accepts_bound_denial() {
        let state = SecretString::new("ZEUS-CANARY-state".into());
        let redirect = "http://127.0.0.1:54321/oauth/callback";
        let make =
            |target: &str, host: &str| format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        let good = make(
            "/oauth/callback?code=fixture-code&state=ZEUS-CANARY-state",
            "127.0.0.1:54321",
        );
        assert!(matches!(callback(&good, redirect, &state), Some(Ok(_))));
        for request in [
            make("/oauth/callback?code=fixture-code&state=wrong","127.0.0.1:54321"),
            make("/oauth/callback?code=fixture-code&state=ZEUS-CANARY-state","evil.example:54321"),
            make("/oauth/callback?code=one&code=two&state=ZEUS-CANARY-state","127.0.0.1:54321"),
            make("http://evil.example/oauth/callback?code=fixture-code&state=ZEUS-CANARY-state","127.0.0.1:54321"),
            make("/other?code=fixture-code&state=ZEUS-CANARY-state","127.0.0.1:54321"),
            "GET /oauth/callback?code=x&state=ZEUS-CANARY-state HTTP/1.1\r\nHost: 127.0.0.1:54321\r\nHost: 127.0.0.1:54321\r\n\r\n".into(),
        ]{assert!(callback(&request,redirect,&state).is_none());}
        let denied = make(
            "/oauth/callback?error=access_denied&state=ZEUS-CANARY-state",
            "127.0.0.1:54321",
        );
        assert!(matches!(callback(&denied, redirect, &state), Some(Err(()))));
    }
    #[tokio::test]
    async fn loopback_receiver_ignores_wrong_state_and_closes_after_one_bound_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let redirect = format!("http://{address}/oauth/callback");
        let task = tokio::spawn(async move {
            receive_callback(
                listener,
                &redirect,
                &SecretString::new("fixture-state".into()),
            )
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!(
                    "GET /oauth/callback?code=wrong&state=wrong HTTP/1.1\r\nHost: {address}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 400"));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(format!("GET /oauth/callback?code=ZEUS-CANARY-code&state=fixture-state HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"));
        assert!(!reply.contains("ZEUS-CANARY"));
        let (code, state) = task.await.unwrap().unwrap();
        assert_eq!(code.expose(), "ZEUS-CANARY-code");
        assert_eq!(state.expose(), "fixture-state");
    }
}
