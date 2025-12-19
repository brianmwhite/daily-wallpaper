use crate::{log, write_bytes_atomic, WallpaperCandidate, WallpaperError, WallpaperSource};
use image;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const METADATA_EXT: &str = "json";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FavoriteEntry {
    pub id: String,
    pub source: WallpaperSource,
    pub title: Option<String>,
    pub description: Option<String>,
    pub attribution: Option<String>,
    pub info_url: Option<String>,
    pub original_url: Option<String>,
    pub date: String,
    pub resolution: Option<String>,
    pub checksum: Option<String>,
    pub metadata_xml: Option<String>,
    pub stored_filename: String,
    pub image_path: PathBuf,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FavoriteFile {
    id: String,
    source: WallpaperSource,
    title: Option<String>,
    description: Option<String>,
    attribution: Option<String>,
    info_url: Option<String>,
    original_url: Option<String>,
    date: String,
    resolution: Option<String>,
    checksum: Option<String>,
    metadata_xml: Option<String>,
    stored_filename: String,
    created_at: u64,
}

pub struct FavoritesManager {
    base_dir: PathBuf,
}

impl FavoritesManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn ensure_dir(&self) -> Result<(), WallpaperError> {
        fs::create_dir_all(&self.base_dir)?;
        Ok(())
    }

    pub fn is_favorited(&self, candidate_id: &str) -> Result<bool, WallpaperError> {
        let entries = self.load_all()?;
        Ok(entries.iter().any(|f| f.id == candidate_id))
    }

    pub fn load_all(&self) -> Result<Vec<FavoriteEntry>, WallpaperError> {
        let mut result = Vec::new();
        if !self.base_dir.exists() {
            return Ok(result);
        }
        let mut seen: HashMap<String, ()> = HashMap::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(METADATA_EXT) {
                continue;
            }
            let data = fs::read(&path)?;
            let parsed: FavoriteFile = match serde_json::from_slice(&data) {
                Ok(v) => v,
                Err(err) => {
                    log(
                        &format!(
                            "Skipping corrupt favorite metadata {}: {err}",
                            path.display()
                        ),
                        false,
                    );
                    continue;
                }
            };
            let img_path = self.base_dir.join(&parsed.stored_filename);
            if !img_path.exists() {
                log(
                    &format!(
                        "Skipping favorite {} because image is missing: {}",
                        parsed.id,
                        img_path.display()
                    ),
                    false,
                );
                continue;
            }
            if seen.contains_key(&parsed.id) {
                log(
                    &format!("Skipping duplicate favorite id {}; keeping first.", parsed.id),
                    false,
                );
                continue;
            }
            seen.insert(parsed.id.clone(), ());
            result.push(FavoriteEntry {
                id: parsed.id,
                source: parsed.source,
                title: parsed.title,
                description: parsed.description,
                attribution: parsed.attribution,
                info_url: parsed.info_url,
                original_url: parsed.original_url,
                date: parsed.date,
                resolution: parsed.resolution,
                checksum: parsed.checksum,
                metadata_xml: parsed.metadata_xml,
                stored_filename: parsed.stored_filename,
                image_path: img_path,
                created_at: parsed.created_at,
            });
        }
        Ok(result)
    }

    pub fn save_favorite(
        &self,
        candidate: &WallpaperCandidate,
    ) -> Result<FavoriteEntry, WallpaperError> {
        self.ensure_dir()?;
        if self.is_favorited(&candidate.id)? {
            return Err(WallpaperError::Message(format!(
                "Candidate {} is already in favorites.",
                candidate.id
            )));
        }

        let base_name = build_favorite_basename(candidate);
        let mut unique_name = base_name.clone();
        let mut counter = 1;
        loop {
            let candidate_image = self
                .base_dir
                .join(format!("{unique_name}{}", image_extension(&candidate.local_path)));
            let candidate_meta = self
                .base_dir
                .join(format!("{unique_name}.{}", METADATA_EXT));
            if !candidate_image.exists() && !candidate_meta.exists() {
                break;
            }
            counter += 1;
            unique_name = format!("{base_name}-{counter}");
        }

        let stored_filename =
            format!("{unique_name}{}", image_extension(&candidate.local_path));
        let target_path = self.base_dir.join(&stored_filename);
        fs::copy(&candidate.local_path, &target_path)?;

        let resolution = detect_resolution(&target_path)?;
        let checksum = compute_checksum(&target_path)?;
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let meta = FavoriteFile {
            id: candidate.id.clone(),
            source: candidate.source,
            title: candidate.title.clone(),
            description: candidate.description.clone(),
            attribution: candidate.attribution.clone(),
            info_url: candidate.info_url.clone(),
            original_url: Some(candidate.image_url.clone()),
            date: candidate.date.clone(),
            resolution: resolution.clone(),
            checksum,
            metadata_xml: candidate.metadata_xml.clone(),
            stored_filename: stored_filename.clone(),
            created_at,
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta)?;
        let meta_path = self.base_dir.join(format!("{unique_name}.{METADATA_EXT}"));
        write_bytes_atomic(&meta_path, &meta_bytes)?;

        Ok(FavoriteEntry {
            id: meta.id,
            source: meta.source,
            title: meta.title,
            description: meta.description,
            attribution: meta.attribution,
            info_url: meta.info_url,
            original_url: meta.original_url,
            date: meta.date,
            resolution: meta.resolution,
            checksum: meta.checksum,
            metadata_xml: meta.metadata_xml,
            stored_filename,
            image_path: target_path,
            created_at,
        })
    }

    pub fn remove(&self, entry: &FavoriteEntry) -> Result<(), WallpaperError> {
        let meta_path = self
            .base_dir
            .join(Path::new(&entry.stored_filename).with_extension(METADATA_EXT));
        let _ = fs::remove_file(&entry.image_path);
        let _ = fs::remove_file(meta_path);
        Ok(())
    }
}

fn build_favorite_basename(candidate: &WallpaperCandidate) -> String {
    let mut title = candidate
        .title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&candidate.id)
        .to_string();
    if title.is_empty() {
        title = candidate.id.clone();
    }
    let sanitized_title = sanitize_component(&title);
    let date = sanitize_component(&candidate.date);
    let source = sanitize_component(crate::source_dir_name(candidate.source));
    let joined = format!("{source}-{date}-{sanitized_title}");
    if joined.trim_matches('-').is_empty() {
        "favorite".to_string()
    } else {
        joined
    }
}

fn sanitize_component(input: &str) -> String {
    let mut cleaned: String = input
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while cleaned.contains("--") {
        cleaned = cleaned.replace("--", "-");
    }
    cleaned.trim_matches('-').to_lowercase()
}

fn image_extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| format!(".{}", ext))
        .unwrap_or_else(|| ".jpg".to_string())
}

fn detect_resolution(path: &Path) -> Result<Option<String>, WallpaperError> {
    match image::image_dimensions(path) {
        Ok((w, h)) => Ok(Some(format!("{}x{}", w, h))),
        Err(_) => Ok(None),
    }
}

fn compute_checksum(path: &Path) -> Result<Option<String>, WallpaperError> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(err.into());
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest = hasher.finalize();
    Ok(Some(format!("{:x}", digest)))
}
