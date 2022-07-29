use std::env;
use std::ffi::CString;
use std::sync::Arc;
use anyhow::{bail, Result};
use esp_homekit_sdk_sys::{accessory, hap, service, task};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::httpd::Configuration;
use esp_idf_svc::netif::EspNetifStack;
use esp_idf_svc::nvs::EspDefaultNvs;
use esp_idf_svc::ping::EspPing;
use esp_idf_svc::sysloop::EspSysLoopStack;
use esp_idf_svc::wifi::EspWifi;

use esp_idf_sys as _;
use spin::Mutex;

const SSID: &str = "ssid";
const PASS: &str = "password";

const SMART_OUTLET_TASK_NAME: &str = "hap_outlet";
const SMART_OUTLET_TASK_STACKSIZE: u32 = 40000;
const SMART_OUTLET_TASK_PRIORITY: UBaseType_t = 1;

static WIFI: Mutex<Option<Box<EspWifi>>> = Mutex::new(None);
static GPIO: CriticalSectionSpinLockMutex<
    Option<esp_idf_hal::gpio::Gpio8<esp_idf_hal::gpio::Output>>,
> = CriticalSectionSpinLockMutex::new(None);

fn main() -> Result<()> {
    esp_idf_sys::link_patches();

    let wifi = wifi()?;
    {
        let lock = WIFI.lock();
        *lock = Some(wifi);
    }

    task::Task::create(
        smart_outlet_handler,
        SMART_OUTLET_TASK_NAME,
        SMART_OUTLET_TASK_STACKSIZE,
        SMART_OUTLET_TASK_PRIORITY,
    );

    Ok(())
}

fn smart_outlet_handler(cv: *mut esp_homekit_sdk_sys::c_types::c_void) {
    env::set_var("RUST_BACKTRACE", "1");

    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;
    let mut switch = pins.gpio5.into_output().unwrap(); // Blue
    switch.set_low();

    use esp32_hal::gpio::Mutex;
    (&GPIO).lock(|val| *val = Some(switch));

    let hap_config = hap::Config {
        name: CString::new("Smart-Outlet").unwrap(),
        model: CString::new("Esp32").unwrap(),
        manufacturer: CString::new("Espressif").unwrap(),
        serial_num: CString::new("111122334455").unwrap(),
        fw_rev: CString::new("1.0.0").unwrap(),
        hw_rev: CString::new("0.1.0").unwrap(),
        pv: CString::new("1.1.0").unwrap(),
        cid: accessory::Category::OUTLET,
    };

    hap::init();

    let mut accessory = accessory::create(&hap_config);
    let mut service = service::create();

    service::add_name(service, "My Smart Outlet");

    let outlet_in_use = service::get_service_by_uuid(service);

    service::set_write_cb(service, Some(outlet_write));

    hap::add_service_to_accessory(accessory, service);

    hap::add_accessory(accessory);

    let setup_code = CString::new("111-22-333").unwrap();
    let setup_id = CString::new("ES32").unwrap();

    hap::secret(setup_code, setup_id);

    hap::start();

    loop {}
}

unsafe extern "C" fn outlet_write(
    write_data: *mut esp_homekit_sdk_sys::hap_write_data_t,
    count: i32,
    serv_priv: *mut esp_homekit_sdk_sys::c_types::c_void,
    write_priv: *mut esp_homekit_sdk_sys::c_types::c_void,
) -> i32 {
    use esp32_hal::gpio::Mutex;

    let mut gpio = &GPIO;

    if (*write_data).val.b == true {
        gpio.lock(|gpio| {
            let gpio = gpio.as_mut().unwrap();
            gpio.set_high();
        })
    } else {
        gpio.lock(|gpio| {
            let gpio = gpio.as_mut().unwrap();
            gpio.set_low();
        })
    }

    hap::HAP_SUCCESS_
}

fn wifi() -> Result<Box<EspWifi>> {
    let mut wifi = Box::new(EspWifi::new(
        Arc::new(EspNetifStack::new()?),
        Arc::new(EspSysLoopStack::new()?),
        Arc::new(EspDefaultNvs::new()?),
    )?);

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

    let status = wifi.get_status();

    if let Status(
        ClientStatus::Started(ClientConnectionStatus::Connected(ClientIpStatus::Done(ip_settings))),
        ApStatus::Started(ApIpStatus::Done),
    ) = status
    {
        info!("Wifi connected, about to do some pings");

        let ping_summary =
            EspPing::default().ping(ip_settings.subnet.gateway, &Default::default())?;
        if ping_summary.transmitted != ping_summary.received {
            bail!(
                "Pinging gateway {} resulted in timeouts",
                ip_settings.subnet.gateway
            );
        }

        info!("Pinging done");
    } else {
        bail!("Unexpected Wifi status: {:?}", status);
    }

    Ok(wifi)
}