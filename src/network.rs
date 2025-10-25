// Copyright © 2025 Akira Miyakoda
//
// This software is released under the MIT License.
// https://opensource.org/licenses/MIT

use std::time::Duration;

use anyhow::anyhow;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{delay::FreeRtos, io::Write, modem::Modem},
    http::{
        Method,
        server::{Configuration as ServerConfiguration, EspHttpServer},
    },
    sntp::{EspSntp, SntpConf, SyncStatus},
    wifi::{ClientConfiguration, Configuration as WifiConfiguration, EspWifi},
};
use futures::executor;
use log::error;
use serde::Serialize;
use tokio::{
    sync::RwLock,
    task,
    time::{MissedTickBehavior, interval},
};

use crate::{measurements, nvs};

const DELAY: u32 = 10;
const MAX_TIMEOUT: u32 = 10_000 / DELAY; // 10 seconds / 10 ms

pub(crate) struct Context<'a> {
    wifi: EspWifi<'a>,
    #[allow(dead_code)]
    ntp: EspSntp<'a>,
    #[allow(dead_code)]
    server: EspHttpServer<'a>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Status {
    pub signal_quality: SignalQuality,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum SignalQuality {
    #[default]
    Unreliable,
    Poor,
    Fair,
    Good,
    Excellent,
}

impl From<SignalQuality> for i32 {
    fn from(value: SignalQuality) -> Self {
        match value {
            SignalQuality::Unreliable => 0,
            SignalQuality::Poor => 1,
            SignalQuality::Fair => 2,
            SignalQuality::Good => 3,
            SignalQuality::Excellent => 4,
        }
    }
}

impl SignalQuality {
    fn from_rssi(rssi: i32) -> Self {
        let rssi = (-rssi).clamp(0, 100);
        if (0..=50).contains(&rssi) {
            Self::Excellent
        } else if (51..=60).contains(&rssi) {
            Self::Good
        } else if (61..=70).contains(&rssi) {
            Self::Fair
        } else if (71..=85).contains(&rssi) {
            Self::Poor
        } else {
            Self::Unreliable
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Message {
    pub timestamp: i64,
    pub temperature: f32,
    pub tds: i32,
}

impl From<measurements::Values> for Message {
    fn from(value: measurements::Values) -> Self {
        Self {
            timestamp: value.timestamp,
            temperature: value.temperature,
            tds: value.tds as i32,
        }
    }
}

static STATUS: RwLock<Option<Status>> = RwLock::const_new(None);

pub(crate) async fn get() -> Option<Status> {
    *STATUS.read().await
}

pub(crate) fn init<'a>(modem: Modem, event_loop: EspSystemEventLoop) -> anyhow::Result<Box<Context<'a>>> {
    task::block_in_place(move || {
        let wifi = init_wifi(modem, event_loop)?;
        let ntp = init_ntp()?;
        let server = init_http_server()?;

        Ok(Box::new(Context { wifi, ntp, server }))
    })
}

fn init_wifi<'a>(modem: Modem, event_loop: EspSystemEventLoop) -> anyhow::Result<EspWifi<'a>> {
    let ssid = nvs::get("ssid")?;
    let psk = nvs::get("psk")?;

    let mut wifi = EspWifi::new(modem, event_loop, None)?;
    wifi.set_configuration(&WifiConfiguration::Client(ClientConfiguration {
        ssid: ssid.as_str().try_into().map_err(|e| anyhow!("{e:?}"))?,
        password: psk.as_str().try_into().map_err(|e| anyhow!("{e:?}"))?,
        ..Default::default()
    }))?;
    wifi.start()?;
    connect_and_wait(&mut wifi)?;

    Ok(wifi)
}

fn connect_and_wait(wifi: &mut EspWifi<'_>) -> anyhow::Result<()> {
    wifi.connect()?;

    // Wait for DNS to get ready
    let mut timeout = 0;
    while wifi.sta_netif().get_dns().is_unspecified() {
        FreeRtos::delay_ms(DELAY);

        timeout += 1;
        if timeout >= MAX_TIMEOUT {
            return Err(anyhow!("WiFi connection timeout"));
        }
    }

    Ok(())
}

fn init_ntp() -> anyhow::Result<EspSntp<'static>> {
    let ntp_server = nvs::get("ntp_server")?;

    let ntp = EspSntp::new(&SntpConf {
        servers: [&ntp_server],
        ..Default::default()
    })?;

    // Wait for NTP client to get synchronized
    let mut timeout = 0;
    while ntp.get_sync_status() != SyncStatus::Completed {
        FreeRtos::delay_ms(DELAY);

        timeout += 1;
        if timeout >= MAX_TIMEOUT {
            return Err(anyhow!("NTP Sync timeout"));
        }
    }

    Ok(ntp)
}

fn init_http_server() -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&ServerConfiguration::default())?;
    server.fn_handler("/", Method::Get, move |request| {
        const NO_CONTENT: u16 = 204;

        let (mut res, msg) = match executor::block_on(measurements::get()) {
            Some(values) => (
                request.into_ok_response()?,
                serde_json::to_string(&Message::from(values))?.into_bytes(),
            ),
            None => (request.into_status_response(NO_CONTENT)?, vec![]),
        };
        res.write_all(&msg)?;

        anyhow::Ok(())
    })?;

    Ok(server)
}
pub(crate) async fn worker(ctx: &mut Box<Context<'_>>) -> anyhow::Result<()> {
    let mut interval = interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        if let Err(e) = update(ctx).await {
            error!("Failed to update status: {e:?}");
        }
    }
}

async fn update<'a>(ctx: &mut Context<'a>) -> anyhow::Result<()> {
    let status = task::block_in_place(move || {
        // Reconnect to WiFi if disconnected
        if !ctx.wifi.is_connected().unwrap_or(false) {
            connect_and_wait(&mut ctx.wifi)?;
        }

        // Update WiFi status
        let rssi = ctx.wifi.get_rssi()?;
        let signal_quality = SignalQuality::from_rssi(rssi);

        anyhow::Ok(Status { signal_quality })
    })?;

    *STATUS.write().await = Some(status);

    Ok(())
}
