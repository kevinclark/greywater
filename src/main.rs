use std::time::*;


use anyhow::Result;

use embedded_hal::prelude::*;
use log::*;


use embedded_hal::digital::v2::*;
use embedded_hal::blocking::delay::DelayUs;

use embedded_svc::event_bus::{EventBus, Postbox};
use embedded_svc::sys_time::SystemTime;

use esp_idf_hal::prelude::*;
use esp_idf_hal::gpio::*;
use esp_idf_hal::delay;

use esp_idf_svc::eventloop::*;
use esp_idf_svc::systime::EspSystemTime;

use esp_idf_sys::c_types;

use heapless::spsc::Queue;

static mut Q: Queue<Duration, 2> = Queue::new();

fn main() -> ! {

    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().expect("Peripheral init");

    let mut delay = delay::Ets;

    let mut trig = peripherals.pins.gpio0
        .into_output()
        .unwrap();

    let echo = peripherals.pins.gpio1
        .into_input()
        .unwrap()
        .into_pull_down()
        .unwrap();

    let (mut tx, mut rx) = unsafe { Q.split() };

    unsafe {
        echo.into_subscribed(move ||{
            let now = EspSystemTime {}.now();
            tx.enqueue(now).expect("Enqueuing time");
        }, InterruptType::AnyEdge)
            .expect("Edge handler");
    }

    // Just let things settle
    delay.delay_ms(10u8);
    println!("Starting distance");

    let mut blocking_deque = move || {
        while !rx.ready() {}
        unsafe { rx.dequeue_unchecked() }
    };

    loop {
        debug!("Starting trigger pulse");
        trig.set_high().expect("Starting trigger pulse");
        delay.delay_us(10u8);
        trig.set_low().expect("Ending trigger pulse");
        debug!("Pulse done.");

        let start = blocking_deque();
        debug!("Got start: {:?}", start);
        let end = blocking_deque();
        debug!("Got end: {:?}", end);

        println!("{}cm", (end - start).as_micros() / 58);

        delay.delay_ms(1000u16);
    }
}

struct Ultrasonic {

}
