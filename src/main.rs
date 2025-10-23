// Copyright © 2025 Akira Miyakoda
//
// This software is released under the MIT License.
// https://opensource.org/licenses/MIT

use std::cell::RefCell;

use embedded_hal_bus::i2c as i2c_bus;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{gpio::PinDriver, i2c, prelude::*},
    nvs::EspDefaultNvsPartition,
};
use tokio::select;

mod display;
mod measurements;
mod network;
mod nvs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Box::new(Peripherals::take()?);
    let event_loop = Box::new(EspSystemEventLoop::take()?);
    let partition = Box::new(EspDefaultNvsPartition::take()?);
    nvs::init(*partition)?;

    let one_wire_pin = Box::new(PinDriver::input_output(peripherals.pins.gpio5)?);
    let i2c = Box::new(i2c::I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio6,
        peripherals.pins.gpio7,
        &i2c::config::Config::new()
            .baudrate(400.kHz().into())
            .scl_enable_pullup(true)
            .sda_enable_pullup(true),
    )?);
    let i2c = Box::new(RefCell::new(*i2c));
    let i2c_display = Box::new(i2c_bus::RefCellDevice::new(&i2c));
    let i2c_adc = Box::new(i2c_bus::RefCellDevice::new(&i2c));

    select! {
        result = display::worker(*i2c_display) => result,
        result = network::worker(peripherals.modem, *event_loop) => result,
        result = measurements::worker(*one_wire_pin, *i2c_adc) => result,
    }
}
