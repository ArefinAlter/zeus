//! Live terminal queue and approval inbox; rendering is limited to ten frames per second.
pub mod app;
pub mod view;

use super::connect::{Identity, connect};
use app::{Action, App, Effect, Key};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::IsTerminal;
use std::time::{Duration, Instant};
use zeus_api::endpoint::ZeusPaths;
use zeus_api::methods::events::{SubscribeParams, UnsubscribeParams};
use zeus_api::methods::jobs::{JobsControlParams, JobsListParams};
use zeus_api::methods::sessions::ApprovalsListParams;
use zeus_api::methods::{ApprovalsList, EventsSubscribe, EventsUnsubscribe, JobsControl, JobsList};
use zeus_api::{ApiError, ApiResult, Client, call};

/// Maximum render frequency.
pub const FRAME_BUDGET_MS: u64 = 100;

struct Screen;
impl Screen {
    fn enter() -> ApiResult<Self> {
        enable_raw_mode().map_err(io_error)?;
        if let Err(error) = execute!(std::io::stderr(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(io_error(error));
        }
        Ok(Self)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stderr(), LeaveAlternateScreen);
    }
}
fn io_error(error: std::io::Error) -> ApiError {
    ApiError::internal(format!("terminal: {}", error.kind()))
}

async fn refresh(client: &impl Client, app: &mut App) -> ApiResult<()> {
    let mut jobs = Vec::new();
    let mut params = JobsListParams::default();
    params.page.limit = Some(100);
    loop {
        let page = call::<JobsList, _>(client, params.clone()).await?;
        jobs.extend(page.items);
        match page.next_cursor {
            Some(cursor) if jobs.len() < 10_000 => params.page.cursor = Some(cursor),
            Some(_) => {
                app.message = "Showing the first 10,000 jobs; use zeus ls to filter.".into();
                break;
            }
            None => break,
        }
    }
    app.update(Action::JobsUpdated(jobs));
    let mut approvals = Vec::new();
    let mut params = ApprovalsListParams::default();
    params.page.limit = Some(100);
    loop {
        let page = call::<ApprovalsList, _>(client, params.clone()).await?;
        approvals.extend(page.items);
        match page.next_cursor {
            Some(cursor) if approvals.len() < 10_000 => params.page.cursor = Some(cursor),
            _ => break,
        }
    }
    app.update(Action::ApprovalsUpdated(approvals));
    Ok(())
}

/// Run using the authenticated CLI connection. Approve always opens the controlling-terminal review.
pub async fn run(paths: &ZeusPaths) -> ApiResult<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(ApiError::invalid_params(
            "zeus tui needs an interactive terminal; use zeus ls --json for scripts",
        ));
    }
    let client = connect(paths, Identity::Cli).await?;
    let mut notifications = client.notifications();
    let subscription = call::<EventsSubscribe, _>(
        &client,
        SubscribeParams {
            include_progress: true,
            progress_interval_ms: Some(1_000),
            ..Default::default()
        },
    )
    .await?;
    let no_animation = std::env::var_os("ZEUS_NO_ANIMATION").is_some();
    let mut app = App::new(no_animation);
    refresh(&client, &mut app).await?;
    let mut screen = Some(Screen::enter()?);
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stderr())).map_err(io_error)?;
    let mut last_refresh = Instant::now();
    let mut dirty = true;
    let mut previous_bytes = app.jobs.iter().map(|j| j.bytes_done).sum::<u64>();
    let result = async {
        loop {
            if dirty || !no_animation {
                terminal.draw(|frame| view::render(frame, &app)).map_err(io_error)?;
                dirty = false;
            }
            tokio::select! {
                _ = client.closed() => return Err(ApiError::new(zeus_api::error::codes::UNAVAILABLE, "The daemon stopped. Start it with zeus daemon start.")),
                _ = tokio::time::sleep(Duration::from_millis(FRAME_BUDGET_MS)) => {},
            }
            while event::poll(Duration::ZERO).map_err(io_error)? {
                match event::read().map_err(io_error)? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let decoded = match key.code {
                            KeyCode::Up => Some(Key::Up), KeyCode::Down => Some(Key::Down),
                            KeyCode::Tab => Some(Key::Tab), KeyCode::Enter => Some(Key::Enter),
                            KeyCode::Esc => Some(Key::Esc), KeyCode::Backspace => Some(Key::Backspace),
                            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => Some(Key::Quit),
                            KeyCode::Char(c) => Some(Key::Char(c)), _ => None,
                        };
                        if let Some(key) = decoded {
                            dirty = true;
                            match app.update(Action::Key(key)) {
                                Some(Effect::Quit) => return Ok(()),
                                Some(Effect::Control(job_id, action)) => {
                                    match call::<JobsControl, _>(&client, JobsControlParams {
                                        job_id, action, priority: None, idempotency_key: Some(format!("tui-{}", ulid::Ulid::generate()))
                                    }).await {
                                        Ok(_) => { app.message.clear(); refresh(&client, &mut app).await?; }
                                        Err(error) => app.message = crate::output::sanitize(&format!("{} [{}]", error.envelope().message, error.envelope().code)),
                                    }
                                }
                                Some(Effect::ReviewApproval(id)) => {
                                    drop(screen.take());
                                    let review = crate::commands::review_approval(&client, id).await;
                                    screen = Some(Screen::enter()?);
                                    terminal.clear().map_err(io_error)?;
                                    app.message = match review {
                                        Ok(message) => message,
                                        Err(error) => crate::output::sanitize(&format!("{} [{}]", error.envelope().message, error.envelope().code)),
                                    };
                                    refresh(&client, &mut app).await?;
                                }
                                None => {},
                            }
                        }
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {},
                }
            }
            // Notifications invalidate the snapshot. A periodic refresh also repairs lost/coalesced progress.
            let mut changed = false;
            loop {
                match notifications.try_recv() {
                    Ok(_) => changed = true,
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => changed = true,
                    Err(_) => break,
                }
            }
            if last_refresh.elapsed() >= Duration::from_secs(1) || changed {
                let elapsed = last_refresh.elapsed().as_secs_f64().max(0.1);
                refresh(&client, &mut app).await?;
                let bytes = app.jobs.iter().map(|j| j.bytes_done).sum::<u64>();
                app.update(Action::Tick { bytes_per_second: (bytes.saturating_sub(previous_bytes) as f64 / elapsed) as u64 });
                previous_bytes = bytes;
                last_refresh = Instant::now();
                dirty = true;
            }
        }
    }.await;
    drop(screen);
    let _ = call::<EventsUnsubscribe, _>(
        &client,
        UnsubscribeParams {
            subscription_id: subscription.subscription_id,
        },
    )
    .await;
    result
}
