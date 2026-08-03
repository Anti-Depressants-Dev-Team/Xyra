use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    YouTube,
    Odysee,
}

impl Platform {
    pub const ALL: [Self; 2] = [Self::YouTube, Self::Odysee];

    pub fn label(self) -> &'static str {
        match self {
            Self::YouTube => "YouTube",
            Self::Odysee => "Odysee",
        }
    }

    pub fn supported_visibilities(self) -> &'static [Visibility] {
        match self {
            Self::YouTube => &[
                Visibility::Public,
                Visibility::Unlisted,
                Visibility::Private,
            ],
            // Odysee supports link-accessible unlisted publications, but does not
            // expose a YouTube-equivalent private visibility for ordinary uploads.
            Self::Odysee => &[Visibility::Public, Visibility::Unlisted],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
}

impl Visibility {
    pub fn label(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Unlisted => "Unlisted",
            Self::Private => "Private",
        }
    }

    pub fn youtube_value(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub path: PathBuf,
    pub title: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub duration_secs: f32,
}

impl Clip {
    pub fn new(path: PathBuf, duration_secs: f32) -> Self {
        let created_at = Utc::now();
        Self {
            id: Uuid::new_v4(),
            path,
            title: format!("Xyra clip {}", created_at.format("%Y-%m-%d %H:%M")),
            description: String::new(),
            created_at,
            duration_secs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditProject {
    pub clip_id: Uuid,
    pub trim_start_secs: f32,
    pub trim_end_secs: f32,
}

impl EditProject {
    pub fn for_clip(clip: &Clip) -> Self {
        Self {
            clip_id: clip.id,
            trim_start_secs: 0.0,
            trim_end_secs: clip.duration_secs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTarget {
    pub platform: Platform,
    pub enabled: bool,
    pub visibility: Visibility,
}

impl PublishTarget {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            enabled: false,
            visibility: Visibility::Public,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishJob {
    pub id: Uuid,
    pub clip_id: Uuid,
    pub title: String,
    pub description: String,
    pub targets: Vec<PublishTarget>,
    pub created_at: DateTime<Utc>,
    pub status: PublishStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PublishStatus {
    Queued,
    Uploading { platform: Platform, progress: f32 },
    Complete,
    Failed(String),
}
