use super::*;
use crate::tests::{build, TestRoot};
use std::collections::VecDeque;
use std::net::TcpListener;

// Deliberately not a real XML keybox or certificate. Only the native importer
// owns XML validation; these fixtures exercise the updater and its boundaries.
const FIXTURE: &[u8] = b"fixture-keybox-v1";
const NEXT_FIXTURE: &[u8] = b"fixture-keybox-v2";

struct MockBackend {
    local: Option<Vec<u8>>,
    responses: VecDeque<Result<KeyboxResponse>>,
    requests: Vec<Validators>,
    stats_calls: usize,
    installs: usize,
    stats_ok: bool,
    install_ok: bool,
    install_writes: bool,
    mutate_on_download: Option<Vec<u8>>,
}

impl MockBackend {
    fn new(local: Option<&[u8]>, responses: Vec<Result<KeyboxResponse>>) -> Self {
        Self {
            local: local.map(<[u8]>::to_vec),
            responses: responses.into(),
            requests: Vec::new(),
            stats_calls: 0,
            installs: 0,
            stats_ok: true,
            install_ok: true,
            install_writes: true,
            mutate_on_download: None,
        }
    }
}

impl UpdateBackend for MockBackend {
    fn stats(&mut self) -> Result<()> {
        self.stats_calls += 1;
        if self.stats_ok {
            Ok(())
        } else {
            Err("mock stats offline".into())
        }
    }

    fn download(&mut self, validators: &Validators) -> Result<KeyboxResponse> {
        self.requests.push(validators.clone());
        if let Some(bytes) = self.mutate_on_download.take() {
            self.local = Some(bytes);
        }
        self.responses.pop_front().expect("unexpected mock download")
    }

    fn local_keybox(&self) -> Result<Vec<u8>> {
        self.local.clone().ok_or_else(|| "mock local keybox missing".into())
    }

    fn install(&mut self, bytes: &[u8]) -> Result<()> {
        self.installs += 1;
        if !self.install_ok {
            return Err("mock native validation failed".into());
        }
        if self.install_writes {
            self.local = Some(bytes.to_vec());
        }
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn response(status: u16, bytes: &[u8]) -> KeyboxResponse {
    let mut headers = ureq::http::HeaderMap::new();
    if status == 200 {
        headers.insert("X-Keybox-Sha256", digest(bytes).parse().unwrap());
    }
    headers.insert("etag", "\"fixture-1\"".parse().unwrap());
    headers.insert("last-modified", "Tue, 10 Nov 2026 12:00:00 GMT".parse().unwrap());
    KeyboxResponse { status, headers, bytes: bytes.to_vec() }
}

fn cache(paths: &Paths, bytes: &[u8]) -> CloudObservation {
    let cloud = CloudObservation {
        checked_at: now().saturating_sub(1),
        boot_id: current_boot_id(),
        connected: true,
        keybox: KeyboxUpdate::Unchanged,
        keybox_checked_at: now().saturating_sub(1),
        installed_sha256: Some(digest(bytes)),
        validators: Validators::from_headers(&response(200, bytes).headers),
        ..CloudObservation::default()
    };
    cloud.save(paths).unwrap();
    cloud
}

#[test]
fn startup_checks_after_ready_but_restarts_preserve_outage_backoff() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut cloud = cache(&paths, FIXTURE);
    cloud.finish(now(), true);
    cloud.save(&paths).unwrap();
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(response(304, b""))]);
    assert_eq!(cloud_check(&paths, CheckMode::Startup, &mut backend).unwrap(), Some(true));
    assert_eq!(backend.requests.len(), 1);
    cloud = CloudObservation::load(&paths);
    cloud.finish(now(), false);
    cloud.save(&paths).unwrap();
    assert_eq!(cloud_check(&paths, CheckMode::Startup, &mut backend).unwrap(), None);
    assert_eq!(backend.requests.len(), 1);
}

