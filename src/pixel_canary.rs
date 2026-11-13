//! Pixel Beta/Canary fingerprint + security-patch resolution.
//!
//! This is the Rust port of `osm0sis/PlayIntegrityFork`'s `autopif4.sh`. The
//! pipeline, in the same order as the shell script:
//!
//!   1. `developer.android.com/about/versions` -> the newest beta track page.
//!   2. That page -> the factory-image and OTA download pages; pick whichever
//!      lists more devices.
//!   3. Parse `<tr id="...">` rows into the model / product (`<device>_beta`)
//!      lists.
//!   4. `flash.android.com` -> the `data-client-config` API key.
//!   5. `content-flashstation-pa.googleapis.com/v1/builds?product=&key=` -> the
//!      newest entry with `"canary": true`; take `releaseCandidateName`,
//!      `buildId`, `releaseTrackVersionName` and `factoryImageDownloadUrl`.
//!   6. `HEAD` the factory image -> `Last-Modified` -> release date, and expiry
//!      at release + 6 weeks.
//!   7. `source.android.com/docs/security/bulletin/pixel` -> the security patch
//!      matching the build id, falling back to `<build>-05`.
//!   8. Device -> first API level table for `DEVICE_INITIAL_SDK_INT`.
//!
//! The output is a `PifProfile` plus the resolved patch date, so the fingerprint
//! and `ro.build.version.security_patch` can never disagree.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pif_common::PifProfile;
use serde_json::Value;

use crate::webui_http::{self, DownloadPolicy};

const VERSIONS_URL: &str = "https://developer.android.com/about/versions";
const FLASH_URL: &str = "https://flash.android.com/";
const FLASH_API: &str = "https://content-flashstation-pa.googleapis.com/v1/builds";
const PIXEL_BULLETIN_URL: &str = "https://source.android.com/docs/security/bulletin/pixel";

const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RELEASE_BYTES: usize = 512 * 1024;
const MAX_REDIRECTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CANARY_EXPIRY_WEEKS: u64 = 6;

/// Device -> `ro.product.first_api_level`, copied from `autopif4.sh`.
fn first_api_level(device: &str) -> i32 {
    match device {
        "bluejay" => 32,
        "panther" | "cheetah" | "lynx" | "felix" | "tangorpro" => 33,
        "shiba" | "husky" | "akita" | "tokay" | "caiman" | "komodo" | "comet" => 34,
        "tegu" => 35,
        "frankel" | "blazer" | "mustang" | "rango" => 36,
        "stallion" => 35,
        "cubs" | "grizzly" | "kodiak" | "yogi" => 37,
        _ => 37,
    }
}

#[derive(Debug, Clone)]
pub struct CanaryBuild {
    pub model: String,
    pub product: String,
    pub device: String,
    pub release_candidate: String,
    pub build_id: String,
    pub incremental: String,
    pub android_release: String,
    pub fingerprint: String,
    pub security_patch: String,
    pub device_initial_sdk_int: i32,
    pub released_on: Option<String>,
    pub expires_on: Option<String>,
}

impl CanaryBuild {
    /// Build the stored profile. `security_patch` is the value resolved from the
    /// Pixel bulletin (identical to what the security-patch sync writes).
    pub fn into_profile(self) -> Result<PifProfile> {
        let entry = pif_common::DeviceEntry { model: self.model, product: self.product };
        // The same block `autopif4.sh` writes to pif.prop, so the existing
        // `PifProfile::from_source` parser and validation stay authoritative.
        let prop = format!(
            "MANUFACTURER=Google\nMODEL={}\nFINGERPRINT={}\nPRODUCT={}\nDEVICE={}\nSECURITY_PATCH={}\nDEVICE_INITIAL_SDK_INT={}\n",
            entry.model,
            self.fingerprint,
            entry.product,
            self.device,
            self.security_patch,
            self.device_initial_sdk_int
        );
        PifProfile::from_source(&entry, prop.as_bytes()).map_err(anyhow::Error::msg)
    }
}

fn fetch(url: &str, allow: fn(&str) -> bool, max: usize, label: &'static str) -> Result<String> {
    let uri: ureq::http::Uri = url.parse().with_context(|| format!("{label} URL is invalid"))?;
    if !allow(url) {
        bail!("{label} URL is outside the fixed Pixel/Play source list");
    }
    let policy = DownloadPolicy {
        resource: label,
        redirect_allowlist: "the fixed Pixel/Play source list",
        max_bytes: max,
        max_size_label: "4 MiB",
        max_redirects: MAX_REDIRECTS,
        timeout: REQUEST_TIMEOUT,
        connect_timeout: CONNECT_TIMEOUT,
    };
    webui_http::download_https_utf8(uri, &policy, |uri| {
        uri.scheme_str() == Some("https") && allow(&uri.to_string())
    })
}

