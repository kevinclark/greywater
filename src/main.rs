use std::sync::Arc;
use std::time::*;

use embedded_hal::{
    prelude::*,
    digital::v2::*,
    blocking::delay::DelayUs
};

use esp_idf_hal::{
    prelude::*,
    gpio::*,
    delay
};

use embedded_svc::{
    ipv4,
    mqtt::client::{Connection, MessageId, MessageImpl, utils::ConnState, Publish, QoS},
    ping::Ping,
    sys_time::SystemTime,
    timer::*,
    wifi::*
};

use esp_idf_svc::{
    mqtt::client::*,
    netif::*,
    nvs::*,
    ping,
    sysloop::*,
    systime::EspSystemTime,
    timer::*,
    wifi::*
};

use esp_idf_sys::EspError;

use anyhow::{bail, Result};
use generic_array::typenum::U5;
use heapless::spsc::{Queue, Consumer};
use log::*;
use median::stack::Filter;

const SSID: &str = env!("GREYWATER_WIFI_SSID");
const PASS: &str = env!("GREYWATER_WIFI_PASS");
const MQTT: &str = env!("GREYWATER_MQTT");

static mut CLEAN_TANK_QUEUE: Queue<Duration, 2> = Queue::new();
static mut BIOREACTOR_TANK_QUEUE: Queue<Duration, 2> = Queue::new();

fn main() -> Result<()> {

    esp_idf_svc::log::EspLogger::initialize_default();

    let _wifi = init_wifi();

    let mut publisher = SensorDataPublisher::connect(MQTT, &MqttClientConfiguration {
        client_id: Some("greywater"),

        ..Default::default()
    })?;

    let peripherals = Peripherals::take().expect("Peripheral init");

    let mut clearwater_sensor =
        ultrasonic_sensor!(
            peripherals.pins.gpio0,
            peripherals.pins.gpio1,
            CLEAN_TANK_QUEUE);

    let mut bioreactor_sensor =
        ultrasonic_sensor!(
            peripherals.pins.gpio2,
            peripherals.pins.gpio3,
            BIOREACTOR_TANK_QUEUE);

    let mut delay = delay::Ets;

    // Just let things settle
    delay.delay_ms(10u8);
    info!("Starting distance");

    let mut clear_filter: Filter<f32, U5> = Filter::new();
    let mut bioreactor_filter: Filter<f32, U5> = Filter::new();

    let mut periodic = EspTimerService::new().expect("Setting timer service").timer(move || {
        debug!("Sampling");

        for _ in 0..5 {
            debug!("Consuming");
            clear_filter.consume(clearwater_sensor.distance_in_cms());
            bioreactor_filter.consume(bioreactor_sensor.distance_in_cms());
            delay.delay_ms(100u8);
        }

        debug!("Checking median");
        let clear_distance = clear_filter.median();
        let bioreactor_distance = bioreactor_filter.median();

        info!("Clear Tank: {}", clear_distance);
        info!("Bioreactor Tank: {}", bioreactor_distance);
        publisher.publish_clear_tank(clear_distance).unwrap();
        publisher.publish_bioreactor(bioreactor_distance).unwrap();
    }).expect("Periodic timer setup");

    periodic.every(Duration::from_secs(10)).expect("Schedule sampling");

    debug!("Timer scheduled");

    loop { }

    Ok(())
}

// This is necessarily a macro because the pins are different *types*
// with no common traits that would let me write a more generic helper
// function or struct. So that's cool...
#[macro_export]
macro_rules! ultrasonic_sensor {
    ($trigger_pin:expr, $echo_pin:expr, $queue:expr) => {
        {
            let (mut tx, response) = unsafe { $queue.split() };

            let trigger_pin = $trigger_pin
                .into_output()
                .unwrap()
                .degrade();

            let echo_pin = $echo_pin
                .into_input()
                .unwrap()
                .into_pull_down()
                .unwrap();

            unsafe {
                echo_pin.into_subscribed(move ||{
                    let now = EspSystemTime {}.now();
                    tx.enqueue(now).expect("Enqueuing time");
                }, InterruptType::AnyEdge)
                    .expect("Edge handler");
            }

            UltrasonicSensor { trigger_pin, response }
        }
    };
}

struct UltrasonicSensor {
    trigger_pin: GpioPin<Output>,
    response: Consumer<'static, Duration, 2>
}

impl UltrasonicSensor {
    fn distance_in_cms(&mut self) -> f32 {
        debug!("Starting trigger pulse");
        self.trigger_pin.set_high().expect("Starting trigger pulse");
        delay::Ets.delay_us(10u8);
        self.trigger_pin.set_low().expect("Ending trigger pulse");
        debug!("Pulse done.");

        let mut blocking_dequeue = move || {
            while !self.response.ready() {}
            unsafe { self.response.dequeue_unchecked() }
        };

        let start = blocking_dequeue();
        debug!("Got start: {:?}", start);
        let end = blocking_dequeue();
        debug!("Got end: {:?}", end);

        let raw = (end - start).as_micros() as f32 / 58.0;
        debug!("Raw: {}", raw);

        raw
    }
}

struct SensorDataPublisher {
    mqtt_client: EspMqttClient<ConnState<MessageImpl, EspError>>,
    listener_handle: std::thread::JoinHandle<()>
}

impl SensorDataPublisher {
    fn connect(address: &str, config: &MqttClientConfiguration) -> Result<Self> {
        let (mqtt_client, mut mqtt_conn) =
            EspMqttClient::new_with_conn(address, config)?;

        let listener_handle = std::thread::spawn(move || {
            debug!("MQTT Listening for messages");

            while let Some(msg) = mqtt_conn.next() {
                match msg {
                    Err(e) => debug!("MQTT Message ERROR: {}", e),
                    Ok(msg) => debug!("MQTT Message: {:?}", msg),
                }
            }

            debug!("MQTT connection loop exit");
        });

        Ok(SensorDataPublisher { mqtt_client, listener_handle })
    }

    fn publish_clear_tank(&mut self, distance: f32) -> Result<MessageId> {
        self.publish("greywater/clean-tank", distance)
    }

    fn publish_bioreactor(&mut self, distance: f32) -> Result<MessageId> {
        self.publish("greywater/bioreactor", distance)
    }

    fn publish(&mut self, topic: &str, distance: f32) -> Result<MessageId> {
        debug!("Publishing to mqtt topic: {}", topic);

        let result = self.mqtt_client.publish(
            topic,
            QoS::AtMostOnce,
            false,
            format!("{{ \"raw_distance\": {} }}", distance).as_bytes(),
        )?;

        debug!("done publishing");
        Ok(result)
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
