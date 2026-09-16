mod app;
mod geo;
mod journey;
mod poller;
mod provider;
mod store;
mod translate;
mod tui;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use tokio::sync::mpsc;

use crate::app::App;
use crate::geo::{Geocoder, NominatimGeocoder};
use crate::poller::{run_poller, seed_translation_cache, PollCommand, Scheduler};
use crate::provider::parcelsapp::ParcelsAppProvider;
use crate::store::{ParcelList, Paths, StateCache};
use crate::translate::{MyMemoryTranslator, Translator};

/// top-like terminal dashboard for international parcel tracking
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Minutes between polls of each parcel
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=1440))]
    interval: u64,
    /// Seconds to wait for parcelsapp before giving up on one poll
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(5..=600))]
    timeout: u64,
    /// Show carrier text as-is instead of translating to English
    #[arg(long)]
    no_translate: bool,
    /// Set your home address (used for the map and journey); saved to parcels.toml
    #[arg(long, value_name = "ADDRESS")]
    home: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let interval = Duration::from_secs(args.interval * 60);

    let paths = Paths::discover()?;
    let mut parcels = ParcelList::load(&paths.parcels)?;
    let cache = StateCache::load(&paths.state)?;

    if let Some(home) = args.home {
        parcels.home = if home.trim().is_empty() { None } else { Some(home) };
        parcels.save(&paths.parcels)?;
    }

    let provider = ParcelsAppProvider::launch(Duration::from_secs(args.timeout))
        .await
        .context("starting headless browser")?;
    let provider: Arc<ParcelsAppProvider> = Arc::new(provider);

    // Resume the cached schedule so a restart does not re-poll everything at once.
    let now = Utc::now();
    let mut scheduler = Scheduler::new(interval, vec![], now);
    for p in &parcels.parcels {
        scheduler.add(&p.number, now);
        if let Some(state) = cache.by_number.get(&p.number) {
            if let Some(t) = &state.tracking {
                scheduler.record_success(&p.number, t.status, state.next_poll - chrono::Duration::from_std(interval)?);
            }
        }
    }

    let translator: Option<Arc<dyn Translator>> = if args.no_translate { None } else { Some(Arc::new(MyMemoryTranslator::new()?)) };
    let translations = seed_translation_cache(&cache);
    // Geocoding misses are cached only for the lifetime of a run: a location
    // that Nominatim could not resolve gets one fresh attempt per start.
    let mut geo = cache.geo.clone();
    geo.retain(|_, v| v.is_some());
    let geocoder: Arc<dyn Geocoder> = Arc::new(NominatimGeocoder::new()?);

    let (cmd_tx, cmd_rx) = mpsc::channel::<PollCommand>(32);
    let (res_tx, res_rx) = mpsc::channel(32);
    let worker = tokio::spawn(run_poller(
        provider.clone(),
        translator,
        translations,
        Some(geocoder),
        geo,
        parcels.home.clone(),
        scheduler,
        cmd_rx,
        res_tx,
    ));

    let app = App::new(parcels, cache, interval);
    let outcome = tui::run(app, paths, cmd_tx.clone(), res_rx).await;

    let _ = cmd_tx.send(PollCommand::Shutdown).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), worker).await;
    if let Ok(provider) = Arc::try_unwrap(provider) {
        provider.close().await;
    }
    outcome.map(|_| ())
}
