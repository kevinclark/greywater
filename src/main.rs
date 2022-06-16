use std::{env, sync::Arc, time::*};


use anyhow::{bail, Result};

use embedded_hal::prelude::_embedded_hal_blocking_delay_DelayMs;
use log::*;


use embedded_hal::digital::v2::*;
use embedded_hal::blocking::delay::DelayUs;

use embedded_svc::ipv4;
use embedded_svc::ping::Ping;
use embedded_svc::wifi::*;

use esp_idf_hal::prelude::*;
use esp_idf_hal::gpio::*;
use esp_idf_hal::delay;

use esp_idf_svc::netif::*;
use esp_idf_svc::nvs::*;
use esp_idf_svc::ping;
use esp_idf_svc::sysloop::*;
use esp_idf_svc::wifi::*;

const SSID: &str = env!("DISTANCE_SSID");
const PASS: &str = env!("DISTANCE_PASS");


fn main() -> Result<()> {

    esp_idf_svc::log::EspLogger::initialize_default();

    let netif_stack = Arc::new(EspNetifStack::new()?);
    let sys_loop_stack = Arc::new(EspSysLoopStack::new()?);
    let default_nvs = Arc::new(EspDefaultNvs::new()?);

    let _wifi =
        wifi(netif_stack.clone(), sys_loop_stack.clone(), default_nvs.clone())
            .expect("Wifi setup");

    let peripherals = Peripherals::take().expect("Peripheral init");

    let mut delay = delay::Ets;
    let mut trig = peripherals.pins.gpio0.into_output()?;
    let echo = peripherals.pins.gpio1.into_input()?.into_pull_down()?;

    // Just let things settle
    delay.delay_ms(10u8);
    println!("Starting distance");

    loop {
        trig.set_high().expect("Starting trigger pulse");
        delay.delay_us(10u8);
        trig.set_low().expect("Ending trigger pulse");

        while echo.is_low()? {}

        let start = SystemTime::now();

        while echo.is_high()? {}

        let end = SystemTime::now();

        let micros = end.duration_since(start)?.as_micros();
        println!("{}us {}cm", micros, micros / 58);

        delay.delay_ms(1000u16);
    }



    Ok(())
}

fn wifi(
    netif_stack: Arc<EspNetifStack>,
    sys_loop_stack: Arc<EspSysLoopStack>,
    default_nvs: Arc<EspDefaultNvs>,
) -> Result<Box<EspWifi>> {
    let mut wifi = Box::new(EspWifi::new(netif_stack, sys_loop_stack, default_nvs)?);

    info!("Wifi created, about to scan");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == SSID);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            SSID, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            SSID
        );
        None
    };

    wifi.set_configuration(&Configuration::Mixed(
        ClientConfiguration {
            ssid: SSID.into(),
            password: PASS.into(),
            channel,
            ..Default::default()
        },
        AccessPointConfiguration {
            ssid: "aptest".into(),
            channel: channel.unwrap_or(1),
            ..Default::default()
        },
    ))?;

    info!("Wifi configuration set, about to get status");

    wifi.wait_status_with_timeout(Duration::from_secs(20), |status| !status.is_transitional())
        .map_err(|e| anyhow::anyhow!("Unexpected Wifi status: {:?}", e))?;

    let status = wifi.get_status();

    if let Status(
        ClientStatus::Started(ClientConnectionStatus::Connected(ClientIpStatus::Done(ip_settings))),
        ApStatus::Started(ApIpStatus::Done),
    ) = status
    {
        info!("Wifi connected");

        ping(&ip_settings)?;

    } else {
        bail!("Unexpected Wifi status: {:?}", status);
    }

    Ok(wifi)
}

fn ping(ip_settings: &ipv4::ClientSettings) -> Result<()> {
    info!("About to do some pings for {:?}", ip_settings);

    let ping_summary =
        ping::EspPing::default().ping(ip_settings.subnet.gateway, &Default::default())?;
    if ping_summary.transmitted != ping_summary.received {
        bail!(
            "Pinging gateway {} resulted in timeouts",
            ip_settings.subnet.gateway
        );
    }

    info!("Pinging done");

    Ok(())
}
