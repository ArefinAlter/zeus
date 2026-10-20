//! Rendering. Names and titles are untrusted: everything goes through [`crate::output::sanitize`] first.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{
    Block, Borders, List, ListItem, Paragraph, Row, Sparkline, Table, TableState,
};

use super::app::{App, Pane};
use crate::output::{format_bytes, percent, progress_bar, sanitize};

/// Draw the whole interface.
pub fn render(frame: &mut Frame, app: &App) {
    let [top, details, spark, bottom, footer] = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length((app.approvals.len().min(5) + 2) as u16),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let focus = |pane: Pane| {
        if app.pane == pane {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    let rows: Vec<Row> = app
        .visible_jobs()
        .iter()
        .enumerate()
        .map(|(i, job)| {
            let state = serde_json::to_value(job.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let row = Row::new([
                sanitize(job.filename.as_deref().unwrap_or("-")),
                state,
                serde_json::to_value(job.source_kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                progress_bar(job.bytes_done, job.bytes_total, 16),
                percent(job.bytes_done, job.bytes_total),
                job.bytes_total.map_or("?".to_string(), format_bytes),
            ]);
            if app.pane == Pane::Jobs && i == app.selected {
                row.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(16),
            Constraint::Length(17),
            Constraint::Length(15),
            Constraint::Length(18),
            Constraint::Length(5),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(["Name", "State", "Asked by", "Saved", "%", "Size"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Downloads ({}/{}) /{}{} ",
                app.visible_jobs().len(),
                app.jobs.len(),
                sanitize(&app.query),
                if app.searching {
                    " · Enter to finish, Esc to clear"
                } else {
                    ""
                }
            ))
            .style(focus(Pane::Jobs)),
    );
    let mut selection = TableState::default();
    if app.pane == Pane::Jobs {
        selection.select(Some(app.selected));
    }
    frame.render_stateful_widget(table, top, &mut selection);

    let detail = app
        .selected_job()
        .map(|job| {
            let source = job
                .origin
                .as_ref()
                .map_or_else(|| job.url_redacted.to_string(), |o| o.to_string());
            match &job.last_error {
                Some(error) => format!(
                    "{} · {} [{}] · {}",
                    source,
                    sanitize(&error.message),
                    error.code,
                    match error.class {
                        zeus_types::ErrorClass::AuthExpired =>
                            "Open approvals to review website access.",
                        zeus_types::ErrorClass::Blocked => "Download this file in your browser.",
                        zeus_types::ErrorClass::Disk => "Free disk space, then retry.",
                        _ => "Review the error before retrying.",
                    }
                ),
                None => format!(
                    "{} · {} saved · {}",
                    source,
                    format_bytes(job.bytes_done),
                    job.id
                ),
            }
        })
        .unwrap_or_else(|| {
            "Add a URL with zeus get <url>, or send one from your browser or AI app.".into()
        });
    frame.render_widget(
        Paragraph::new(sanitize(&detail)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Selected download "),
        ),
        details,
    );

    let samples: Vec<u64> = app.throughput.iter().copied().collect();
    let current = samples.last().copied().unwrap_or(0);
    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Throughput {}/s ", format_bytes(current))),
        )
        .data(&samples);
    frame.render_widget(sparkline, spark);

    let items: Vec<ListItem> = app
        .approvals
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let line = format!(
                "{}  {}",
                sanitize(&a.approval.summary.title),
                a.approval
                    .origin
                    .as_ref()
                    .map_or(String::new(), |o| o.to_string())
            );
            let item = ListItem::new(line);
            if app.pane == Pane::Approvals && i == app.selected {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    let approvals = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Approvals waiting for you ({}) ",
                app.approvals.len()
            ))
            .style(focus(Pane::Approvals)),
    );
    frame.render_widget(approvals, bottom);

    let hint = if app.message.is_empty() {
        "q quit  / search  Tab pane  j/k move  p pause  r resume/retry  c cancel  Enter review"
    } else {
        app.message.as_str()
    };
    frame.render_widget(Paragraph::new(hint), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::Action;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use zeus_types::JobSummary;

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn job(name: &str) -> JobSummary {
        serde_json::from_value(serde_json::json!({
            "id": zeus_types::IdGenerator::new().job_id().to_string(), "state": "downloading", "quarantined": false,
            "source_kind": "cli", "filename": name, "url_redacted": "https://a.example/x", "bytes_done": 5, "bytes_total": 10,
            "created_at": "2026-10-05T09:15:30.250Z", "updated_at": "2026-10-05T09:15:30.250Z"
        }))
        .unwrap()
    }

    #[test]
    fn renders_jobs_throughput_and_key_hints() {
        let mut app = App::new(false);
        app.update(Action::JobsUpdated(vec![job("dataset.zip")]));
        app.update(Action::Tick {
            bytes_per_second: 15 * 1024 * 1024,
        });
        let text = screen(&app);
        assert!(
            text.contains("dataset.zip") && text.contains("downloading") && text.contains(" 50%")
        );
        assert!(text.contains("Throughput") && text.contains("15.0 MiB/s"));
        assert!(text.contains("q quit"));
    }

    #[test]
    fn hostile_names_cannot_reach_the_screen() {
        let mut app = App::new(false);
        app.update(Action::JobsUpdated(vec![job(
            "evil\u{1b}[2Jname\u{202e}.pdf",
        )]));
        let text = screen(&app);
        assert!(!text.contains('\u{1b}') && !text.contains('\u{202e}'));
        assert!(text.contains("evil"));
    }

    #[test]
    fn selection_below_the_first_screen_is_visible() {
        let mut app = App::new(false);
        app.update(Action::JobsUpdated(
            (0..100).map(|i| job(&format!("file-{i:03}.zip"))).collect(),
        ));
        app.selected = 99;
        let text = screen(&app);
        assert!(text.contains("file-099.zip"));
        assert!(!text.contains("file-000.zip"));
    }
}
