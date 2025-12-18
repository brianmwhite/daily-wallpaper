use crate::{fetch_bing_candidate, Result, Settings, WallpaperCandidate, WallpaperSource};

use super::{FetchResult, Source, SourceContext};

pub struct BingSource;

impl Source for BingSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Bing
    }

    fn label(&self) -> &'static str {
        "Bing"
    }

    fn description(&self) -> &'static str {
        "Bing daily wallpaper"
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let fetched = fetch_bing_candidate(
            ctx.client,
            ctx.cache,
            ctx.settings,
            ctx.date_label,
            ctx.resolutions.to_vec(),
        )?;
        Ok(FetchResult::single(
            fetched.candidate,
            fetched.skipped_download,
        ))
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _settings: &Settings,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| crate::WallpaperError::Message("No Bing candidates found.".into()))
    }
}
