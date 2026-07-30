use serde::Serialize;
use tauri::{AppHandle, Manager};
use ts_rs::TS;

/// Built/installed info for the About page (kit's `getVersionInfo` option).
#[derive(Serialize, TS)]
pub struct VersionInfo {
    pub version: String,
    pub build_date: String,
    pub installed_at: Option<String>,
}

#[tauri::command]
pub async fn get_version_info(app: AppHandle) -> VersionInfo {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let base_date = option_env!("BUILD_DATE").unwrap_or("unknown").to_string();
    let build_date = if base_date == "unknown" {
        base_date
    } else {
        match app.path().app_data_dir().ok() {
            Some(dir) => fetch_build_datetime(&version, &base_date, &dir).await,
            None => base_date,
        }
    };
    let installed_at = load_or_record_install_date(&app, &version);
    VersionInfo {
        version,
        build_date,
        installed_at,
    }
}

/// Returns `"YYYY-MM-DD HH:MM"` for the given version by fetching the GitHub
/// release `published_at` field. Caches the result so only the first call per
/// version hits the network. Falls back to `base_date` (`"YYYY-MM-DD"`) on any
/// error so the UI always shows at least a date.
async fn fetch_build_datetime(version: &str, base_date: &str, data_dir: &std::path::Path) -> String {
    // Local / non-release builds: nothing to fetch.
    if version == "local-build" || version == "unknown" {
        return base_date.to_string();
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    struct BuildTimeCache {
        version: String,
        datetime: String,
    }

    let cache_path = data_dir.join("build-time-cache.json");

    // Cache hit?
    if let Ok(raw) = std::fs::read_to_string(&cache_path) {
        if let Ok(c) = serde_json::from_str::<BuildTimeCache>(&raw) {
            if c.version == version {
                return c.datetime;
            }
        }
    }

    // Fetch from GitHub releases API.
    let url =
        format!("https://api.github.com/repos/SirBepy/server_supervisor/releases/tags/v{version}");
    let result: Option<String> = async {
        #[derive(serde::Deserialize)]
        struct GhRelease {
            published_at: Option<String>,
        }

        let client = reqwest::Client::builder()
            .user_agent("server-supervisor-app")
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .ok()?;
        let resp = client.get(&url).send().await.ok()?;
        let release: GhRelease = resp.json().await.ok()?;
        let iso = release.published_at?;
        // ISO 8601: "2026-06-28T13:35:00Z" -> "2026-06-28 13:35"
        let date_part = iso.get(..10)?;
        let time_part = iso.get(11..16)?;
        Some(format!("{date_part} {time_part}"))
    }
    .await;

    match result {
        Some(datetime) => {
            let cache = BuildTimeCache {
                version: version.to_string(),
                datetime: datetime.clone(),
            };
            if let Ok(json) = serde_json::to_string(&cache) {
                let _ = std::fs::write(&cache_path, json);
            }
            datetime
        }
        None => base_date.to_string(),
    }
}

fn load_or_record_install_date(app: &AppHandle, current_version: &str) -> Option<String> {
    #[derive(serde::Deserialize, serde::Serialize)]
    struct InstallInfo {
        version: String,
        installed_at: String,
    }

    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("install-info.json");

    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(info) = serde_json::from_str::<InstallInfo>(&content) {
            if info.version == current_version {
                return Some(info.installed_at);
            }
        }
    }

    let today = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let info = InstallInfo {
        version: current_version.to_string(),
        installed_at: today.clone(),
    };
    if let Ok(json) = serde_json::to_string(&info) {
        let _ = std::fs::write(&path, json);
    }
    Some(today)
}
