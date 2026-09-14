use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::provider::Status;

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

pub fn status_color(s: Status) -> Color {
    match s {
        Status::Pending => Color::Gray,
        Status::InTransit => Color::Blue,
        Status::OutForDelivery => Color::Yellow,
        Status::Delivered => Color::Green,
        Status::Exception => Color::Red,
        Status::Unknown => Color::DarkGray,
    }
}

/// Coarse human duration: "45s", "2m", "5h", "2d"; negative or zero -> "now".
pub fn humanize(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs <= 0 {
        "now".into()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub fn draw(frame: &mut Frame, app: &App, now: DateTime<Utc>, spinner_tick: usize) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    draw_header(frame, header, app, now);
    match &app.mode {
        Mode::Detail { scroll } => draw_detail(frame, body, app, *scroll),
        _ => draw_table(frame, body, app, now, spinner_tick),
    }
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>) {
    let next = app
        .parcels
        .parcels
        .iter()
        .filter_map(|p| app.state_for(&p.number))
        .filter(|s| !matches!(s.tracking.as_ref().map(|t| t.status), Some(Status::Delivered)))
        .map(|s| s.next_poll)
        .min()
        .map(|t| format!("next poll {}", humanize(t - now)))
        .unwrap_or_else(|| "next poll —".into());
    let mut spans = vec![
        Span::styled(" parcli ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {} parcels · {}", app.parcels.parcels.len(), next)),
    ];
    if let Some(err) = &app.last_error {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(err.clone(), Style::default().fg(Color::Red)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>, spinner_tick: usize) {
    if app.parcels.parcels.is_empty() {
        let hint = Paragraph::new("no parcels — press a to add a tracking number").dark_gray().centered();
        frame.render_widget(hint, area);
        return;
    }
    let header = Row::new(["LABEL", "NUMBER", "CARRIER", "STATUS", "LAST EVENT", "AGE", "NEXT"])
        .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan));
    let rows = app.parcels.parcels.iter().map(|p| {
        let state = app.state_for(&p.number);
        let tracking = state.and_then(|s| s.tracking.as_ref());
        let status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
        let latest = tracking.and_then(|t| t.events.first());
        let next = if app.polling.as_deref() == Some(p.number.as_str()) {
            format!("{} polling", SPINNER[spinner_tick % SPINNER.len()])
        } else if status == Status::Delivered {
            "done".into()
        } else {
            state.map(|s| humanize(s.next_poll - now)).unwrap_or_else(|| "now".into())
        };
        let status_text = if tracking.is_none() && state.and_then(|s| s.last_error.as_ref()).is_some() {
            "error".to_string()
        } else if tracking.is_none() {
            "…".to_string()
        } else {
            status.label().to_string()
        };
        Row::new(vec![
            Cell::from(p.label.clone().unwrap_or_default()),
            Cell::from(p.number.clone()),
            Cell::from(tracking.and_then(|t| t.carrier.clone()).unwrap_or_default()),
            Cell::from(status_text).style(Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD)),
            Cell::from(latest.map(|e| e.description.clone()).unwrap_or_default()),
            Cell::from(latest.and_then(|e| e.time).map(|t| humanize(now - t)).unwrap_or_default()),
            Cell::from(next),
        ])
    });
    let widths = [
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(18),
        Constraint::Length(16),
        Constraint::Min(20),
        Constraint::Length(5),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, scroll: u16) {
    let Some(parcel) = app.selected_parcel() else { return };
    let tracking = app.state_for(&parcel.number).and_then(|s| s.tracking.as_ref());
    let title = match (&parcel.label, tracking.and_then(|t| t.carrier.as_deref())) {
        (Some(l), Some(c)) => format!(" {l} · {} · {c} ", parcel.number),
        (Some(l), None) => format!(" {l} · {} ", parcel.number),
        (None, Some(c)) => format!(" {} · {c} ", parcel.number),
        (None, None) => format!(" {} ", parcel.number),
    };
    let items: Vec<ListItem> = match tracking {
        None => vec![ListItem::new("no data yet")],
        Some(t) if t.events.is_empty() => vec![ListItem::new("no events reported")],
        Some(t) => t
            .events
            .iter()
            .skip((scroll as usize).min(t.events.len().saturating_sub(1)))
            .map(|e| {
                let when = e.time.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "—".repeat(8));
                let loc = e.location.clone().unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{when}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{loc:<20} "), Style::default().fg(Color::Cyan)),
                    Span::raw(e.description.clone()),
                ]))
            })
            .collect(),
    };
    let status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(Span::styled(format!(" {} ", status.label()), Style::default().fg(status_color(status)))));
    frame.render_widget(List::new(items).block(block), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::Normal => Line::from(" a add · d remove · r refresh · R refresh all · Enter detail · j/k move · q quit ").dark_gray(),
        Mode::Adding(input) => {
            let prompt = " add: ";
            let width = area.width.saturating_sub(prompt.len() as u16 + 1) as usize;
            let scroll = input.visual_scroll(width);
            let shown: String = input.value().chars().skip(scroll).collect();
            let cursor_x = area.x + prompt.len() as u16 + (input.visual_cursor().saturating_sub(scroll)) as u16;
            frame.set_cursor_position((cursor_x, area.y));
            Line::from(vec![Span::styled(prompt, Style::default().fg(Color::Yellow)), Span::raw(shown)])
        }
        Mode::ConfirmRemove => {
            let number = app.selected_parcel().map(|p| p.number.as_str()).unwrap_or("");
            Line::from(format!(" remove {number}? (y/n) ")).yellow()
        }
        Mode::Detail { .. } => Line::from(" j/k scroll · r refresh · q/Esc back ").dark_gray(),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{TrackEvent, Tracking};
    use crate::store::{ParcelList, ParcelState, StateCache};
    use chrono::TimeZone;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app, now(), 0)).unwrap();
        terminal.backend().to_string()
    }

    fn sample_app() -> App {
        let mut list = ParcelList::default();
        list.add("RB123456789CN", Some("camera"), now());
        list.add("1Z999AA10123456784", None, now());
        let mut cache = StateCache::default();
        cache.by_number.insert(
            "RB123456789CN".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "RB123456789CN".into(),
                    carrier: Some("China Post".into()),
                    status: Status::InTransit,
                    events: vec![TrackEvent {
                        time: Some(now() - chrono::Duration::hours(5)),
                        description: "Departed facility".into(),
                        location: Some("Shenzhen".into()),
                    }],
                    fetched_at: now(),
                }),
                last_error: None,
                failures: 0,
                next_poll: now() + chrono::Duration::minutes(7),
            },
        );
        App::new(list, cache, Duration::from_secs(600))
    }

    #[test]
    fn table_shows_header_rows_and_values() {
        let app = sample_app();
        let out = render(&app, 120, 12);
        assert!(out.contains("LABEL"), "{out}");
        assert!(out.contains("camera"), "{out}");
        assert!(out.contains("RB123456789CN"), "{out}");
        assert!(out.contains("China Post"), "{out}");
        assert!(out.contains("in transit"), "{out}");
        assert!(out.contains("Departed facility"), "{out}");
        assert!(out.contains("5h"), "{out}");
        assert!(out.contains("7m"), "{out}");
        assert!(out.contains("1Z999AA10123456784"), "{out}");
        assert!(out.contains("2 parcels"), "{out}");
        assert!(out.contains("a add"), "{out}");
    }

    #[test]
    fn header_ignores_delivered_parcels_for_next_poll() {
        let mut list = ParcelList::default();
        list.add("RB123456789CN", None, now());
        let mut cache = StateCache::default();
        cache.by_number.insert(
            "RB123456789CN".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "RB123456789CN".into(),
                    carrier: None,
                    status: Status::Delivered,
                    events: vec![],
                    fetched_at: now(),
                }),
                last_error: None,
                failures: 0,
                next_poll: now() + chrono::Duration::minutes(5),
            },
        );
        let app = App::new(list, cache, Duration::from_secs(600));
        let out = render(&app, 120, 12);
        assert!(out.contains("next poll \u{2014}"), "{out}");
    }

    #[test]
    fn empty_list_shows_hint() {
        let app = App::new(ParcelList::default(), StateCache::default(), Duration::from_secs(600));
        let out = render(&app, 80, 10);
        assert!(out.contains("press a to add"), "{out}");
    }

    #[test]
    fn adding_mode_shows_prompt_and_input() {
        let mut app = sample_app();
        app.mode = Mode::Adding(tui_input::Input::new("RB9".into()));
        let out = render(&app, 80, 10);
        assert!(out.contains("add:"), "{out}");
        assert!(out.contains("RB9"), "{out}");
    }

    #[test]
    fn confirm_mode_names_the_parcel() {
        let mut app = sample_app();
        app.selected = 1;
        app.mode = Mode::ConfirmRemove;
        let out = render(&app, 80, 10);
        assert!(out.contains("remove 1Z999AA10123456784? (y/n)"), "{out}");
    }

    #[test]
    fn detail_mode_lists_events() {
        let mut app = sample_app();
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 12);
        assert!(out.contains("Shenzhen"), "{out}");
        assert!(out.contains("Departed facility"), "{out}");
        assert!(!out.contains("LABEL"), "{out}");
    }

    #[test]
    fn detail_scroll_past_end_still_shows_last_event() {
        let mut app = sample_app();
        app.mode = Mode::Detail { scroll: 50 };
        let out = render(&app, 100, 12);
        assert!(out.contains("Departed facility"), "{out}");
    }

    #[test]
    fn polling_row_shows_spinner_and_error_shows_in_header() {
        let mut app = sample_app();
        app.polling = Some("1Z999AA10123456784".into());
        app.last_error = Some("1Z999AA10123456784: timed out".into());
        let out = render(&app, 120, 12);
        assert!(out.contains("polling"), "{out}");
        assert!(out.contains("timed out"), "{out}");
    }

    #[test]
    fn humanize_durations() {
        assert_eq!(humanize(chrono::Duration::seconds(45)), "45s");
        assert_eq!(humanize(chrono::Duration::seconds(125)), "2m");
        assert_eq!(humanize(chrono::Duration::hours(5) + chrono::Duration::minutes(3)), "5h");
        assert_eq!(humanize(chrono::Duration::days(2)), "2d");
        assert_eq!(humanize(chrono::Duration::seconds(-5)), "now");
    }

    #[test]
    fn status_colors_are_distinct() {
        let all = [Status::Pending, Status::InTransit, Status::OutForDelivery, Status::Delivered, Status::Exception, Status::Unknown];
        let mut colors: Vec<Color> = all.iter().map(|s| status_color(*s)).collect();
        colors.sort_by_key(|c| format!("{c:?}"));
        colors.dedup();
        assert_eq!(colors.len(), 6);
    }
}
