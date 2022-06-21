use std::sync::Arc;
use std::time::*;

use heapless::spsc::Consumer;
use log::*;
use anyhow::{bail, Result};

use embedded_hal::prelude::*;
use embedded_hal::digital::v2::*;
use embedded_hal::blocking::delay::DelayUs;

use esp_idf_hal::prelude::*;
use esp_idf_hal::gpio::*;
use esp_idf_hal::delay;


use embedded_svc::sys_time::SystemTime;
use embedded_svc::timer::*;
use embedded_svc::ipv4;
use embedded_svc::ping::Ping;
use embedded_svc::wifi::*;
use embedded_svc::mqtt::client::{Connection, Publish, QoS};


use esp_idf_svc::systime::EspSystemTime;
use esp_idf_svc::timer::*;
use esp_idf_svc::netif::*;
use esp_idf_svc::nvs::*;
use esp_idf_svc::ping;
use esp_idf_svc::sysloop::*;
use esp_idf_svc::wifi::*;
use esp_idf_svc::mqtt::client::*;

use generic_array::typenum::U5;
use median::stack::Filter;
use heapless::spsc::Queue;

const SSID: &str = env!("GREYWATER_WIFI_SSID");
const PASS: &str = env!("GREYWATER_WIFI_PASS");
const MQTT: &str = env!("GREYWATER_MQTT");

static mut CLEARWATER_QUEUE: Queue<Duration, 2> = Queue::new();

fn main() -> Result<()> {

    esp_idf_svc::log::EspLogger::initialize_default();

    let _wifi = init_wifi();

    let mqtt_conf = MqttClientConfiguration {
        client_id: Some("greywater"),

        ..Default::default()
    };

    let (mut mqtt_client, mut mqtt_conn) = EspMqttClient::new_with_conn(MQTT, &mqtt_conf)?;

    std::thread::spawn(move || {
        debug!("MQTT Listening for messages");

        while let Some(msg) = mqtt_conn.next() {
            match msg {
                Err(e) => debug!("MQTT Message ERROR: {}", e),
                Ok(msg) => debug!("MQTT Message: {:?}", msg),
            }
        }

        debug!("MQTT connection loop exit");
    });


    let peripherals = Peripherals::take().expect("Peripheral init");

    let clearwater_trig = peripherals.pins.gpio0
        .into_output()
        .unwrap()
        .degrade();

    let clearwater_echo = peripherals.pins.gpio1
        .into_input()
        .unwrap()
        .into_pull_down()
        .unwrap();

    let mut delay = delay::Ets;

    let (mut clearwater_tx, clearwater_rx) = unsafe { CLEARWATER_QUEUE.split() };

    unsafe {
        clearwater_echo.into_subscribed(move ||{
            let now = EspSystemTime {}.now();
            clearwater_tx.enqueue(now).expect("Enqueuing time");
        }, InterruptType::AnyEdge)
            .expect("Edge handler");
    }

    let mut clearwater_sensor = UltrasonicSensor { tx: clearwater_trig, rx: clearwater_rx };

    // Just let things settle
    delay.delay_ms(10u8);
    info!("Starting distance");

    let mut publish_clear_tank = move |distance: f32| {
        debug!("Publishing to mqtt");
        mqtt_client.publish(
            "greywater/clear",
            QoS::AtMostOnce,
            false,
            &distance.to_be_bytes()[..],
        ).unwrap();
        debug!("done publishing")
    };

    let mut filter: Filter<f32, U5> = Filter::new();

    let mut periodic = EspTimerService::new().expect("Setting timer service").timer(move || {
        for _ in 0..5 {
            filter.consume(clearwater_sensor.distance_in_cms());
            delay.delay_ms(100u8);
        }

        let distance = filter.median();

        info!("Median: {}", distance);
        publish_clear_tank(distance)
    }).expect("Periodic timer setup");

    periodic.every(Duration::from_secs(10)).expect("Schedule sampling");

    loop { }

    Ok(())
}

struct UltrasonicSensor {
    tx: GpioPin<Output>,
    rx: Consumer<'static, Duration, 2>
}

impl UltrasonicSensor {
    fn distance_in_cms(&mut self) -> f32 {
        let mut delay = delay::Ets;

        debug!("Starting trigger pulse");
        self.tx.set_high().expect("Starting trigger pulse");
        delay.delay_us(10u8);
        self.tx.set_low().expect("Ending trigger pulse");
        debug!("Pulse done.");

        let mut blocking_deque = move || {
            while !self.rx.ready() {}
            unsafe { self.rx.dequeue_unchecked() }
        };

        let start = blocking_deque();
        debug!("Got start: {:?}", start);
        let end = blocking_deque();
        debug!("Got end: {:?}", end);

        let raw = (end - start).as_micros() as f32 / 58.0;
        debug!("Raw: {}", raw);

        raw
    }
}


fn init_wifi() -> Result<Box<EspWifi>> {
    let netif_stack = Arc::new(EspNetifStack::new()?);
    let sys_loop_stack = Arc::new(EspSysLoopStack::new()?);
    let default_nvs = Arc::new(EspDefaultNvs::new()?);

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
