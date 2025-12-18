use crate::{fetch_spotlight_candidates, Result, Settings, WallpaperCandidate, WallpaperSource};

use super::{FetchResult, Source, SourceContext};

pub struct SpotlightSource;

impl Source for SpotlightSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Spotlight
    }

    fn label(&self) -> &'static str {
        "Spotlight"
    }

    fn description(&self) -> &'static str {
        "Windows Spotlight image feed"
    }

    fn supports_day(&self) -> bool {
        false
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let candidates =
            fetch_spotlight_candidates(ctx.client, ctx.cache, ctx.settings, ctx.date_label)?;
        Ok(FetchResult {
            candidates,
            skipped_download: false,
        })
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        settings: &Settings,
    ) -> Result<&'a WallpaperCandidate> {
        let idx = settings.spotlight_index.saturating_sub(1);
        candidates.get(idx).ok_or_else(|| {
            crate::WallpaperError::Message(format!(
                "Requested Spotlight index {} not available; {} images found.",
                settings.spotlight_index,
                candidates.len()
            ))
        })
    }
}