#[test]
fn initial_ready_check_imports_once_and_next_periodic_check_waits_six_hours() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(None, vec![Ok(response(200, FIXTURE))]);
    assert_eq!(cloud_check(&paths, CheckMode::Scheduled, &mut backend).unwrap(), Some(false));
    assert_eq!(backend.installs, 1);
    let cloud = CloudObservation::load(&paths);
    assert_eq!(cloud.keybox, KeyboxUpdate::Updated);
    assert_eq!(cloud.installed_sha256, Some(digest(FIXTURE)));
    assert_eq!(cloud.next_check_at - cloud.checked_at, CLOUD_INTERVAL.as_secs());
    assert_eq!(cloud.failures, 0);
    assert_eq!(cloud_check(&paths, CheckMode::Scheduled, &mut backend).unwrap(), None);
    assert_eq!(backend.stats_calls, 1);
    assert_eq!(backend.requests.len(), 1);
    assert_eq!(backend.installs, 1);
    assert!(fs::read_dir(&paths.data)
        .unwrap()
        .all(|entry| { !entry.unwrap().file_name().to_string_lossy().contains(".tmp") }));
}

#[test]
fn identical_200_skips_staging_native_import_and_description_writes() {
    let root = TestRoot::new();
    let paths = root.paths();
    let prop = paths.module.join("module.prop");
    fs::write(&prop, "description=keep\n").unwrap();
    let before = fs::metadata(&prop).unwrap().modified().unwrap();
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(response(200, FIXTURE))]);
    assert_eq!(cloud_check(&paths, CheckMode::Manual, &mut backend).unwrap(), Some(true));
    assert_eq!(backend.installs, 0);
    assert_eq!(fs::metadata(prop).unwrap().modified().unwrap(), before);
    assert_eq!(CloudObservation::load(&paths).keybox, KeyboxUpdate::Unchanged);
    assert_eq!(fs::read_dir(&paths.data).unwrap().count(), 2); // lock + state, no staging
}

#[test]
fn conditional_304_uses_confirmed_local_cache_without_reimporting_or_reading_body() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut cloud = cache(&paths, FIXTURE);
    let old_validators = cloud.validators.clone();
    let mut not_modified = response(304, b"not a keybox body");
    not_modified.headers.remove("last-modified");
    not_modified.headers.insert("etag", "W/\"fixture-2\"".parse().unwrap());
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(not_modified)]);
    assert!(update_keybox(&paths, &mut cloud, &mut backend).unwrap());
    assert_eq!(backend.requests.as_slice(), std::slice::from_ref(&old_validators));
    assert_eq!(backend.installs, 0);
    assert_eq!(cloud.validators.etag.as_deref(), Some("W/\"fixture-2\""));
    assert_eq!(cloud.validators.last_modified, old_validators.last_modified);
    assert_eq!(cloud.installed_sha256, Some(digest(FIXTURE)));
}

#[test]
fn changed_or_missing_local_file_drops_conditionals_and_downloads_full_body() {
    for local in [None, Some(NEXT_FIXTURE)] {
        let root = TestRoot::new();
        let paths = root.paths();
        let mut cloud = cache(&paths, FIXTURE);
        let mut backend = MockBackend::new(local, vec![Ok(response(200, FIXTURE))]);
        assert!(!update_keybox(&paths, &mut cloud, &mut backend).unwrap());
        assert_eq!(backend.requests, [Validators::default()]);
        assert_eq!(backend.installs, 1);
    }
}

#[test]
fn unrequested_304_and_local_mutation_during_conditional_request_fail_without_success() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(None, vec![Ok(response(304, b""))]);
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    let failed = CloudObservation::load(&paths);
    assert_eq!(failed.keybox, KeyboxUpdate::Failed);
    assert_eq!(failed.installed_sha256, None);
    assert_eq!(failed.keybox_checked_at, 0);
    assert_eq!(backend.installs, 0);

    let mut cloud = cache(&paths, FIXTURE);
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(response(304, b""))]);
    backend.mutate_on_download = Some(NEXT_FIXTURE.to_vec());
    assert!(update_keybox(&paths, &mut cloud, &mut backend).is_err());
    assert_eq!(backend.installs, 0);
}

#[test]
fn failed_native_install_preserves_successful_cache_and_retry_does_not_304_the_failed_version() {
    let root = TestRoot::new();
    let paths = root.paths();
    let old = cache(&paths, FIXTURE);
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(response(200, NEXT_FIXTURE))]);
    backend.install_ok = false;
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    let failed = CloudObservation::load(&paths);
    assert_eq!(failed.keybox, KeyboxUpdate::Failed);
    assert_eq!(failed.installed_sha256, old.installed_sha256);
    assert_eq!(failed.validators, old.validators);
    assert_eq!(failed.keybox_checked_at, old.keybox_checked_at);
    assert_eq!(failed.next_check_at - failed.checked_at, CLOUD_RETRY_MIN.as_secs());
    backend.install_ok = true;
    backend.responses.push_back(Ok(response(200, NEXT_FIXTURE)));
    assert_eq!(cloud_check(&paths, CheckMode::Manual, &mut backend).unwrap(), Some(false));
    assert_eq!(backend.installs, 2);
    assert_eq!(backend.requests, [old.validators.clone(), old.validators]);
}