fn allowed(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else { return false };
    if uri.scheme_str() != Some("https") {
        return false;
    }
    let Some(authority) = uri.authority() else { return false };
    let authority = authority.as_str();
    if authority.contains('@') {
        return false;
    }
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    matches!(
        host.to_ascii_lowercase().as_str(),
        "developer.android.com"
            | "flash.android.com"
            | "content-flashstation-pa.googleapis.com"
            | "source.android.com"
    )
}

/// Same anchor strategy as `autopif4.sh`: the newest `href="/about/versions/NN"`
/// link, preferring the one marked as a preview.
fn latest_beta_path(html: &str) -> Option<String> {
    let mut preview = Vec::new();
    let mut any = Vec::new();
    let bytes = html.as_bytes();
    let mut index = 0;
    while let Some(found) = html[index..].find("href=\"/about/versions/") {
        let start = index + found + "href=\"".len();
        let Some(end_rel) = html[start..].find('"') else { break };
        let path = &html[start..start + end_rel];
        if path.ends_with(|c: char| c.is_ascii_digit()) {
            // Look back a few hundred bytes for the preview marker.
            let from = start.saturating_sub(400);
            let window = &html[from..start];
            if window.contains("data-icon=\"preview\"") {
                preview.push(path.to_string());
            } else {
                any.push(path.to_string());
            }
        }
        index = start + end_rel;
        if index >= bytes.len() {
            break;
        }
    }
    let pick_newest = |mut list: Vec<String>| -> Option<String> {
        list.sort_by(|a, b| b.cmp(a));
        list.into_iter().next()
    };
    pick_newest(preview).or_else(|| pick_newest(any))
}

fn download_pages(html: &str, marker: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let mut index = 0;
    while let Some(found) = html[index..].find("href=\"") {
        let start = index + found + "href=\"".len();
        let Some(end_rel) = html[start..].find('"') else { break };
        let path = html[start..start + end_rel].trim_start_matches("/about/versions/");
        if path.contains(marker) {
            let rows = html.matches("tr id=").count();
            if best.as_ref().is_none_or(|(count, _)| rows > *count) {
                best = Some((rows, path.to_string()));
            }
        }
        index = start + end_rel;
    }
    best.map(|(_, path)| format!("https://developer.android.com{path}"))
}

fn parse_products(html: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    // `<tr id="<device>"> ... <td>Model</td> ...` — the model is the first cell.
    for chunk in html.split("tr id=\"").skip(1) {
        let Some(end) = chunk.find('"') else { continue };
        let device = &chunk[..end];
        if device.is_empty() || !device.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let Some(td_start) = chunk.find("<td>") else { continue };
        let Some(td_end_rel) = chunk[td_start..].find("</td>") else { continue };
        let model = &chunk[td_start + 4..td_start + td_end_rel];
        if model.is_empty() || model.len() > 64 {
            continue;
        }
        rows.push((model.to_string(), format!("{device}_beta")));
    }
    rows
}

fn client_config_key(html: &str) -> Option<String> {
    let marker = html.find("data-client-config=")?;
    let rest = &html[marker + "data-client-config=".len()..];
    let end = rest.find(['>', ' ', '\n', '"']).unwrap_or(rest.len());
    let raw = rest[..end].trim_matches(['"', '\'', ';', '&']);
    // The page emits `key=<value>`; keep only the value.
    let value = raw.split([';', '&']).find_map(|part| part.strip_prefix("key="))?;
    let key = value.trim();
    (!key.is_empty() && key.len() <= 128).then(|| key.to_string())
}

fn parse_canary(json: &str) -> Option<(String, String, String, String, String)> {
    let value: Value = serde_json::from_str(json).ok()?;
    let builds = value.get("builds").and_then(Value::as_array)?;
    // `tac ... | grep -m1 -A13 '"canary": true'` — newest canary entry.
    let canary = builds
        .iter()
        .rev()
        .find(|build| build.get("canary").and_then(Value::as_bool) == Some(true))?;
    let field = |name: &str| canary.get(name).and_then(Value::as_str).map(str::to_string);
    let release = field("releaseCandidateName")?;
    let incremental = field("buildId")?;
    let android_release = field("releaseTrackVersionName").unwrap_or_default();
    let image = field("factoryImageDownloadUrl").unwrap_or_default();
    let canary_id = field("canaryId").or_else(|| field("id")).unwrap_or_default();
    Some((release, incremental, android_release, image, canary_id))
}

fn head_last_modified(url: &str) -> Option<String> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let response = agent.head(uri).call().ok()?;
    response
        .headers()
        .get("last-modified")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// `Mon, 05 Aug 2026 00:00:00 GMT` -> `2026-08-05`. Implemented without chrono
