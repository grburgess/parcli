use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::{App, Effect};
use crate::poller::{PollCommand, PollResult};
use crate::store::Paths;
use crate::ui;

/// Run the dashboard until the user quits. Restores the terminal on exit and on panic.
pub async fn run(
    mut app: App,
    paths: Paths,
    cmd_tx: mpsc::Sender<PollCommand>,
    mut results: mpsc::Receiver<PollResult>,
) -> Result<App> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));
    let mut terminal = ratatui::init();
    let outcome = event_loop(&mut terminal, &mut app, &paths, &cmd_tx, &mut results).await;
    ratatui::restore();
    outcome.map(|_| app)
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    cmd_tx: &mpsc::Sender<PollCommand>,
    results: &mut mpsc::Receiver<PollResult>,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut spinner = 0usize;
    loop {
        terminal.draw(|f| ui::draw(f, app, Utc::now(), spinner))?;
        let effects = tokio::select! {
            _ = tick.tick() => { spinner = spinner.wrapping_add(1); vec![] }
            Some(r) = results.recv() => app.apply_poll_result(r, Utc::now()),
            Some(ev) = events.next() => match ev.context("reading terminal events")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key, Utc::now()),
                _ => vec![],
            },
        };
        for effect in effects {
            match effect {
                Effect::SaveParcels => {
                    if let Err(e) = app.parcels.save(&paths.parcels) {
                        app.last_error = Some(format!("saving parcels: {e:#}"));
                    }
                }
                Effect::SaveState => {
                    if let Err(e) = app.cache.save(&paths.state) {
                        app.last_error = Some(format!("saving state: {e:#}"));
                    }
                }
                Effect::Send(cmd) => {
                    if cmd_tx.send(cmd).await.is_err() {
                        app.last_error = Some("poller stopped".into());
                    }
                }
                Effect::Quit => return Ok(()),
            }
        }
    }
}