#[test]
fn acknowledgement_without_matching_installed_bytes_does_not_record_update_success() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(None, vec![Ok(response(200, FIXTURE))]);
    backend.install_writes = false;
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    let cloud = CloudObservation::load(&paths);
    assert_eq!(cloud.keybox, KeyboxUpdate::Failed);
    assert_eq!(cloud.installed_sha256, None);
    assert_eq!(cloud.keybox_checked_at, 0);
}

#[test]
fn offline_retry_is_persisted_exponential_bounded_and_never_every_minute() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Err("mock network offline".into())]);
    backend.stats_ok = false;
    assert!(cloud_check(&paths, CheckMode::Scheduled, &mut backend).is_err());
    let cloud = CloudObservation::load(&paths);
    assert!(!cloud.connected);
    assert_eq!(cloud.failures, 1);
    assert_eq!(cloud.next_check_at - cloud.checked_at, 15 * 60);
    assert_eq!(cloud_check(&paths, CheckMode::Scheduled, &mut backend).unwrap(), None);
    assert_eq!(backend.requests.len(), 1);
    assert!(status(Ok(()), &cloud).ok);
    let mut schedule = CloudObservation::default();
    for (index, seconds) in [900, 1800, 3600, 7200, 14400, 21600, 21600].into_iter().enumerate() {
        schedule.finish(1000, false);
        assert_eq!(schedule.failures, index as u32 + 1);
        assert_eq!(schedule.next_check_at - schedule.checked_at, seconds);
        assert!(!schedule.due(1000 + 60));
        assert!(schedule.due(1000 + seconds));
    }
    schedule.finish(2000, true);
    assert_eq!(schedule.failures, 0);
    assert_eq!(schedule.next_check_at, 2000 + CLOUD_INTERVAL.as_secs());
    assert!(schedule.due(1999)); // time moved backwards
}

#[test]
fn stats_offline_does_not_block_a_valid_keybox_or_claim_core_failure() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(None, vec![Ok(response(200, FIXTURE))]);
    backend.stats_ok = false;
    assert_eq!(cloud_check(&paths, CheckMode::Manual, &mut backend).unwrap(), Some(false));
    let cloud = CloudObservation::load(&paths);
    assert_eq!(cloud.keybox, KeyboxUpdate::Updated);
    assert!(cloud.connected);
    assert_eq!(cloud.failures, 0);
    let checked = status(Ok(()), &cloud);
    assert!(checked.ok && checked.cloud_connected);
    assert_eq!(checked.phase, "running");
    let text = description(&build(), DisplayState::Checked(&checked));
    assert!(text.starts_with('\u{2705}'));
    assert!(!text.contains("mock stats offline"));
}

#[test]
fn stats_connection_and_keybox_update_errors_remain_separate() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut backend = MockBackend::new(None, vec![Err("mock keybox endpoint failed".into())]);
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    let cloud = CloudObservation::load(&paths);
    assert!(cloud.connected);
    assert_eq!(cloud.keybox, KeyboxUpdate::Failed);
    assert_eq!(cloud.failures, 1);
    assert_eq!(cloud.keybox_checked_at, 0);
    let checked = status(Ok(()), &cloud);
    assert!(checked.ok && checked.cloud_connected);
    assert_eq!(checked.keybox_update, "failed");
    assert!(checked.keybox_reason.contains("mock keybox endpoint failed"));
    assert_eq!(cloud.next_check_at - cloud.checked_at, CLOUD_RETRY_MIN.as_secs());
}

#[test]
fn bad_hash_bad_status_encoded_and_oversized_responses_never_import() {
    let mut cases = Vec::new();
    let mut bad_hash = response(200, FIXTURE);
    bad_hash.headers.insert("X-Keybox-Sha256", "0".repeat(64).parse().unwrap());
    cases.push(bad_hash);
    let mut missing = response(200, FIXTURE);
    missing.headers.remove("X-Keybox-Sha256");
    cases.push(missing);
    let mut encoded = response(200, FIXTURE);
    encoded.headers.insert("content-encoding", "gzip".parse().unwrap());
    cases.push(encoded);
    cases.push(response(200, &vec![b'x'; KEYBOX_LIMIT + 1]));
    cases.push(response(200, b""));
    cases.push(response(302, b""));
    cases.push(response(503, b""));
    for reply in cases {
        let root = TestRoot::new();
        let paths = root.paths();
        let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(reply)]);
        assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
        assert_eq!(backend.installs, 0);
        assert_eq!(backend.local.as_deref(), Some(FIXTURE));
        assert_eq!(CloudObservation::load(&paths).keybox, KeyboxUpdate::Failed);
    }
}

