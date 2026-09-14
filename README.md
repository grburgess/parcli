# parcli

A `top`-like terminal dashboard for international parcel tracking. It drives the
free [parcelsapp.com](https://parcelsapp.com) widget in headless Chrome, so no
API key is needed.

```
 parcli  2 parcels · ⌂ set · next poll 7m                                          14:00:00
╭ parcels ──────────────────────────────────────────────────────────────────────────────────╮
│LABEL      NUMBER            CARRIER          STATUS        LAST EVENT           AGE  NEXT   │
│camera     RB123456789CN     China Post        in transit   Left the facility    5h   ▰▱▱▱▱ 7m│
│           XT336054383TS     Chronopost France pending      Packages being prep… 1h   ▰▰▱▱▱ 6m│
╰────────────────────────────────────────────────────────────────────────────────────────────╯
 a add · d remove · r refresh · R refresh all · Enter detail · m map · j/k move · q quit
```

## Features

- Add and remove tracking numbers from the keyboard; the list and last-known
  results persist across restarts.
- Sequential polling with per-parcel exponential backoff; delivered parcels stop
  polling. Status pills, staleness-colored event age, and a progress bar toward
  the next poll.
- Detail view: summary card, animated origin → home journey strip, event
  timeline, and a Braille world map with origin / waypoint / current / home pins.
- Carrier messages translated to English (MyMemory), locations geocoded
  (OpenStreetMap Nominatim) — both free, keyless, and cached.

This is a personal tool that automates a public website; it is not affiliated
with parcelsapp.com and may break if the site changes.

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
    parcli --no-translate  # show carrier text as-is (no English translation)
    parcli --home "Musterstraße 1, 10115 Berlin"   # save your address (map + journey destination)

| Key | Action |
|---|---|
| `a` | add a tracking number (optionally followed by a label: `RB123456789CN camera`) |
| `d` | remove the selected parcel (confirms with `y`/`n`) |
| `r` / `R` | refresh selected / all |
| `Enter` | show event history; `q`/`Esc` to go back |
| `m` | toggle the map pane (detail view) |
| `j`/`k`, arrows | move |
| `q` | quit |
| `Ctrl-C` | quit |

Delivered parcels stop being polled. Failed polls back off exponentially up to
an hour.

## Translation

Carrier messages that are not in English are translated to English with the free, anonymous [MyMemory](https://mymemory.translated.net) API (roughly 5,000 characters per day). Each distinct message is looked up once (whether or not it needed translating) and the result is cached in `state.json`, so later polls and restarts cost nothing. The table shows the English text; the detail view shows the original beneath it. Use `--no-translate` to turn this off.

## Journey and map

The detail view (Enter) shows a journey strip — origin ● ━━ waypoints ━━ ◉ current ┄┄ ○ home — with a pulse moving toward home, and (in terminals ≥ 100 columns) a world map with the same pins. Locations come from the carrier's scan events; set your address once with `--home` so the destination pin appears. Press `m` to hide or show the map.

## Privacy

parcli talks to three services: parcelsapp.com (the tracking number, via a headless browser), MyMemory (event text, for translation) and OpenStreetMap Nominatim (place names from scan events and your home address, for the map; at most one request per second, each place looked up once and cached in `state.json`). Nothing else leaves your machine. Use `--no-translate` to skip MyMemory.

## Files

- `~/Library/Application Support/parcli/parcels.toml` (Linux: `~/.config/parcli/`) — your list
- `~/Library/Caches/parcli/state.json` (Linux: `~/.cache/parcli/`) — last known results

## Development

    cargo test                               # unit tests, no browser needed
    cargo test live_ -- --ignored --nocapture  # one real poll against parcelsapp (needs network, takes 10–60 s)

Design: `docs/superpowers/specs/2026-09-13-parcli-design.md`.
