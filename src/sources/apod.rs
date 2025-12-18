use crate::{fetch_apod_candidate, Result, Settings, WallpaperCandidate, WallpaperSource};

use super::{FetchResult, Source, SourceContext};

pub struct ApodSource;

impl Source for ApodSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Apod
    }

    fn label(&self) -> &'static str {
        "APOD"
    }

    fn description(&self) -> &'static str {
        "NASA Astronomy Picture of the Day"
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let candidate = fetch_apod_candidate(ctx.client, ctx.cache, ctx.settings, ctx.date_label)?;
        Ok(FetchResult::single(candidate, false))
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _settings: &Settings,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| crate::WallpaperError::Message("No APOD candidates found.".into()))
    }
}
