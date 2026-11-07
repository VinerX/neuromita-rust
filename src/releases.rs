use std::{collections::HashSet, future::Future, pin::Pin, time::Duration};
use tokio::sync::OnceCell;

use serde::Deserialize;

const REPOSITORY: &str = "Atm4x/NeuroMita";
const MANIFEST_URL: &str =
    "https://github.com/Atm4x/NeuroMita/releases/latest/download/releases-manifest.json";

#[derive(Clone, Debug)]
pub struct ReleaseRecord {
    pub id: String,
    pub title: String,
    pub date: String,
    pub summary: String,
    pub details: String,
    pub url: String,
    pub prerelease: bool,
}

#[derive(Clone, Deserialize)]
struct GithubAsset {
    name: String,
}

#[derive(Clone, Deserialize)]
struct GithubRelease {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    tag_name: String,
    #[serde(default)]
    release_tag: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct ReleaseManifest {
    repository: String,
    releases: Vec<GithubRelease>,
}

pub type ReleaseFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ReleaseRecord>, String>> + Send + 'a>>;

pub trait ReleaseFeed: Send + Sync {
    fn fetch(&self) -> ReleaseFuture<'_>;
}

#[derive(Default)]
pub struct GithubReleaseFeed {
    client: OnceCell<reqwest::Client>,
}

impl ReleaseFeed for GithubReleaseFeed {
    fn fetch(&self) -> ReleaseFuture<'_> {
        Box::pin(async move {
            let client = self
                .client
                .get_or_try_init(|| async {
                    reqwest::Client::builder()
                        .user_agent("NeuroMita-Rust/0.1 release-feed")
                        .timeout(Duration::from_secs(12))
                        .build()
                })
                .await
                .map_err(|error| error.to_string())?;
            fetch_releases(client)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

async fn fetch_releases(client: &reqwest::Client) -> Result<Vec<ReleaseRecord>, reqwest::Error> {
    let manifest = client.get(MANIFEST_URL).send().await;
    let payload = match manifest {
        Ok(response) if response.status().is_success() => {
            match response.json::<ReleaseManifest>().await {
                Ok(manifest) if manifest.repository == REPOSITORY => Some(manifest.releases),
                _ => None,
            }
        }
        _ => None,
    };
    let releases = match payload {
        Some(releases) => releases,
        None => {
            let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=100");
            client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<GithubRelease>>()
                .await?
        }
    };
    Ok(project_releases(releases))
}

fn project_releases(releases: Vec<GithubRelease>) -> Vec<ReleaseRecord> {
    let mut seen = HashSet::new();
    let mut cards = Vec::new();
    for release in releases {
        if release.draft || !has_launcher_assets(&release) {
            continue;
        }
        let tag = release.release_tag.as_deref().unwrap_or(&release.tag_name);
        if !seen.insert(tag.to_string()) {
            continue;
        }
        let details = normalize_body(release.body.as_deref().unwrap_or_default());
        let url = if release.html_url.is_empty() {
            format!("https://github.com/{REPOSITORY}/releases/tag/{tag}")
        } else {
            release.html_url
        };
        let summary = summarize_body(&details);
        cards.push(ReleaseRecord {
            id: release
                .id
                .map_or_else(|| tag.to_string(), |id| id.to_string()),
            title: release
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("NeuroMita {tag}")),
            date: release
                .published_at
                .unwrap_or_default()
                .chars()
                .take(10)
                .collect(),
            summary,
            details,
            url,
            prerelease: release.prerelease,
        });
    }
    cards.sort_by(|a, b| b.date.cmp(&a.date));
    cards
}

fn has_launcher_assets(release: &GithubRelease) -> bool {
    release.assets.iter().any(|asset| {
        let name = asset.name.to_ascii_lowercase();
        let archive = name.ends_with(".zip") || name.ends_with(".7z") || name.ends_with(".rar");
        archive && (name.contains("pythonbuild") || name.contains("unitybuild"))
    })
}

fn normalize_body(body: &str) -> String {
    body.lines()
        .map(|line| {
            let mut line = line.trim();
            while line.starts_with('#') {
                line = &line[1..];
            }
            line = line.trim();
            if line.starts_with("```") {
                return String::new();
            }
            line.replace("**", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn summarize_body(body: &str) -> String {
    let mut meaningful = body
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("Изменения")
                && !line.starts_with("Changes")
                && !line.starts_with("Установка:")
                && !line.starts_with("Installation:")
        })
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    if meaningful.is_empty() {
        return String::new();
    }
    if meaningful.chars().count() > 280 {
        meaningful = meaningful.chars().take(277).collect::<String>();
        if let Some(index) = meaningful.rfind(' ') {
            meaningful.truncate(index);
        }
        meaningful.push_str(" …");
    } else if body.lines().filter(|line| !line.trim().is_empty()).count() > 3 {
        meaningful.push_str(" …");
    }
    meaningful
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_uses_valid_unicode_boundaries() {
        let input = "Привет ".repeat(100);
        let result = summarize_body(&input);
        assert!(result.chars().count() <= 280);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn markdown_heading_is_cleaned() {
        assert_eq!(
            normalize_body("## Исправления\n\n**Новый** чат"),
            "Исправления\n\nНовый чат"
        );
    }
}