/// so the module keeps its current dependency set.
fn parse_http_date(value: &str) -> Option<(i64, String)> {
    let mut parts = value.split_whitespace();
    let _weekday = parts.next()?;
    let day: i64 = parts.next()?.trim_end_matches(',').parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    if !(2000..=2100).contains(&year) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((days, format!("{year:04}-{month:02}-{day:02}")))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `autopif4.sh` greps the bulletin line that carries the build id and keeps the
/// last `<td>…</td>` on it. The date is not always in the cell right after the
/// build, so this walks the whole containing `<tr>` instead of assuming a
/// column order, and returns the first calendar date it finds.
fn bulletin_security_patch(html: &str, build: &str) -> Option<String> {
    let needle = format!("<td>{build}");
    let mut index = 0;
    while let Some(found) = html[index..].find(&needle) {
        let hit = index + found;
        let row_start = html[..hit].rfind("<tr").unwrap_or(hit);
        let row_end = html[hit..].find("</tr>").map(|end| hit + end).unwrap_or(html.len());
        let row = &html[row_start..row_end];
        let mut cell = 0;
        while let Some(open) = row[cell..].find("<td>") {
            let start = cell + open + 4;
            let Some(close) = row[start..].find("</td>") else { break };
            let text = row[start..start + close].trim();
            if is_patch_date(text) {
                return Some(text.to_string());
            }
            cell = start + close;
        }
        index = hit + needle.len();
    }
    None
}

fn is_patch_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let year: u32 = match value[..4].parse() {
        Ok(year) => year,
        Err(_) => return false,
    };
    let month: u32 = match value[5..7].parse() {
        Ok(month) => month,
        Err(_) => return false,
    };
    let day: u32 = match value[8..].parse() {
        Ok(day) => day,
        Err(_) => return false,
    };
    if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let leap = year % 400 == 0 || (year % 4 == 0 && year % 100 != 0);
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    day <= days[(month - 1) as usize]
}

