// Copyright © 2025 Akira Miyakoda
//
// This software is released under the MIT License.
// https://opensource.org/licenses/MIT

use std::sync::OnceLock;

use anyhow::anyhow;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

static NVS: OnceLock<EspNvs<NvsDefault>> = OnceLock::new();

pub(crate) fn get(key: &str) -> anyhow::Result<String> {
    let mut buf = vec![0_u8; 128];

    let nvs = NVS.get().expect("NVS not initialized");
    let value = nvs.get_str(key, &mut buf)?;
    value.ok_or(anyhow!("Value not found")).map(|v| v.to_owned())
}

pub(crate) fn init(partition: EspDefaultNvsPartition) -> anyhow::Result<()> {
    let nvs = EspNvs::new(partition, "cobitis-config", false)?;
    NVS.set(nvs).map_err(|_| anyhow!("NVS already initialized"))?;

    Ok(())
}
