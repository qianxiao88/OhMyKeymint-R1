use std::sync::Mutex;

use kmr_common::km_err;
use kmr_ta::device::RetrieveAttestationIds;
use kmr_wire::AttestationIdInfo;

use crate::config::config;

pub struct AttestationIdMgr;

struct AttestationIdCache {
    snapshot: Mutex<Option<AttestationIdInfo>>,
    discovery: Mutex<()>,
}

impl AttestationIdCache {
    const fn new() -> Self {
        Self { snapshot: Mutex::new(None), discovery: Mutex::new(()) }
    }

    fn read(&self) -> Result<Option<AttestationIdInfo>, kmr_common::Error> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| km_err!(UnknownError, "attestation ID cache lock poisoned"))
    }

    fn prepare_with(
        &self,
        resolve: impl FnOnce() -> Result<Option<AttestationIdInfo>, kmr_common::Error>,
    ) -> Result<(), kmr_common::Error> {
        if self.read()?.is_some() {
            return Ok(());
        }
        let _discovery = self
            .discovery
            .lock()
            .map_err(|_| km_err!(UnknownError, "attestation ID discovery lock poisoned"))?;
        if self.read()?.is_some() {
            return Ok(());
        }
        // Discovery may take OperationLock, perform Binder calls and persist config.
        // Never retain the snapshot mutex: a TA callback must only read this cache.
        if let Some(ids) = resolve()? {
            *self
                .snapshot
                .lock()
                .map_err(|_| km_err!(UnknownError, "attestation ID cache lock poisoned"))? =
                Some(ids);
        }
        Ok(())
    }
}

static ATTESTATION_IDS: AttestationIdCache = AttestationIdCache::new();

/// Run before acquiring a TA mutex. The TA callback itself never discovers IDs
/// or waits for the data-operation lock held by runtime config reload.
pub(crate) fn prepare_attestation_ids() {
    if let Err(error) = ATTESTATION_IDS.prepare_with(resolve_attestation_ids) {
        log::warn!("failed to prepare runtime attestation IDs: {error:?}");
    }
}

fn resolve_attestation_ids() -> Result<Option<AttestationIdInfo>, kmr_common::Error> {
    let guard = config().read().map_err(|_| km_err!(UnknownError, "config lock poisoned"))?;
    let needs_resolution = !guard.device.override_telephony_properties
        && (guard.device.imei.trim().is_empty()
            || guard.device.imei2.trim().is_empty()
            || guard.device.meid.trim().is_empty());
    let device = if needs_resolution {
        drop(guard);
        match crate::plat::device_ids::resolve_runtime_device_ids() {
            Ok(Some(device)) => device,
            Ok(None) => return Ok(None),
            Err(error) => {
                log::warn!("failed to resolve runtime attestation IDs: {error:#}");
                return Ok(None);
            }
        }
    } else {
        guard.device.clone()
    };
    Ok(Some(AttestationIdInfo {
        brand: device.brand.into_bytes(),
        device: device.device.into_bytes(),
        product: device.product.into_bytes(),
        serial: device.serial.into_bytes(),
        imei: device.imei.into_bytes(),
        imei2: device.imei2.into_bytes(),
        meid: device.meid.into_bytes(),
        manufacturer: device.manufacturer.into_bytes(),
        model: device.model.into_bytes(),
    }))
}

impl RetrieveAttestationIds for AttestationIdMgr {
    fn get(&self) -> Result<AttestationIdInfo, kmr_common::Error> {
        self.get_ids()?.ok_or_else(|| km_err!(CannotAttestIds, "attestation ID info not available"))
    }

    fn get_ids(&self) -> Result<Option<AttestationIdInfo>, kmr_common::Error> {
        ATTESTATION_IDS.read()
    }

    fn destroy_all(&mut self) -> Result<(), kmr_common::Error> {
        // ignore this
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    fn single_imei_ids() -> AttestationIdInfo {
        AttestationIdInfo {
            brand: b"fixture-brand".to_vec(),
            device: b"fixture-device".to_vec(),
            product: b"fixture-product".to_vec(),
            serial: b"fixture-serial".to_vec(),
            manufacturer: b"fixture-manufacturer".to_vec(),
            model: b"fixture-model".to_vec(),
            imei: b"111111111111111".to_vec(),
            imei2: Vec::new(),
            meid: Vec::new(),
        }
    }

    #[test]
    fn partial_snapshot_is_one_shot_and_callback_only_reads_cache() {
        let cache = AttestationIdCache::new();
        assert!(cache.read().unwrap().is_none());
        cache
            .prepare_with(|| {
                assert!(cache.read().unwrap().is_none());
                Ok(Some(single_imei_ids()))
            })
            .unwrap();
        cache.prepare_with(|| panic!("a partial successful snapshot must be cached")).unwrap();
        let ids = cache.read().unwrap().unwrap();
        assert_eq!(ids.imei, b"111111111111111");
        assert!(ids.imei2.is_empty() && ids.meid.is_empty());
        assert_eq!(ids.brand, b"fixture-brand");
    }

    #[test]
    fn unavailable_services_and_discovery_errors_remain_retryable() {
        let cache = AttestationIdCache::new();
        cache.prepare_with(|| Ok(None)).unwrap();
        assert!(cache.read().unwrap().is_none());
        assert!(cache.prepare_with(|| Err(km_err!(UnknownError, "fixture error"))).is_err());
        assert!(cache.read().unwrap().is_none());
        cache.prepare_with(|| Ok(Some(single_imei_ids()))).unwrap();
        assert!(cache.read().unwrap().is_some());
    }

    #[test]
    fn ta_callback_does_not_wait_for_discovery_blocked_by_reload_operation_lock() {
        let cache = Arc::new(AttestationIdCache::new());
        let operation = Arc::new(Mutex::new(()));
        let reload = operation.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let discovery_cache = Arc::clone(&cache);
        let discovery_operation = Arc::clone(&operation);
        let discovery = std::thread::spawn(move || {
            discovery_cache.prepare_with(|| {
                started_tx.send(()).unwrap();
                let _operation = discovery_operation.lock().unwrap();
                Ok(Some(single_imei_ids()))
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (callback_tx, callback_rx) = mpsc::channel();
        let callback = std::thread::spawn(move || {
            callback_tx.send(cache.read().unwrap().is_none()).unwrap();
        });
        let callback_result = callback_rx.recv_timeout(Duration::from_secs(2));
        // Release before asserting so a regression cannot strand the test worker.
        drop(reload);
        discovery.join().unwrap().unwrap();
        callback.join().unwrap();
        assert!(callback_result.unwrap());
    }
}
