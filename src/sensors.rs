use std::time::Duration;

use embedded_hal::{
    digital::v2::OutputPin as _,
    blocking::delay::DelayUs, prelude::{_embedded_hal_serial_Write, _embedded_hal_serial_Read}
};

use esp_idf_hal::{
    gpio::{GpioPin, Output, OutputPin, InputPin},
    delay,
    serial::{Serial, Uart, Tx, Rx}
};

use log::*;
use nb::block;
use heapless::spsc::Consumer;

pub trait UltrasonicSensor {
    fn distance_in_cms(&mut self) -> f32;
}

// NOTE: This is necessarily a macro because the pins are different *types*
// with no common traits that would let me write a more generic helper
// function or struct. So that's cool...
#[macro_export]
macro_rules! hc_sr04 {
    ($trigger_pin:expr, $echo_pin:expr, $queue:expr) => {
        {
            let (mut tx, response) = unsafe { $queue.split() };

            let trigger_pin = $trigger_pin
                .into_output()
                .expect("Setting trigger pin as output")
                .degrade();

            let echo_pin = $echo_pin
                .into_input()
                .expect("Setting echo pin as input")
                .into_pull_down()
                .expect("Enabling echo pin pull down");

            unsafe {
                echo_pin.into_subscribed(move ||{
                    let now = EspSystemTime {}.now();
                    tx.enqueue(now).expect("Enqueuing time");
                }, InterruptType::AnyEdge)
                    .expect("Setting edge interrupt for echo pin");
            }

            $crate::sensors::HcSr04::new(trigger_pin, response)
        }
    };
}

// Driver for any HcSr04 compatible device (RCWL-1601, US-100 (without UART)).
pub struct HcSr04 {
    trigger_pin: GpioPin<Output>,
    response: Consumer<'static, Duration, 2>
}

impl HcSr04 {
    pub fn new(trigger_pin: GpioPin<Output>, response: Consumer<'static, Duration, 2>) -> HcSr04 {
        HcSr04 { trigger_pin, response }
    }
}

impl UltrasonicSensor for HcSr04 {
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

pub struct Us100<UART: Uart> {
    tx: Tx<UART>,
    rx: Rx<UART>
}

impl<UART: Uart> Us100<UART> {
    pub fn new<TX: OutputPin, RX: InputPin>(serial: Serial<UART, TX, RX>) -> Us100<UART> {
        // TODO: Should I apply the correct baud here?
        let (tx, rx) = serial.split();
        Us100 { tx, rx }
    }
}
impl<UART: Uart> UltrasonicSensor for Us100<UART> {
    fn distance_in_cms(&mut self) -> f32 {
        debug!("US100: Sending bytes");
        block!(self.tx.write(0x55)).expect("Failed to send to serial connection");
        block!(self.tx.flush()).expect("Failed flush");
        debug!("US100: Done sending bytes");
        debug!("US100: Reading first byte");
        let first = block!(self.rx.read()).expect("Reading first byte");
        debug!("US100: Reading second byte");
        let second = block!(self.rx.read()).expect("Reading second byte");
        let mms = ((first as u16) << 8) | second as u16;

        mms as f32 / 100 as f32
    }
}