/// Resolve a fresh Pixel Canary fingerprint. `match_device` mirrors `-m`/`--match`.
pub fn resolve_canary(match_device: Option<&str>) -> Result<CanaryBuild> {
    let versions = fetch(VERSIONS_URL, allowed, MAX_PAGE_BYTES, "Android versions index")?;
    let beta_path = latest_beta_path(&versions)
        .ok_or_else(|| anyhow!("no Pixel beta track found on the Android versions index"))?;
    let beta_url = format!("https://developer.android.com{beta_path}");
    let beta = fetch(&beta_url, allowed, MAX_PAGE_BYTES, "Pixel beta track")?;

    let factory = download_pages(&beta, "factory")
        .ok_or_else(|| anyhow!("no Pixel factory image page in the current beta track"))?;
    let factory_html = fetch(&factory, allowed, MAX_PAGE_BYTES, "Pixel factory images")?;
    let mut rows = parse_products(&factory_html);
    if rows.is_empty() {
        let ota = download_pages(&beta, "ota")
            .ok_or_else(|| anyhow!("no Pixel device rows on the current beta pages"))?;
        let ota_html = fetch(&ota, allowed, MAX_PAGE_BYTES, "Pixel OTA images")?;
        rows = parse_products(&ota_html);
    }
    if rows.is_empty() {
        bail!("the Pixel beta pages list no usable devices");
    }

    let chosen = match_device
        .and_then(|device| {
            let product = format!("{device}_beta");
            rows.iter().find(|(_, p)| *p == product).cloned()
        })
        .unwrap_or_else(|| {
            // Deterministic pick: the newest device alphabetically. A random pick
            // would make the stored profile churn on every refresh.
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            rows[0].clone()
        });

    let flash = fetch(FLASH_URL, allowed, MAX_PAGE_BYTES, "Android Flash Tool")?;
    let key = client_config_key(&flash)
        .ok_or_else(|| anyhow!("the Android Flash Tool page exposes no client-config key"))?;
    let builds_url =
        format!("{FLASH_API}?product={}&key={}", percent_encode(&chosen.1), percent_encode(&key));
    let builds = fetch(&builds_url, allowed, MAX_RELEASE_BYTES, "Flash Station builds")?;
    let (release, incremental, android_release, image, canary_id) =
        parse_canary(&builds).ok_or_else(|| anyhow!("Flash Station returned no canary build"))?;

    // `20260805` -> `2026-08-05`
    let canary_date = (canary_id.len() == 8 && canary_id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{}-{}-{}", &canary_id[..4], &canary_id[4..6], &canary_id[6..]));

    let (released_on, expires_on) = match head_last_modified(&image) {
        Some(value) => match parse_http_date(&value) {
            Some((days, date)) => {
                let (year, month, day) = civil_from_days(days + 7 * CANARY_EXPIRY_WEEKS as i64);
                (Some(date), Some(format!("{year:04}-{month:02}-{day:02}")))
            }
            None => (None, None),
        },
        None => (None, None),
    };

    let bulletin = fetch(PIXEL_BULLETIN_URL, allowed, MAX_PAGE_BYTES, "Pixel update bulletins")?;
    let security_patch = bulletin_security_patch(&bulletin, &incremental)
        .or_else(|| canary_date.clone().map(|date| format!("{date}-05")))
        .ok_or_else(|| anyhow!("cannot resolve a security patch level for this canary build"))?;

    if !is_patch_date(&security_patch) {
        bail!("resolved security patch level is not a calendar date");
    }

    let device = chosen.1.trim_end_matches("_beta").to_string();
    let product = chosen.1;
    Ok(CanaryBuild {
        fingerprint: format!(
            "google/{product}/{device}:CANARY/{release}/{incremental}:user/release-keys"
        ),
        incremental: incremental.clone(),
        model: chosen.0,
        device_initial_sdk_int: first_api_level(&device),
        security_patch,
        product,
        device,
        release_candidate: release,
        build_id: incremental,
        android_release,
        released_on,
        expires_on,
    })
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_dates_round_trip_through_civil_days() {
        let (days, date) = parse_http_date("Mon, 05 Aug 2026 00:00:00 GMT").unwrap();
        assert_eq!(date, "2026-08-05");
        assert_eq!(civil_from_days(days), (2026, 8, 5));
        assert_eq!(civil_from_days(days + 42), (2026, 9, 16));
        assert!(parse_http_date("nonsense").is_none());
        assert!(parse_http_date("Mon, 32 Aug 2026 00:00:00 GMT").is_none());
    }

    #[test]
    fn patch_dates_reject_impossible_values() {
        assert!(is_patch_date("2026-08-05"));
        assert!(is_patch_date("2024-02-29"));
        assert!(!is_patch_date("2026-02-29"));
        assert!(!is_patch_date("2026-13-01"));
        assert!(!is_patch_date("2026-00-10"));
        assert!(!is_patch_date("2026-08-00"));
        assert!(!is_patch_date("2026-8-5"));
        assert!(!is_patch_date("20260805"));
    }

    #[test]
    fn beta_track_prefers_the_preview_link() {
        let html = r#"<a href="/about/versions/16">16</a>
            <a data-icon="preview" href="/about/versions/17">17</a>
            <a href="/about/versions/15">15</a>"#;
        assert_eq!(latest_beta_path(html).as_deref(), Some("/about/versions/17"));
        let plain = r#"<a href="/about/versions/15">15</a><a href="/about/versions/16">16</a>"#;
        assert_eq!(latest_beta_path(plain).as_deref(), Some("/about/versions/16"));
        assert!(latest_beta_path("nothing here").is_none());
    }

    #[test]
    fn device_rows_become_beta_products() {
        let html = r#"<tr id="mustang">
            <td>Pixel 10 Pro XL</td><td>...</td>
            <tr id="tokay"><td>Pixel 9</td><td>...</td>"#;
        let rows = parse_products(html);
        assert_eq!(rows[0], ("Pixel 10 Pro XL".to_string(), "mustang_beta".to_string()));
        assert_eq!(rows[1], ("Pixel 9".to_string(), "tokay_beta".to_string()));
    }

    #[test]
    fn canary_entries_are_read_newest_first() {
        let json = r#"{"builds":[
            {"canary":false,"buildId":"old"},
            {"canary":true,"releaseCandidateName":"BP2A.260805.001","buildId":"16004061",
             "releaseTrackVersionName":"17","factoryImageDownloadUrl":"https://x/y.zip",
             "canaryId":"2026-08-05"}]}"#;
        let (release, incremental, _, _, canary) = parse_canary(json).unwrap();
        assert_eq!(release, "BP2A.260805.001");
        assert_eq!(incremental, "16004061");
        assert_eq!(canary, "2026-08-05");
        assert!(parse_canary("{}").is_none());
    }

    #[test]
    fn build_ids_map_to_the_documented_first_api_levels() {
        assert_eq!(first_api_level("bluejay"), 32);
        assert_eq!(first_api_level("husky"), 34);
        assert_eq!(first_api_level("mustang"), 36);
        assert_eq!(first_api_level("kodiak"), 37);
        assert_eq!(first_api_level("unknown-device"), 37);
    }

    #[test]
    fn bulletin_rows_resolve_a_patch_date() {
        let html = "<tr><td>16004061</td><td>2026-08-05</td></tr>";
        assert_eq!(bulletin_security_patch(html, "16004061").as_deref(), Some("2026-08-05"));
        assert!(bulletin_security_patch(html, "99999999").is_none());
    }
}
