// Copyright © 2025 Akira Miyakoda
//
// This software is released under the MIT License.
// https://opensource.org/licenses/MIT

use std::time::Duration;

use ads1x1x::{Ads1x1x, FullScaleRange, TargetAddr, channel};
use anyhow::anyhow;
use chrono::Utc;
use ds18b20::{Ds18b20, InputPin, OneWire, OutputPin, Resolution};
use esp_idf_svc::hal::{
    delay::{Delay, FreeRtos},
    gpio::GpioError,
    i2c::I2cError,
};
use log::{error, info};
use tokio::{
    sync::RwLock,
    task,
    time::{MissedTickBehavior, interval},
};

type Ads1115<I2C> = Ads1x1x<I2C, ads1x1x::ic::Ads1115, ads1x1x::ic::Resolution16Bit, ads1x1x::mode::OneShot>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Values {
    pub timestamp: i64,
    pub temperature: f32,
    pub tds: f32,
}

struct Context<PIN, I2C>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    one_wire: OneWire<PIN>,
    ds18b20: Ds18b20,
    ads1115: Ads1115<I2C>,
}

const RETRY_COUNT: i32 = 3;

static VALUES: RwLock<Option<Values>> = RwLock::const_new(None);

pub(crate) async fn get() -> Option<Values> {
    *VALUES.read().await
}

pub(crate) async fn worker<PIN, I2C>(one_wire_pin: PIN, i2c: I2C) -> anyhow::Result<()>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    let mut ctx = init(one_wire_pin, i2c).await?;
    info!("Worker initialized");

    let mut interval = interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        if let Err(e) = update(&mut ctx).await {
            error!("Failed to update measurements: {e:?}");
        }
    }
}

async fn init<PIN, I2C>(one_wire_pin: PIN, i2c: I2C) -> anyhow::Result<Box<Context<PIN, I2C>>>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    task::block_in_place(move || {
        let (one_wire, ds18b20) = init_ds18b20(one_wire_pin)?;
        let ads1115 = init_ads1115(i2c)?;

        Ok(Box::new(Context {
            one_wire,
            ds18b20,
            ads1115,
        }))
    })
}

fn init_ds18b20<PIN>(pin: PIN) -> anyhow::Result<(OneWire<PIN>, Ds18b20)>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
{
    let mut one_wire = OneWire::new(pin).unwrap();
    let mut delay = Delay::new_default();

    // Retry to initialize DS18B20 up to 3 times
    for _ in 0..RETRY_COUNT {
        for address in one_wire.devices(false, &mut delay).flatten() {
            if address.family_code() != ds18b20::FAMILY_CODE {
                continue;
            }

            let ds18b20 = Ds18b20::new::<GpioError>(address).map_err(|e| anyhow!("{e:?}"))?;
            ds18b20
                .set_config(-128, 127, Resolution::Bits12, &mut one_wire, &mut delay)
                .unwrap();

            return Ok((one_wire, ds18b20));
        }

        FreeRtos::delay_ms(1000);
    }

    Err(anyhow!("DS18B20 not found"))
}

fn init_ads1115<I2C>(i2c: I2C) -> anyhow::Result<Ads1115<I2C>>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    let mut ads1115 = Ads1x1x::new_ads1115(i2c, TargetAddr::default());
    ads1115.set_full_scale_range(FullScaleRange::Within4_096V).unwrap();

    Ok(ads1115)
}

async fn update<PIN, I2C>(ctx: &mut Context<PIN, I2C>) -> anyhow::Result<()>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    let values = task::block_in_place(move || {
        let timestamp = Utc::now().timestamp_millis();
        let temperature = read_temperature(&mut ctx.one_wire, &ctx.ds18b20)?;
        let tds = read_tds(&mut ctx.ads1115, temperature)?;

        anyhow::Ok(Values {
            timestamp,
            temperature,
            tds,
        })
    })?;

    *VALUES.write().await = Some(values);

    Ok(())
}

fn read_temperature<PIN>(one_wire: &mut OneWire<PIN>, ds18b20: &Ds18b20) -> anyhow::Result<f32>
where
    PIN: InputPin<Error = GpioError> + OutputPin<Error = GpioError>,
{
    let mut err: Option<_> = None;

    for _ in 0..RETRY_COUNT {
        let mut delay = Delay::new_default();
        ds18b20
            .start_temp_measurement(one_wire, &mut delay)
            .map_err(|e| anyhow!("{e:?}"))?;

        Resolution::Bits12.delay_for_measurement_time(&mut delay);
        match ds18b20.read_data(one_wire, &mut delay) {
            Ok(data) => return Ok((data.temperature * 10.0).round() / 10.0),
            Err(e) => err = Some(e),
        }
    }

    Err(anyhow!("{:?}", err.unwrap()))
}

fn read_tds<I2C>(ads1115: &mut Ads1115<I2C>, temperature: f32) -> anyhow::Result<f32>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    const MAX_VOLTAGE: f32 = 4.096;
    const MAX_RAW_VALUE: f32 = 32767.0;

    let raw_value = nb::block!(ads1115.read(channel::SingleA0)).map_err(|e| anyhow!("{e:?}"))?;
    let voltage = f32::from(raw_value) * MAX_VOLTAGE / MAX_RAW_VALUE;

    // See https://wiki.keyestudio.com/KS0429_keyestudio_TDS_Meter_V1.0

    // temperature compensation formula: fFinalResult(25^C) = fFinalResult(current)/(1.0+0.02*(fTP-25.0));
    let coefficient = 1.0 + 0.02 * (temperature - 25.0);
    //temperature compensation
    let voltage = voltage / coefficient;
    //convert voltage value to tds value
    let tds = (133.42 * voltage.powi(3) - 255.86 * voltage.powi(2) + 857.39 * voltage) * 0.5;

    Ok(tds.round())
}
