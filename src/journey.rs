//! The animated journey strip: a 3-line "origin -> home" progress track shown
//! above the event timeline in the detail view.

use std::collections::HashSet;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::geo::{normalize_place, Coord, GeoCache};
use crate::provider::{Status, Tracking};
use crate::ui::status_color;
use crate::ui::theme::{DIM, WARN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    Origin,
    Waypoint,
    Current,
    Home,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub name: String,
    pub coord: Option<Coord>,
    pub kind: StopKind,
}

/// Build the ordered list of journey stops: distinct event locations oldest -> newest
/// (events are stored newest-first in `Tracking`, so we walk them in reverse), then
/// home, if set. Locations are deduped case-insensitively via `normalize_place`, and
/// empty locations are skipped. The last event location is `Current` unless the parcel
/// is `Delivered`, in which case it stays a `Waypoint` and `Home` carries the checkmark.
pub fn journey_stops(tracking: Option<&Tracking>, home: Option<&str>, geo: &GeoCache) -> Vec<Stop> {
    let mut stops = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(t) = tracking {
        for event in t.events.iter().rev() {
            let Some(raw) = event.location.as_deref() else { continue };
            let normalized = normalize_place(raw);
            if normalized.is_empty() {
                continue;
            }
            if !seen.insert(normalized.to_lowercase()) {
                continue;
            }
            let coord = geo.get(&normalized).cloned().flatten();
            let kind = if stops.is_empty() { StopKind::Origin } else { StopKind::Waypoint };
            stops.push(Stop { name: normalized, coord, kind });
        }
    }

    let delivered = tracking.map(|t| t.status) == Some(Status::Delivered);
    if let Some(last) = stops.last_mut() {
        last.kind = if delivered { StopKind::Waypoint } else { StopKind::Current };
    }

    if let Some(h) = home {
        // Raw key (not normalized): matches how the poller caches the home geocode.
        let coord = geo.get(h).cloned().flatten();
        stops.push(Stop { name: h.to_string(), coord, kind: StopKind::Home });
    }

    stops
}

fn glyph_char(kind: StopKind, delivered: bool) -> char {
    match kind {
        StopKind::Origin | StopKind::Waypoint => '●',
        StopKind::Current => '◉',
        StopKind::Home => {
            if delivered {
                '✔'
            } else {
                '○'
            }
        }
    }
}

fn sub_label(kind: StopKind) -> &'static str {
    match kind {
        StopKind::Origin => "origin",
        StopKind::Current => "now",
        StopKind::Home => "home",
        StopKind::Waypoint => "",
    }
}

/// Truncate `name` to at most `max_len` chars, appending `…` when cut.
pub(crate) fn truncate_name(name: &str, max_len: usize) -> String {
    if name.chars().count() <= max_len {
        return name.to_string();
    }
    let keep = max_len.saturating_sub(1).max(1);
    let mut out: String = name.chars().take(keep).collect();
    out.push('…');
    out
}

/// Write `text` into `row`, centered on column `col`, clamped to stay in bounds.
fn place_centered(row: &mut [(char, Option<Color>)], col: usize, text: &str, color: Option<Color>) {
    let w = row.len();
    let text_width = text.chars().count();
    if w == 0 || text_width == 0 {
        return;
    }
    let start = if text_width >= w { 0 } else { col.saturating_sub(text_width / 2).min(w - text_width) };
    for (i, ch) in text.chars().enumerate() {
        if let Some(cell) = row.get_mut(start + i) {
            *cell = (ch, color);
        }
    }
}

/// Group a row of colored cells into spans, merging consecutive cells of the same color.
fn cells_to_line(cells: Vec<(char, Option<Color>)>) -> Line<'static> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut cur_color: Option<Color> = None;
    for (ch, color) in cells {
        if !buf.is_empty() && color != cur_color {
            spans.push(span_for(std::mem::take(&mut buf), cur_color));
        }
        cur_color = color;
        buf.push(ch);
    }
    if !buf.is_empty() {
        spans.push(span_for(buf, cur_color));
    }
    Line::from(spans)
}

fn span_for(text: String, color: Option<Color>) -> Span<'static> {
    match color {
        Some(c) => Span::styled(text, Style::default().fg(c)),
        None => Span::raw(text),
    }
}

