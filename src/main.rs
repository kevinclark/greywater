use std::time::*;


use anyhow::Result;

use embedded_hal::prelude::*;
use log::*;


use embedded_hal::digital::v2::*;
use embedded_hal::blocking::delay::DelayUs;

use esp_idf_hal::prelude::*;
use esp_idf_hal::gpio::*;
use esp_idf_hal::delay;

use embedded_svc::sys_time::SystemTime;
use embedded_svc::timer::*;

use esp_idf_svc::systime::EspSystemTime;
use esp_idf_svc::timer::*;

use generic_array::typenum::U5;
use median::stack::Filter;
use heapless::spsc::Queue;

static mut Q: Queue<Duration, 2> = Queue::new();

fn main() -> ! {

    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().expect("Peripheral init");

    let mut trig = peripherals.pins.gpio0
        .into_output()
        .unwrap();

    let echo = peripherals.pins.gpio1
        .into_input()
        .unwrap()
        .into_pull_down()
        .unwrap();

    let mut delay = delay::Ets;

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

    let mut distance_in_cms = move || {
        let mut delay = delay::Ets;

        debug!("Starting trigger pulse");
        trig.set_high().expect("Starting trigger pulse");
        delay.delay_us(10u8);
        trig.set_low().expect("Ending trigger pulse");
        debug!("Pulse done.");

        let start = blocking_deque();
        debug!("Got start: {:?}", start);
        let end = blocking_deque();
        debug!("Got end: {:?}", end);

        let raw = (end - start).as_micros() as f32 / 58.0;
        println!("Raw: {}", raw);

        raw
    };

    let mut filter: Filter<f32, U5> = Filter::new();

    let mut periodic_timer = EspTimerService::new().unwrap().timer(move || {
        for _ in 0..5 {
            filter.consume(distance_in_cms());
            delay.delay_ms(100u8);
        }

        println!("Median: {}", filter.median());
    }).expect("Periodic timer setup");

    periodic_timer.every(Duration::from_secs(10)).unwrap();

    loop { }
}