#[test]
fn optional_hash_on_304_must_match_confirmed_cache() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut cloud = cache(&paths, FIXTURE);
    let mut reply = response(304, b"");
    reply.headers.insert("X-Keybox-Sha256", digest(NEXT_FIXTURE).parse().unwrap());
    let mut backend = MockBackend::new(Some(FIXTURE), vec![Ok(reply)]);
    assert!(update_keybox(&paths, &mut cloud, &mut backend).is_err());
    assert_eq!(backend.installs, 0);
}

#[test]
fn last_modified_only_is_supported_and_unsafe_or_large_validators_are_discarded() {
    let mut headers = ureq::http::HeaderMap::new();
    headers.insert("last-modified", "Tue, 10 Nov 2026 12:00:00 GMT".parse().unwrap());
    let only_date = Validators::from_headers(&headers);
    assert!(only_date.present() && only_date.etag.is_none());
    for etag in ["bad", "\"a b\"", "\"a\"b\"", "\"\r\ninject\"", &"x".repeat(513)] {
        let mut validators = Validators { etag: Some(etag.into()), last_modified: None };
        validators.sanitize();
        assert!(!validators.present());
    }
    let mut validators = Validators {
        etag: Some("W/\"good\"".into()),
        last_modified: Some("date\r\nHeader: value".into()),
    };
    validators.sanitize();
    assert_eq!(validators.etag.as_deref(), Some("W/\"good\""));
    assert_eq!(validators.last_modified, None);
}

#[test]
fn concurrent_manual_monitor_updates_do_no_network_without_lock() {
    let root = TestRoot::new();
    let paths = root.paths();
    let lock = Lock::acquire(&paths.data.join("update.lock"), Duration::ZERO).unwrap();
    let mut backend = MockBackend::new(None, vec![]);
    assert_eq!(cloud_check(&paths, CheckMode::Scheduled, &mut backend).unwrap(), None);
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    assert_eq!(backend.stats_calls, 0);
    assert!(backend.requests.is_empty());
    assert!(!paths.data.join("cloud.json").exists());
    drop(lock);
}

#[test]
fn real_http_mock_sends_both_conditionals_and_accepts_304_and_last_modified_only() {
    for with_etag in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0u8; 1];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 4096);
            }
            stream
                .write_all(b"HTTP/1.1 304 Not Modified\r\nETag: W/\"fixture-2\"\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8(request).unwrap().to_ascii_lowercase()
        });
        let client: ureq::Agent = ureq::Agent::config_builder()
            .https_only(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let mut validators = Validators::from_headers(&response(200, FIXTURE).headers);
        if !with_etag {
            validators.etag = None;
        }
        let reply = download_keybox(&client, &url, &validators).unwrap();
        assert_eq!(reply.status, 304);
        assert!(reply.bytes.is_empty());
        let request = server.join().unwrap();
        assert!(request.contains("if-modified-since: tue, 10 nov 2026 12:00:00 gmt\r\n"));
        assert_eq!(request.contains("if-none-match: \"fixture-1\"\r\n"), with_etag);
    }
}

#[test]
fn real_http_mock_truncated_body_fails_without_returning_download_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort")
            .unwrap();
    });
    let client: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into();
    assert!(download_keybox(&client, &url, &Validators::default()).is_err());
    server.join().unwrap();
}

#[test]
fn atomic_state_error_is_not_reported_as_success() {
    let root = TestRoot::new();
    let paths = root.paths();
    fs::create_dir(paths.data.join("cloud.json")).unwrap();
    let mut backend = MockBackend::new(None, vec![Ok(response(200, FIXTURE))]);
    assert!(cloud_check(&paths, CheckMode::Manual, &mut backend).is_err());
    assert_eq!(backend.installs, 1);
    assert!(paths.data.join("cloud.json").is_dir());
    assert_eq!(CloudObservation::load(&paths).keybox, KeyboxUpdate::Pending);
}
