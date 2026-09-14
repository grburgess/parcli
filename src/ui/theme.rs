//! Shared palette and small styling helpers for the TUI.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Primary accent: table title, header band, footer key letters.
pub const ACCENT: Color = Color::Cyan;
/// Fallback carrier color, used only when a carrier name is empty.
pub const CARRIER: Color = Color::Magenta;
/// Warning accent (amber). Not yet applied anywhere; reserved for a future warning state.
#[allow(dead_code)]
pub const WARN: Color = Color::Indexed(214);
/// Muted text for secondary or inactive content.
pub const DIM: Color = Color::DarkGray;
/// Background for alternating ("zebra") table rows.
pub const ZEBRA: Color = Color::Indexed(235);
/// Background for the last-error indicator.
pub const ERROR_BG: Color = Color::Indexed(52);

const CARRIER_PALETTE: [Color; 8] = [
    Color::Magenta,
    Color::Blue,
    Color::Green,
    Color::Cyan,
    Color::Yellow,
    Color::LightMagenta,
    Color::LightBlue,
    Color::LightGreen,
];

/// FNV-1a (64-bit) over raw bytes.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, b| (hash ^ *b as u64).wrapping_mul(PRIME))
}

/// A stable color for a carrier name: the same name always maps to the same
/// color, spread across an 8-color palette by a hash of the name.
pub fn carrier_color(name: &str) -> Color {
    let index = (fnv1a(name.as_bytes()) % CARRIER_PALETTE.len() as u64) as usize;
    CARRIER_PALETTE[index]
}

/// A footer key hint: the key in bold accent, the label dimmed (e.g. `a` `add`).
pub fn key_hint(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(key.to_string(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {label}"), Style::default().fg(DIM)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrier_color_is_stable_and_distinct_for_samples() {
        assert_eq!(carrier_color("DHL"), carrier_color("DHL"), "same name must be stable");
        assert_eq!(carrier_color("China Post"), carrier_color("China Post"));

        let dhl = carrier_color("DHL");
        let china_post = carrier_color("China Post");
        let chronopost = carrier_color("Chronopost France");
        assert_ne!(dhl, china_post, "DHL and China Post must not collide");
        assert_ne!(dhl, chronopost, "DHL and Chronopost France must not collide");
        assert_ne!(china_post, chronopost, "China Post and Chronopost France must not collide");
    }

    #[test]
    fn key_hint_bolds_the_key_and_dims_the_label() {
        let spans = key_hint("a", "add");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "a");
        assert_eq!(spans[0].style.fg, Some(ACCENT));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " add");
        assert_eq!(spans[1].style.fg, Some(DIM));
    }
}
