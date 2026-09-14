use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::provider::Status;

mod detail;
mod map;
pub mod theme;

use detail::draw_detail;

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

/// Green when fresher than a day, yellow under three days, red beyond that.
pub fn age_color(age: chrono::Duration) -> Color {
    if age < chrono::Duration::hours(24) {
        Color::Green
    } else if age < chrono::Duration::hours(72) {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Render a `width`-cell bar, `▰` for the elapsed fraction and `▱` for the rest.
pub fn progress_bar(frac: f64, width: usize) -> String {
    let frac = if frac.is_nan() { 0.0 } else { frac.clamp(0.0, 1.0) };
    let filled = ((frac * width as f64).floor() as usize).min(width);
    format!("{}{}", "▰".repeat(filled), "▱".repeat(width - filled))
}

/// A colored " label " pill for a status, black text on the status color.
pub fn status_pill(status: Status) -> Span<'static> {
    Span::styled(
        format!(" {} ", status.label()),
        Style::default().fg(Color::Black).bg(status_color(status)).add_modifier(Modifier::BOLD),
    )
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
        Mode::Detail { scroll } => draw_detail(frame, body, app, *scroll, now, spinner_tick),
        _ => draw_table(frame, body, app, now, spinner_tick),
    }
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>) {
    let [left, right] = Layout::horizontal([Constraint::Min(0), Constraint::Length(8)]).areas(area);
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
    let home_span = if app.parcels.home.is_some() {
        Span::styled("⌂ set", Style::default().fg(Color::Green))
    } else {
        Span::styled("⌂ unset", Style::default().fg(theme::DIM))
    };
    let mut spans = vec![
        Span::styled(" parcli ", Style::default().fg(Color::Black).bg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {} parcels · ", app.parcels.parcels.len())),
        home_span,
        Span::raw(format!(" · {next}")),
    ];
    if let Some(err) = &app.last_error {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(format!(" {err} "), Style::default().fg(Color::Red).bg(theme::ERROR_BG)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
    let clock = now.with_timezone(&chrono::Local).format("%H:%M:%S").to_string();
    frame.render_widget(Paragraph::new(clock).right_aligned(), right);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>, spinner_tick: usize) {
    if app.parcels.parcels.is_empty() {
        let hint = Paragraph::new("no parcels — press a to add a tracking number")
            .style(Style::default().fg(theme::DIM))
            .centered();
        frame.render_widget(hint, area);
        return;
    }
    let sel_status = app
        .selected_parcel()
        .and_then(|p| app.state_for(&p.number))
        .and_then(|s| s.tracking.as_ref())
        .map(|t| t.status)
        .unwrap_or(Status::Unknown);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(" parcels ", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)))
        .border_style(Style::default().fg(status_color(sel_status)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = Row::new(["LABEL", "NUMBER", "CARRIER", "STATUS", "LAST EVENT", "AGE", "NEXT"])
        .style(Style::default().fg(Color::Black).bg(theme::ACCENT).add_modifier(Modifier::BOLD));
    let zebra = Style::default().bg(theme::ZEBRA);
    let rows = app.parcels.parcels.iter().enumerate().map(|(i, p)| {
        let state = app.state_for(&p.number);
        let tracking = state.and_then(|s| s.tracking.as_ref());
        let status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
        let latest = tracking.and_then(|t| t.events.first());

        let next_cell = if app.polling.as_deref() == Some(p.number.as_str()) {
            Cell::from(format!("{} polling", SPINNER[spinner_tick % SPINNER.len()])).style(Style::default().fg(Color::Yellow))
        } else if status == Status::Delivered {
            Cell::from("done").style(Style::default().fg(Color::Green))
        } else if let Some(s) = state {
            let remaining = s.next_poll - now;
            let frac = 1.0 - remaining.num_seconds() as f64 / app.interval.as_secs_f64();
            Cell::from(format!("{} {}", progress_bar(frac, 5), humanize(remaining)))
        } else {
            Cell::from("now")
        };

        let status_cell = if tracking.is_some() {
            Cell::from(Line::from(status_pill(status)))
        } else if state.and_then(|s| s.last_error.as_ref()).is_some() {
            Cell::from("error").style(Style::default().fg(Color::Red))
        } else {
            Cell::from("…").style(Style::default().fg(theme::DIM))
        };

        let age = latest.and_then(|e| e.time).map(|t| now - t);
        let age_cell = match age {
            Some(d) => Cell::from(humanize(d)).style(Style::default().fg(age_color(d))),
            None => Cell::from(""),
        };

        let carrier_name = tracking.and_then(|t| t.carrier.clone()).unwrap_or_default();
        let carrier_fg = if carrier_name.is_empty() { theme::CARRIER } else { theme::carrier_color(&carrier_name) };
        let mut row = Row::new(vec![
            Cell::from(p.label.clone().unwrap_or_default()).style(Style::default().fg(theme::ACCENT)),
            Cell::from(p.number.clone()).style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Cell::from(carrier_name).style(Style::default().fg(carrier_fg)),
            status_cell,
            Cell::from(latest.map(|e| e.display_text().to_string()).unwrap_or_default()),
            age_cell,
            next_cell,
        ]);
        if i % 2 == 1 && i != app.selected {
            row = row.style(zebra);
        }
        row
    });
    let widths = [
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(18),
        Constraint::Length(18),
        Constraint::Min(20),
        Constraint::Length(5),
        Constraint::Length(14),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// Join `key_hint` groups into one line, separated by " · ", with a leading space.
fn hint_line(hints: Vec<Vec<Span<'static>>>) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, hint) in hints.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
        }
        spans.extend(hint);
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::Normal => {
            let mut hints = vec![
                theme::key_hint("a", "add"),
                theme::key_hint("d", "remove"),
                theme::key_hint("r", "refresh"),
                theme::key_hint("R", "refresh all"),
                theme::key_hint("Enter", "detail"),
                theme::key_hint("j/k", "move"),
                theme::key_hint("q", "quit"),
            ];
            if app.parcels.home.is_none() {
                hints.push(vec![Span::styled("--home to set your address", Style::default().fg(theme::DIM))]);
            }
            hint_line(hints)
        }
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
        Mode::Detail { .. } => hint_line(vec![
            theme::key_hint("j/k", "scroll"),
            theme::key_hint("r", "refresh"),
            theme::key_hint("q/Esc", "back"),
        ]),
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
                        translated: None,
                    }],
                    fetched_at: now(),
                    attributes: vec![],
                    tracking_url: None,
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
        assert!(out.contains("parcels"), "block title: {out}");
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
                    attributes: vec![],
                    tracking_url: None,
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
    fn detail_shows_summary_card_and_timeline_with_original_text() {
        let mut app = sample_app();
        {
            let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
            let t = st.tracking.as_mut().unwrap();
            t.events[0].translated = Some("Left the facility".into());
            t.attributes = vec![("Days in transit".into(), "2".into())];
            t.tracking_url = Some("https://example.test/track".into());
        }
        app.mode = Mode::Detail { scroll: 0 };
        // +3 rows over the other detail tests: the journey strip (task 4) now takes a
        // fixed 3 rows between the card and timeline, and this test's 8-row card plus
        // 2-line event needs the extra room to keep everything visible at once.
        let out = render(&app, 100, 23);
        for needle in [
            "summary",
            "Number",
            "RB123456789CN",
            "Carrier",
            "China Post",
            "Days in transit",
            "2",
            "Tracking link",
            "https://example.test/track",
            "events (1)",
            "●",
            "Left the facility",
            "Departed facility",
            "Shenzhen",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
        }

        // Value column must line up: every key is padded to the width of the longest key
        // present, so "Number" and "Days in transit" values start at the same column.
        let keys = ["Number", "Label", "Carrier", "Status", "Days in transit", "Last update", "Next poll", "Tracking link"];
        let key_w = keys.iter().map(|k| k.chars().count()).max().unwrap();
        let expected_number = format!("{:<key_w$}  {}", "Number", "RB123456789CN");
        let expected_attr = format!("{:<key_w$}  {}", "Days in transit", "2");
        assert!(out.contains(&expected_number), "keys not aligned, missing {expected_number:?} in:\n{out}");
        assert!(out.contains(&expected_attr), "keys not aligned, missing {expected_attr:?} in:\n{out}");
    }

    #[test]
    fn detail_event_already_english_renders_one_line_not_two() {
        let mut app = sample_app();
        {
            let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
            // Same text stored as `translated` (as translate_events now persists for
            // already-English text) must not print a duplicate original line beneath.
            st.tracking.as_mut().unwrap().events[0].translated = Some("Departed facility".into());
        }
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 20);
        assert_eq!(out.matches("Departed facility").count(), 1, "should not duplicate the original line:\n{out}");
    }

    #[test]
    fn detail_next_poll_mirrors_table_for_delivered_and_overdue() {
        let mut app = sample_app();
        {
            let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
            st.tracking.as_mut().unwrap().status = Status::Delivered;
            st.next_poll = now() + chrono::Duration::minutes(10); // not overdue, but delivered
        }
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 20);
        // No `tracking_url`/attributes set here, so the card's longest key is "Last update".
        let keys = ["Number", "Label", "Carrier", "Status", "Last update", "Next poll"];
        let key_w = keys.iter().map(|k| k.chars().count()).max().unwrap();
        let expected_done = format!("{:<key_w$}  {}", "Next poll", "done");
        assert!(out.contains(&expected_done), "delivered parcel should show done:\n{out}");

        // Overdue but not delivered: table shows "now" via humanize, detail must match.
        app.selected = 1;
        app.cache.by_number.insert(
            "1Z999AA10123456784".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "1Z999AA10123456784".into(),
                    carrier: None,
                    status: Status::InTransit,
                    events: vec![],
                    fetched_at: now(),
                    attributes: vec![],
                    tracking_url: None,
                }),
                last_error: None,
                failures: 0,
                next_poll: now() - chrono::Duration::minutes(1),
            },
        );
        let out = render(&app, 100, 20);
        let expected_now = format!("{:<key_w$}  {}", "Next poll", "now");
        assert!(out.contains(&expected_now), "overdue non-delivered parcel should show now:\n{out}");
    }

    #[test]
    fn detail_without_data_shows_placeholder_and_error() {
        let mut app = sample_app();
        app.selected = 1; // 1Z999… has no cache entry
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 16);
        assert!(out.contains("no data yet"), "{out}");
        app.cache.by_number.insert(
            "1Z999AA10123456784".into(),
            crate::store::ParcelState { tracking: None, last_error: Some("timed out".into()), failures: 1, next_poll: now() },
        );
        let out = render(&app, 100, 16);
        assert!(out.contains("timed out"), "{out}");
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
    fn progress_bar_fills_with_fraction() {
        assert_eq!(progress_bar(0.0, 5), "▱▱▱▱▱");
        assert_eq!(progress_bar(0.5, 4), "▰▰▱▱");
        assert_eq!(progress_bar(1.0, 3), "▰▰▰");
        assert_eq!(progress_bar(7.0, 2), "▰▰", "clamped");
        assert_eq!(progress_bar(-1.0, 2), "▱▱", "clamped");
    }

    #[test]
    fn age_colors_by_staleness() {
        assert_eq!(age_color(chrono::Duration::hours(3)), Color::Green);
        assert_eq!(age_color(chrono::Duration::hours(30)), Color::Yellow);
        assert_eq!(age_color(chrono::Duration::days(4)), Color::Red);
    }

    #[test]
    fn table_shows_progress_bar_and_translated_text() {
        let mut app = sample_app();
        // 7 minutes remaining of a 10-minute interval → 30% elapsed → 1 of 5 blocks
        let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
        st.tracking.as_mut().unwrap().events[0].translated = Some("Left the facility".into());
        let out = render(&app, 120, 12);
        assert!(out.contains("▰▱▱▱▱ 7m"), "{out}");
        assert!(out.contains("Left the facility"), "{out}");
        assert!(!out.contains("Departed facility"), "table shows translation only: {out}");
        assert!(out.contains("parcels"), "block title: {out}");
    }

    #[test]
    fn status_colors_are_distinct() {
        let all = [Status::Pending, Status::InTransit, Status::OutForDelivery, Status::Delivered, Status::Exception, Status::Unknown];
        let mut colors: Vec<Color> = all.iter().map(|s| status_color(*s)).collect();
        colors.sort_by_key(|c| format!("{c:?}"));
        colors.dedup();
        assert_eq!(colors.len(), 6);
    }

    #[test]
    fn footer_shows_key_hints() {
        let app = sample_app();
        let out = render(&app, 120, 12);
        assert!(out.contains("a add"), "{out}");
        assert!(out.contains("d remove"), "{out}");
        assert!(out.contains("--home to set your address"), "no home set, should hint at --home:\n{out}");

        let mut app = sample_app();
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 12);
        assert!(out.contains("j/k scroll"), "{out}");
        assert!(out.contains("q/Esc back"), "{out}");
    }

    #[test]
    fn header_shows_home_indicator() {
        let app = sample_app();
        let out = render(&app, 120, 12);
        assert!(out.contains('⌂'), "{out}");
        assert!(out.contains("⌂ unset"), "no home configured:\n{out}");

        let mut app = sample_app();
        app.parcels.home = Some("Roissy CDG".into());
        let out = render(&app, 120, 12);
        assert!(out.contains("⌂ set"), "home configured:\n{out}");
        assert!(!out.contains("--home to set your address"), "home is set, no hint needed:\n{out}");
    }

    #[test]
    fn detail_shows_journey_strip_with_home() {
        let mut app = sample_app();
        app.parcels.home = Some("Berlin".into());
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 20);
        assert!(out.contains('┄'), "in-transit strip should show remaining dashes to home:\n{out}");
        assert!(out.contains("Berlin"), "strip should name the home stop:\n{out}");
    }

    #[test]
    fn detail_wide_shows_map_with_home_pin() {
        let mut app = sample_app();
        app.parcels.home = Some("Berlin".into());
        app.cache.geo.insert("Shenzhen".into(), Some((22.5445741, 114.0545429)));
        app.cache.geo.insert("Berlin".into(), Some((52.52, 13.405)));
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 120, 30);
        assert!(out.contains("map"), "wide detail should show the map pane:\n{out}");
        assert!(out.contains('⌂'), "map should pin home:\n{out}");
    }

    #[test]
    fn detail_narrow_hides_map() {
        let mut app = sample_app();
        app.parcels.home = Some("Berlin".into());
        app.cache.geo.insert("Shenzhen".into(), Some((22.5445741, 114.0545429)));
        app.cache.geo.insert("Berlin".into(), Some((52.52, 13.405)));
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 90, 30);
        assert!(!out.contains("map"), "narrow detail should not show the map pane:\n{out}");
    }

    #[test]
    fn detail_map_toggle_off_hides_map() {
        let mut app = sample_app();
        app.parcels.home = Some("Berlin".into());
        app.cache.geo.insert("Shenzhen".into(), Some((22.5445741, 114.0545429)));
        app.cache.geo.insert("Berlin".into(), Some((52.52, 13.405)));
        app.mode = Mode::Detail { scroll: 0 };
        app.show_map = false;
        let out = render(&app, 120, 30);
        assert!(!out.contains("map"), "show_map=false should hide the map pane:\n{out}");
    }
}
