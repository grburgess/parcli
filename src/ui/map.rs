//! The world-map pane: a Braille canvas plotting journey stops (origin, waypoints,
//! current position, home) with lines between consecutive geocoded stops.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Map, MapResolution};
use ratatui::widgets::{Block, BorderType};
use ratatui::Frame;

use crate::geo::Coord;
use crate::journey::{Stop, StopKind};
use crate::provider::Status;

use super::{status_color, theme};

const MIN_LON_SPAN: f64 = 10.0;
const MIN_LAT_SPAN: f64 = 6.0;
const PAD_FRAC: f64 = 0.15;

/// Bounding box (x = longitude, y = latitude) for a canvas covering `coords`,
/// padded 15% on each side and enforcing a minimum span of 10° lon x 6° lat,
/// clamped to valid lon/lat ranges. Empty input falls back to a world view.
pub fn map_bounds(coords: &[Coord]) -> ([f64; 2], [f64; 2]) {
    if coords.is_empty() {
        return ([-180.0, 180.0], [-60.0, 85.0]);
    }

    let lons: Vec<f64> = coords.iter().map(|&(_, lon)| lon).collect();
    let lats: Vec<f64> = coords.iter().map(|&(lat, _)| lat).collect();
    let (lon_min, lon_max) = (lons.iter().cloned().fold(f64::INFINITY, f64::min), lons.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    let (lat_min, lat_max) = (lats.iter().cloned().fold(f64::INFINITY, f64::min), lats.iter().cloned().fold(f64::NEG_INFINITY, f64::max));

    let lon_center = (lon_min + lon_max) / 2.0;
    let lat_center = (lat_min + lat_max) / 2.0;

    let lon_span = ((lon_max - lon_min) * (1.0 + 2.0 * PAD_FRAC)).max(MIN_LON_SPAN);
    let lat_span = ((lat_max - lat_min) * (1.0 + 2.0 * PAD_FRAC)).max(MIN_LAT_SPAN);

    let x = [(lon_center - lon_span / 2.0).clamp(-180.0, 180.0), (lon_center + lon_span / 2.0).clamp(-180.0, 180.0)];
    let y = [(lat_center - lat_span / 2.0).clamp(-90.0, 90.0), (lat_center + lat_span / 2.0).clamp(-90.0, 90.0)];
    (x, y)
}

fn pin_glyph(kind: StopKind, tick: usize) -> (&'static str, ratatui::style::Color) {
    match kind {
        StopKind::Origin => ("●", theme::ACCENT),
        StopKind::Waypoint => ("•", theme::DIM),
        StopKind::Current => {
            let glyph = if tick % 2 == 0 { "◉" } else { "○" };
            (glyph, theme::ACCENT)
        }
        StopKind::Home => ("⌂", ratatui::style::Color::Green),
    }
}

/// Draw the map pane into `area`: a Braille-marker world map with lines between
/// consecutive geocoded stops (in `status_color`) and pins labeled with stop names.
pub fn draw_map(frame: &mut Frame, area: Rect, stops: &[Stop], status: Status, tick: usize) {
    let border_style = Style::default().fg(status_color(status));
    let block = Block::bordered().border_type(BorderType::Rounded).title(" map ").border_style(border_style);

    let coords: Vec<Coord> = stops.iter().filter_map(|s| s.coord).collect();
    let (x_bounds, y_bounds) = map_bounds(&coords);
    let line_color = status_color(status);
    let stops = stops.to_vec();

    let canvas = Canvas::default()
        .block(block)
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(move |ctx| {
            ctx.draw(&Map { resolution: MapResolution::High, color: theme::DIM });

            if coords.is_empty() {
                ctx.layer();
                ctx.print(0.0, 0.0, Span::styled("no locations yet", Style::default().fg(theme::DIM)));
                return;
            }

            let mut prev: Option<Coord> = None;
            for stop in &stops {
                if let Some((lat, lon)) = stop.coord {
                    if let Some((plat, plon)) = prev {
                        ctx.draw(&CanvasLine { x1: plon, y1: plat, x2: lon, y2: lat, color: line_color });
                    }
                    prev = Some((lat, lon));
                }
            }

            ctx.layer();
            for stop in &stops {
                let Some((lat, lon)) = stop.coord else { continue };
                let (glyph, color) = pin_glyph(stop.kind, tick);
                let name = truncate(&stop.name, 12);
                ctx.print(lon, lat, Span::styled(format!("{glyph} {name}"), Style::default().fg(color)));
            }
        });

    frame.render_widget(canvas, area);
}

/// Truncate `name` to at most `max_len` chars, appending `…` when cut.
fn truncate(name: &str, max_len: usize) -> String {
    if name.chars().count() <= max_len {
        return name.to_string();
    }
    let keep = max_len.saturating_sub(1).max(1);
    let mut out: String = name.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_bounds_pads_and_enforces_min_span() {
        // Single coord: min span, centered on it.
        let (x, y) = map_bounds(&[(22.5, 114.0)]);
        assert_eq!(x, [114.0 - 5.0, 114.0 + 5.0]);
        assert_eq!(y, [22.5 - 3.0, 22.5 + 3.0]);

        // Two far-apart coords: padded bbox, span exceeds the minimum.
        let (x, y) = map_bounds(&[(22.5, 114.0), (49.0, 2.5)]);
        let lon_span = 114.0 - 2.5;
        let lat_span = 49.0 - 22.5;
        let expected_lon_span = lon_span * 1.3;
        let expected_lat_span = lat_span * 1.3;
        let lon_center = (114.0 + 2.5) / 2.0;
        let lat_center = (49.0 + 22.5) / 2.0;
        assert!((x[1] - x[0] - expected_lon_span).abs() < 1e-9, "{x:?}");
        assert!((y[1] - y[0] - expected_lat_span).abs() < 1e-9, "{y:?}");
        assert!((((x[0] + x[1]) / 2.0) - lon_center).abs() < 1e-9);
        assert!((((y[0] + y[1]) / 2.0) - lat_center).abs() < 1e-9);

        // Empty: world fallback.
        assert_eq!(map_bounds(&[]), ([-180.0, 180.0], [-60.0, 85.0]));
    }

    #[test]
    fn map_bounds_clamps_at_poles() {
        let (x, y) = map_bounds(&[(89.9, 179.9), (89.9, 179.9)]);
        assert!(x[1] <= 180.0 && x[0] >= -180.0, "{x:?}");
        assert!(y[1] <= 90.0 && y[0] >= -90.0, "{y:?}");
        assert_eq!(y[1], 90.0, "north pole should clamp to 90");

        let (x, y) = map_bounds(&[(-89.9, -179.9)]);
        assert!(x[0] >= -180.0, "{x:?}");
        assert_eq!(y[0], -90.0, "south pole should clamp to -90");
    }
}
