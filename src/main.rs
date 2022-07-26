use std::time::Duration;
use core::fmt::Write;

use embedded_hal::prelude::*;

use esp_idf_hal::{
    prelude::*,
    delay, i2c, serial::{Serial, config::Config as SerialConfig, Pins}
};

use embedded_svc::{
    mqtt::client::{Connection, MessageId, MessageImpl, utils::ConnState, Publish, QoS},
    timer::*,
};

use esp_idf_svc::{
    mqtt::client::*,
    timer::*,
};

use esp_idf_sys::EspError;

use anyhow::Result;
use generic_array::typenum::U5;

use log::*;
use median::stack::Filter;
use ssd1306::mode::DisplayConfig;

use greywater::{comms, sensors::{Us100, UltrasonicSensor}};


const SSID: &str = env!("GREYWATER_WIFI_SSID");
const PASS: &str = env!("GREYWATER_WIFI_PASS");
const MQTT: &str = env!("GREYWATER_MQTT");

fn main() -> Result<()> {

    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().expect("Peripheral init");

    let pins = peripherals.pins;

    // Clearwater: GPIO 0 and 1
    let mut clearwater_sensor = Us100::new(
        Serial::new(
            peripherals.uart0,
            Pins {
                tx: pins.gpio0,
                rx: pins.gpio1,
                cts: None,
                rts: None
            },
            SerialConfig::default().baudrate(Hertz(9600))
        ).expect("Setting up serial connection to us-100")
    );


    // Bioreactor: GPIO 2 and 3
    let mut bioreactor_sensor = Us100::new(
        Serial::new(
            peripherals.uart1,
            Pins {
                tx: pins.gpio2,
                rx: pins.gpio3,
                cts: None,
                rts: None
            },
            SerialConfig::default().baudrate(Hertz(9600))
        ).expect("Setting up serial connection to us-100")
    );

    // Display: GPIO 8 and 9
    let mut display = {
        let di = ssd1306::I2CDisplayInterface::new(i2c::Master::<i2c::I2C0, _, _>::new(
                peripherals.i2c0,
                i2c::MasterPins { sda: pins.gpio8, scl: pins.gpio9 },
                <i2c::config::MasterConfig as Default>::default().baudrate(400.kHz().into())
        )?);

        let mut display = ssd1306::Ssd1306::new(
            di,
            ssd1306::size::DisplaySize128x32,
            ssd1306::rotation::DisplayRotation::Rotate0
        ).into_terminal_mode();

        display.init().expect("Initializing display");
        display.clear().expect("Clearing display");
        display
    };

    let mut delay = delay::Ets;

    write!(display, "Starting wifi...")?;
    let _wifi = comms::connect_to_wifi(SSID, PASS);

    let mut publisher = SensorDataPublisher::connect(MQTT, &MqttClientConfiguration {
        client_id: Some("greywater"),

        ..Default::default()
    })?;


    // Just let things settle
    delay.delay_ms(10u8);
    info!("Starting distance");

    let mut clear_filter: Filter<f32, U5> = Filter::new();
    let mut bioreactor_filter: Filter<f32, U5> = Filter::new();

    let mut periodic = EspTimerService::new().expect("Setting timer service").timer(move || {
        debug!("Sampling");

        for _ in 0..5 {
            debug!("Consuming from clear");
            clear_filter.consume(clearwater_sensor.distance_in_cms());
            debug!("Consuming from bioreactor");
            bioreactor_filter.consume(bioreactor_sensor.distance_in_cms());
            debug!("Settling.");
            delay.delay_ms(100u8);
        }

        debug!("Checking median");
        let clear_distance = clear_filter.median();
        let bioreactor_distance = bioreactor_filter.median();

        info!("Clear Tank: {}", clear_distance);
        info!("Bioreactor Tank: {}", bioreactor_distance);
        display.clear().unwrap();
        write!(display, "Clear Tank: {:.0}cm\n\n", clear_distance).unwrap();
        write!(display, "Bioreactor: {:.0}cm\n", bioreactor_distance).unwrap();

        if let Err(err) = publisher.publish_clear_tank(clear_distance) {
            error!("Unable to publish clear tank distance: {}", err);
        }

        if let Err(err) = publisher.publish_bioreactor(bioreactor_distance) {
            error!("Unable to publish bioreactor distance: {}", err);
        }
    }).expect("Periodic timer setup");

    periodic.every(Duration::from_secs(10)).expect("Schedule sampling");

    debug!("Timer scheduled");

    loop_forever()
}

#[allow(dead_code, unreachable_code)]
fn loop_forever() -> Result<()> {
    loop { }

    Ok(())
}

#[allow(dead_code)] // The listener_handle just needs to hold the thread reference
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


