// Copyright © 2025 Akira Miyakoda
//
// This software is released under the MIT License.
// https://opensource.org/licenses/MIT

use std::{borrow::Cow, time::Duration};

use anyhow::anyhow;
use chrono::Utc;
use chrono_tz::Tz;
use embedded_graphics::{
    Drawable, geometry,
    image::ImageRaw,
    mono_font::{DecorationDimensions, MonoFont, MonoTextStyle, mapping},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, PrimitiveStyleBuilder},
    text::{Baseline, Text},
};
use esp_idf_svc::hal::i2c::I2cError;
use log::error;
use sh1106::{mode::GraphicsMode, prelude::*};
use tokio::time::MissedTickBehavior;
use tokio::{task, time::interval};

use crate::{measurements, network, nvs};

const STYLE_LINE: PrimitiveStyle<BinaryColor> = PrimitiveStyleBuilder::new()
    .stroke_width(1)
    .stroke_color(BinaryColor::On)
    .build();

const FONT_TER_14: MonoFont = MonoFont {
    image: ImageRaw::new(include_bytes!("../fonts/ter-u14b.raw"), 128),
    glyph_mapping: &mapping::ISO_8859_1,
    character_size: geometry::Size::new(8, 14),
    character_spacing: 0,
    baseline: 11,
    underline: DecorationDimensions::new(11 + 2, 1),
    strikethrough: DecorationDimensions::new(14 / 2, 1),
};
const STYLE_TER_14: MonoTextStyle<BinaryColor> = MonoTextStyle::new(&FONT_TER_14, BinaryColor::On);

const FONT_TER_24: MonoFont = MonoFont {
    image: ImageRaw::new(include_bytes!("../fonts/ter-u24b.raw"), 192),
    glyph_mapping: &mapping::ISO_8859_1,
    character_size: geometry::Size::new(12, 24),
    character_spacing: 0,
    baseline: 19,
    underline: DecorationDimensions::new(19 + 2, 1),
    strikethrough: DecorationDimensions::new(24 / 2, 1),
};
const STYLE_TER_24: MonoTextStyle<BinaryColor> = MonoTextStyle::new(&FONT_TER_24, BinaryColor::On);

pub(crate) struct Context<I2C>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    graphics: GraphicsMode<I2cInterface<I2C>>,
    timezone: Tz,
}

pub(crate) fn init<I2C>(i2c: I2C) -> anyhow::Result<Box<Context<I2C>>>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    task::block_in_place(move || {
        let mut graphics: GraphicsMode<_> = sh1106::Builder::new().connect_i2c(i2c).into();
        graphics.init().unwrap();
        graphics.clear();
        graphics.flush().unwrap();

        let timezone = nvs::get("timezone")?;
        let timezone: Tz = timezone.parse()?;

        Ok(Box::new(Context { graphics, timezone }))
    })
}

pub(crate) async fn greet<I2C>(ctx: &mut Box<Context<I2C>>) -> anyhow::Result<()>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    task::block_in_place(move || {
        let graphics = &mut ctx.graphics;

        graphics.clear();

        Text::with_baseline("Cobitis v1.2", Point::new(16, 18), STYLE_TER_14, Baseline::Top).draw(graphics)?;
        Text::with_baseline("Starting...", Point::new(20, 36), STYLE_TER_14, Baseline::Top).draw(graphics)?;

        ctx.graphics.flush().map_err(|e| anyhow!("{e:?}"))?;

        Ok(())
    })
}

pub(crate) async fn worker<I2C>(ctx: &mut Box<Context<I2C>>) -> anyhow::Result<()>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    let mut interval = interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        if let Err(e) = draw(ctx).await {
            error!("Failed to draw: {e:?}");
        }
    }
}

async fn draw<I2C>(ctx: &mut Context<I2C>) -> anyhow::Result<()>
where
    I2C: embedded_hal::i2c::I2c<Error = I2cError>,
{
    let (temp, tds) = {
        let m = measurements::get().await;
        (m.map(|m| m.temperature), m.map(|m| m.tds))
    };
    let signal_level: i32 = {
        let v = network::get().await;
        v.map(|v| v.signal_quality).unwrap_or_default().into()
    };

    task::block_in_place(move || {
        let graphics = &mut ctx.graphics;

        graphics.clear();

        // Draw date & time
        let text = Utc::now()
            .with_timezone(&ctx.timezone)
            .format("%m/%d %H:%M")
            .to_string();
        Text::with_baseline(&text, Point::new(10, 0), STYLE_TER_14, Baseline::Top).draw(graphics)?;

        // Draw signal quality bars
        for i in 1..=signal_level {
            let x = 107 + i * 2;
            let y = 12 - i * 2;
            Line::new(Point::new(x, y), Point::new(x, 11))
                .into_styled(STYLE_LINE)
                .draw(graphics)?;
        }

        // Draw temperature
        let text: Cow<_> = if let Some(v) = temp {
            format!("{v:>7.1}").into()
        } else {
            "    -.-".into()
        };

        Text::with_baseline(&text, Point::new(0, 16), STYLE_TER_24, Baseline::Top).draw(graphics)?;
        Text::with_baseline(&text, Point::new(1, 16), STYLE_TER_24, Baseline::Top).draw(graphics)?;
        Text::with_baseline("°C", Point::new(89, 23), STYLE_TER_14, Baseline::Top).draw(graphics)?;

        // Draw TDS
        let text: Cow<_> = if let Some(v) = tds {
            format!("{v:>7.0}").into()
        } else {
            "      -".into()
        };

        Text::with_baseline(&text, Point::new(0, 40), STYLE_TER_24, Baseline::Top).draw(graphics)?;
        Text::with_baseline(&text, Point::new(1, 40), STYLE_TER_24, Baseline::Top).draw(graphics)?;
        Text::with_baseline("ppm", Point::new(90, 47), STYLE_TER_14, Baseline::Top).draw(graphics)?;

        ctx.graphics.flush().map_err(|e| anyhow!("{e:?}"))?;

        Ok(())
    })
}
