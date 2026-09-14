# parcli

A `top`-like terminal dashboard for international parcel tracking. It drives the
free [parcelsapp.com](https://parcelsapp.com) widget in headless Chrome, so no
API key is needed.

## Requirements

- Rust stable (1.80+)
- Google Chrome or Chromium. If it is not found automatically, set
  `PARCLI_CHROME=/path/to/chrome`.
- parcelsapp.com blocks Chrome's default headless user agent; parcli masks it automatically, so no configuration is needed — but if polls start failing with `NO_DATA`, the site has changed its detection.

## Usage

    cargo install --path .
    parcli                 # poll every 10 minutes
    parcli --interval 30   # poll every 30 minutes
    parcli --timeout 120   # seconds to wait for parcelsapp per poll (default 90)

| Key | Action |
|---|---|
| `a` | add a tracking number (optionally followed by a label: `RB123456789CN camera`) |
| `d` | remove the selected parcel (confirms with `y`/`n`) |
| `r` / `R` | refresh selected / all |
| `Enter` | show event history; `q`/`Esc` to go back |
| `j`/`k`, arrows | move |
| `q` | quit |

Delivered parcels stop being polled. Failed polls back off exponentially up to
an hour.

## Files

- `~/Library/Application Support/parcli/parcels.toml` (Linux: `~/.config/parcli/`) — your list
- `~/Library/Caches/parcli/state.json` (Linux: `~/.cache/parcli/`) — last known results

## Development

    cargo test                               # unit tests, no browser needed
    cargo test live_ -- --ignored --nocapture  # one real poll against parcelsapp (needs network, takes 10–60 s)

Design: `docs/superpowers/specs/2026-09-13-parcli-design.md`.
