//! The world-map pane: a Braille canvas plotting journey stops (origin, waypoints,
//! current position, home) with lines between consecutive geocoded stops.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Map, MapResolution};
use ratatui::widgets::{Block, BorderType};
use ratatui::Frame;

use crate::geo::Coord;
use crate::journey::{truncate_name, Stop, StopKind};
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

/// Glyph and color for a stop's pin. `Current` is colored by `status_color`
/// (e.g. red for an `Exception`); the others are fixed regardless of status.
fn pin_style(kind: StopKind, status: Status, tick: usize) -> (&'static str, Color) {
    match kind {
        StopKind::Origin => ("●", theme::ACCENT),
        StopKind::Waypoint => ("•", theme::DIM),
        StopKind::Current => {
            let glyph = if tick % 2 == 0 { "◉" } else { "○" };
            (glyph, status_color(status))
        }
        StopKind::Home => ("⌂", Color::Green),
    }
}

/// Where a pin's label should start (in longitude) and whether it is
/// left-anchored (drawn ending at `lon` rather than starting there). Pins in
/// the eastern half of `x_bounds` flip so their label doesn't clip against
/// the canvas's right edge.
fn label_anchor(lon: f64, x_bounds: [f64; 2], label_len: usize, inner_width: u16) -> (f64, bool) {
    let span = (x_bounds[1] - x_bounds[0]).max(f64::EPSILON);
    let frac = (lon - x_bounds[0]) / span;
    let flipped = frac > 0.5;
    if !flipped {
        return (lon, false);
    }
    let cells_per_lon = (inner_width as f64 / span).max(f64::EPSILON);
    let start = lon - (label_len.saturating_sub(1)) as f64 / cells_per_lon;
    (start, true)
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
    let inner_width = block_inner_width(area);

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
                let (glyph, color) = pin_style(stop.kind, status, tick);
                let name = truncate_name(&stop.name, 12);
                let label_len = name.chars().count() + 1 + glyph.chars().count();
                let (start_lon, flipped) = label_anchor(lon, x_bounds, label_len, inner_width);
                let text = if flipped { format!("{name} {glyph}") } else { format!("{glyph} {name}") };
                ctx.print(start_lon, lat, Span::styled(text, Style::default().fg(color)));
            }
        });

    frame.render_widget(canvas, area);
}

/// Width (in cells) of the canvas's paintable area once its border is subtracted.
fn block_inner_width(area: Rect) -> u16 {
    Block::bordered().border_type(BorderType::Rounded).inner(area).width
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

    #[test]
    fn pin_style_colors_current_by_status() {
        let (_, color) = pin_style(StopKind::Current, Status::Exception, 0);
        assert_eq!(color, status_color(Status::Exception));
        assert_eq!(color, Color::Red);

        // Other kinds are unaffected by status.
        let (_, origin_color) = pin_style(StopKind::Origin, Status::Exception, 0);
        assert_eq!(origin_color, theme::ACCENT);
    }

    #[test]
    fn map_label_flips_left_for_eastern_pins() {
        let bounds = [-180.0, 180.0];
        let (_, flipped_east) = label_anchor(-180.0 + 0.8 * 360.0, bounds, 10, 100);
        assert!(flipped_east, "a pin at 80% of the range should flip its label left");

        let (start_west, flipped_west) = label_anchor(-180.0 + 0.2 * 360.0, bounds, 10, 100);
        assert!(!flipped_west, "a pin at 20% of the range should not flip");
        assert_eq!(start_west, -180.0 + 0.2 * 360.0, "unflipped label starts at the pin");
    }
}
