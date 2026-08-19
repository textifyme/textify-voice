//! `textify-voice models` — whisper.cpp model cache management.

use anyhow::{Context, Result};
use clap::Args;

use voice_asr_whisper::{ModelId, ModelManager};

use crate::common::ModelArg;

#[derive(Args, Debug)]
pub struct ModelsArgs {
    /// Download (or confirm already cached) a specific model.
    #[arg(long, value_enum)]
    pub download: Option<ModelArg>,

    /// Print only the cache directory path (useful for scripting).
    #[arg(long)]
    pub path: bool,
}

pub fn run(args: ModelsArgs) -> Result<()> {
    let manager = ModelManager::new().context("resolving the whisper model cache directory")?;

    if args.path {
        println!("{}", manager.cache_dir().display());
        return Ok(());
    }

    if let Some(model_arg) = args.download {
        let id = model_arg.to_model_id();
        if manager.is_cached(id) {
            println!("{} already cached at {}", id.filename(), manager.model_path(id).display());
            return Ok(());
        }
        println!("downloading {} from {} ...", id.filename(), id.url());
        let mut last_pct = u64::MAX;
        manager
            .ensure_downloaded(
                id,
                Some(&mut |downloaded: u64, total: u64| {
                    if total > 0 {
                        let pct = (downloaded * 100) / total;
                        if pct != last_pct {
                            last_pct = pct;
                            eprint!("\r  {pct:3}%  ({downloaded}/{total} bytes)");
                        }
                    }
                }),
            )
            .with_context(|| format!("downloading {}", id.filename()))?;
        println!();
        println!("done: {}", manager.model_path(id).display());
        return Ok(());
    }

    // No flags: status listing.
    println!("model cache directory: {}", manager.cache_dir().display());
    println!();
    for id in [ModelId::TinyEn, ModelId::BaseEn] {
        let (min, max) = id.expected_size_range();
        let status = if manager.is_cached(id) { "cached" } else { "not cached" };
        println!(
            "  {:<20} {:<12} expected size {}-{} MB   {}",
            id.filename(),
            status,
            min / 1_000_000,
            max / 1_000_000,
            id.url()
        );
    }
    println!();
    println!("Download one with: textify-voice models --download tiny.en|base.en");
    Ok(())
}