/// Render the 3-line journey strip: stop names, the track (glyphs + fill), and
/// sub-labels. Glyph positions are spread evenly across `width`. Segments up to and
/// including the current stop are solid (`━`, status color); segments after it are
/// dashed (`┄`, dim) with a `▸` pulse advancing with `tick`. Delivered parcels render
/// fully solid with a `✔` at home; an Exception colors the current glyph red (via
/// `status_color`).
pub fn render_strip(stops: &[Stop], status: Status, tick: usize, width: u16) -> Vec<Line<'static>> {
    if stops.is_empty() {
        return vec![Line::from(Span::styled("no locations yet", Style::default().fg(DIM)))];
    }

    let w = (width as usize).max(1);
    let n = stops.len();
    let delivered = status == Status::Delivered;
    let only_home = n == 1 && stops[0].kind == StopKind::Home;

    let positions: Vec<usize> =
        if n == 1 { vec![0] } else { (0..n).map(|i| i * w.saturating_sub(1) / (n - 1)).collect() };

    let filled_upto = if delivered {
        n.saturating_sub(1)
    } else {
        stops.iter().position(|s| s.kind == StopKind::Current).unwrap_or(0)
    };

    let mut track: Vec<(char, Option<Color>)> = vec![(' ', None); w];
    let mut names: Vec<(char, Option<Color>)> = vec![(' ', None); w];
    let mut labels: Vec<(char, Option<Color>)> = vec![(' ', None); w];

    for i in 0..n.saturating_sub(1) {
        let start = positions[i] + 1;
        let end = positions[i + 1].min(w);
        let solid = i < filled_upto;
        for cell in track.iter_mut().take(end).skip(start.min(end)) {
            *cell = if solid { ('━', Some(status_color(status))) } else { ('┄', Some(DIM)) };
        }
        if !solid {
            let seg_len = end.saturating_sub(start);
            if seg_len > 0 {
                let pulse_col = start + (tick % seg_len);
                if let Some(cell) = track.get_mut(pulse_col) {
                    *cell = ('▸', Some(WARN));
                }
            }
        }
    }

    let max_name_len = (w / n).saturating_sub(2).max(3);
    for (i, stop) in stops.iter().enumerate() {
        let col = positions[i].min(w.saturating_sub(1));
        let glyph_color = if i <= filled_upto { status_color(status) } else { DIM };
        track[col] = (glyph_char(stop.kind, delivered), Some(glyph_color));

        let name = truncate_name(&stop.name, max_name_len);
        place_centered(&mut names, col, &name, None);

        let label = if only_home { "waiting for first scan" } else { sub_label(stop.kind) };
        if !label.is_empty() {
            place_centered(&mut labels, col, label, Some(DIM));
        }
    }

    vec![cells_to_line(names), cells_to_line(track), cells_to_line(labels)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::TrackEvent;
    use chrono::{TimeZone, Utc};

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    fn event(location: &str) -> TrackEvent {
        TrackEvent { time: Some(now()), description: "moved".into(), location: Some(location.into()), translated: None }
    }

    fn tracking(status: Status, events: Vec<TrackEvent>) -> Tracking {
        Tracking { number: "X".into(), carrier: None, status, events, fetched_at: now(), attributes: vec![], tracking_url: None }
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn strip_text(lines: &[Line]) -> String {
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn stops_dedup_and_order() {
        // Stored newest-first, as `Tracking.events` always is.
        let t = tracking(
            Status::InTransit,
            vec![event("Roissy CDG"), event("shenzhen"), event("Shenzhen")],
        );
        let geo = GeoCache::new();
        let stops = journey_stops(Some(&t), Some("Berlin"), &geo);

        let names: Vec<&str> = stops.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Shenzhen", "Roissy CDG", "Berlin"], "oldest->newest then home, deduped");
        assert_eq!(stops[0].kind, StopKind::Origin);
        assert_eq!(stops[1].kind, StopKind::Current);
        assert_eq!(stops[2].kind, StopKind::Home);
    }

    #[test]
    fn strip_marks_current_and_dashes_remaining() {
        let stops = vec![
            Stop { name: "Shenzhen".into(), coord: None, kind: StopKind::Origin },
            Stop { name: "Roissy".into(), coord: None, kind: StopKind::Current },
            Stop { name: "Berlin".into(), coord: None, kind: StopKind::Home },
        ];
        let lines = render_strip(&stops, Status::InTransit, 0, 40);
        let text = strip_text(&lines);
        assert!(text.contains('◉'), "{text}");
        assert!(text.contains('┄'), "{text}");
        assert!(text.contains('▸'), "{text}");
    }

    #[test]
    fn strip_delivered_is_solid_with_check() {
        let stops = vec![
            Stop { name: "Shenzhen".into(), coord: None, kind: StopKind::Origin },
            Stop { name: "Roissy".into(), coord: None, kind: StopKind::Waypoint },
            Stop { name: "Berlin".into(), coord: None, kind: StopKind::Home },
        ];
        let lines = render_strip(&stops, Status::Delivered, 0, 40);
        let text = strip_text(&lines);
        assert!(text.contains('✔'), "{text}");
        assert!(!text.contains('┄'), "{text}");
    }

    #[test]
    fn strip_pulse_moves_with_tick() {
        let stops = vec![
            Stop { name: "A".into(), coord: None, kind: StopKind::Current },
            Stop { name: "B".into(), coord: None, kind: StopKind::Home },
        ];
        let l0 = strip_text(&render_strip(&stops, Status::InTransit, 0, 40));
        let l1 = strip_text(&render_strip(&stops, Status::InTransit, 1, 40));
        assert_ne!(l0, l1, "pulse position should move between ticks");
    }

    #[test]
    fn strip_handles_narrow_width() {
        let stops = vec![
            Stop { name: "Shenzhen".into(), coord: None, kind: StopKind::Origin },
            Stop { name: "Roissy CDG".into(), coord: None, kind: StopKind::Current },
            Stop { name: "Berlin".into(), coord: None, kind: StopKind::Home },
        ];
        let lines = render_strip(&stops, Status::InTransit, 0, 20);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(line.width() <= 20, "{line:?}");
        }
    }

    #[test]
    fn empty_stops_render_single_hint_line() {
        let lines = render_strip(&[], Status::Unknown, 0, 40);
        assert_eq!(lines.len(), 1);
        assert!(strip_text(&lines).contains("no locations yet"));
    }
}
