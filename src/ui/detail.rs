//! The single-parcel detail view: summary card, journey strip, and event timeline.

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::journey::{journey_stops, render_strip};
use crate::provider::Status;

use super::map::draw_map;
use super::{age_color, humanize, status_color, status_pill, theme};

pub(super) fn draw_detail(frame: &mut Frame, area: Rect, app: &App, scroll: u16, now: DateTime<Utc>, tick: usize) {
    let Some(parcel) = app.selected_parcel() else { return };
    let state = app.state_for(&parcel.number);
    let tracking = state.and_then(|s| s.tracking.as_ref());
    let sel_status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
    let border_style = Style::default().fg(status_color(sel_status));

    let title = format!(" {} ", parcel.label.as_deref().unwrap_or(&parcel.number));
    let outer = Block::bordered().border_type(BorderType::Rounded).title(title).border_style(border_style);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let home = app.parcels.home.as_deref();
    let stops = journey_stops(tracking, home, &app.cache.geo);

    let inner = if app.show_map && area.width >= 100 {
        let [left, right] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(inner);
        draw_map(frame, right, &stops, sel_status, tick);
        left
    } else {
        inner
    };

    let key_style = Style::default().fg(theme::DIM);

    let mut rows: Vec<(String, Span<'static>)> = vec![(String::from("Number"), Span::raw(parcel.number.clone()))];
    if let Some(label) = &parcel.label {
        rows.push((String::from("Label"), Span::raw(label.clone())));
    }
    if let Some(carrier) = tracking.and_then(|t| t.carrier.as_deref()) {
        rows.push((String::from("Carrier"), Span::styled(carrier.to_string(), Style::default().fg(theme::carrier_color(carrier)))));
    }
    if let Some(t) = tracking {
        rows.push((String::from("Status"), status_pill(t.status)));
        for (name, value) in &t.attributes {
            rows.push((name.clone(), Span::raw(value.clone())));
        }
        let age = humanize(now - t.fetched_at);
        let last_update = if age == "now" { "just now".to_string() } else { format!("{age} ago") };
        rows.push((String::from("Last update"), Span::raw(last_update)));
    }
    if let Some(s) = state {
        let next = if app.polling.as_deref() == Some(parcel.number.as_str()) {
            "polling".to_string()
        } else if tracking.map(|t| t.status) == Some(Status::Delivered) {
            "done".to_string()
        } else {
            humanize(s.next_poll - now)
        };
        rows.push((String::from("Next poll"), Span::raw(next)));
    }
    if let Some(url) = tracking.and_then(|t| t.tracking_url.as_deref()) {
        rows.push((String::from("Tracking link"), Span::styled(url.to_string(), Style::default().fg(theme::DIM))));
    }

    let key_w = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let card_lines: Vec<Line> = rows
        .into_iter()
        .map(|(k, v)| Line::from(vec![Span::styled(format!("{k:<key_w$}  "), key_style), v]))
        .collect();

    let max_card_height = inner.height.saturating_sub(6).max(3);
    let card_height = ((card_lines.len() as u16) + 2).min(max_card_height);
    let [card_area, strip_area, timeline_area] =
        Layout::vertical([Constraint::Length(card_height), Constraint::Length(3), Constraint::Min(3)]).areas(inner);

    let card_block = Block::bordered().border_type(BorderType::Rounded).title(" summary ").border_style(border_style);
    frame.render_widget(Paragraph::new(card_lines).block(card_block), card_area);

    let strip_lines = render_strip(&stops, sel_status, tick, strip_area.width);
    frame.render_widget(Paragraph::new(strip_lines), strip_area);

    let event_count = tracking.map(|t| t.events.len()).unwrap_or(0);
    let timeline_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" events ({event_count}) "))
        .border_style(border_style);

    match tracking {
        None => {
            let msg = match state.and_then(|s| s.last_error.as_ref()) {
                Some(err) => Paragraph::new(vec![
                    Line::from("no data yet"),
                    Line::from(Span::styled(format!("last error: {err}"), Style::default().fg(Color::Red))),
                ]),
                None => Paragraph::new("no data yet"),
            };
            frame.render_widget(msg.block(timeline_block), timeline_area);
        }
        Some(t) if t.events.is_empty() => {
            frame.render_widget(Paragraph::new("no events reported").block(timeline_block), timeline_area);
        }
        Some(t) => {
            let items: Vec<ListItem> = t
                .events
                .iter()
                .enumerate()
                .skip((scroll as usize).min(t.events.len().saturating_sub(1)))
                .map(|(i, e)| {
                    let is_newest = i == 0;
                    let dot = if is_newest {
                        Span::styled("● ", Style::default().fg(status_color(t.status)))
                    } else {
                        Span::styled("│ ", Style::default().fg(theme::DIM))
                    };
                    let when = e.time.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "—".repeat(16));
                    let when_color = e.time.map(|d| age_color(now - d)).unwrap_or(theme::DIM);
                    let loc = e.location.clone().unwrap_or_default();
                    let mut text_style = Style::default();
                    if is_newest {
                        text_style = text_style.add_modifier(Modifier::BOLD);
                    }
                    let mut lines = vec![Line::from(vec![
                        dot,
                        Span::styled(format!("{when}  "), Style::default().fg(when_color)),
                        Span::styled(format!("{loc:<20} "), Style::default().fg(theme::ACCENT)),
                        Span::styled(e.display_text().to_string(), text_style),
                    ])];
                    if let Some(translated) = &e.translated {
                        if translated != &e.description {
                            lines.push(Line::from(vec![
                                Span::styled("│ ", Style::default().fg(theme::DIM)),
                                Span::styled(
                                    e.description.clone(),
                                    Style::default().fg(theme::DIM).add_modifier(Modifier::ITALIC),
                                ),
                            ]));
                        }
                    }
                    ListItem::new(lines)
                })
                .collect();
            frame.render_widget(List::new(items).block(timeline_block), timeline_area);
        }
    }
}
