use crate::{Result, ACCESSIBLE_ROOT_PATH};
use atspi::{
    proxy::{accessible::AccessibleProxy, application::ApplicationProxy},
    Role,
};
use float_pretty_print::PrettyPrintFloat;
use futures_lite::future::block_on;
use std::sync::Mutex;
use std::{fmt::Formatter, sync::Arc, time::Duration};
use zbus::{Connection, ProxyBuilder};

#[derive(Debug, Clone, Default)]
pub struct ResponseStats {
    pub samples: u32,
    pub sum: Duration,
    pub min: Option<Duration>,
    pub max: Option<Duration>,
    pub mean: Option<Duration>,
    pub variance: u128,
    pub std_dev: Option<Duration>,
}

// Pretty print ResponseTimeStats.:
impl std::fmt::Display for ResponseStats {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Convert to the appropriate unit. Three digits and three decimals.
        let to_pretty = |duration: Duration| -> String {
            if duration.as_secs() > 0 {
                let millis = duration.as_millis() as f64 / 1_000.0;
                format!("{}s", PrettyPrintFloat(millis))
            } else if duration.as_millis() > 0 {
                let micros = duration.as_micros() as f64 / 1_000.0;
                format!("{}ms", PrettyPrintFloat(micros))
            } else if duration.as_micros() > 0 {
                let nanos = duration.as_nanos() as f64 / 1_000.0;
                format!("{:5.2}us", PrettyPrintFloat(nanos))
            } else if duration.as_nanos() > 0 {
                let nanos = duration.as_nanos();
                format!("{}ns", nanos)
            } else {
                format!("{}ns", PrettyPrintFloat(0.0f64))
            }
        };

        let min = self.min.unwrap_or(Duration::from_secs(0));
        let max = self.max.unwrap_or(Duration::from_secs(0));
        let mean = self.mean.unwrap_or(Duration::from_secs(0));
        let std_dev = self.std_dev.unwrap_or(Duration::from_secs(0));

        write!(
            f,
            "min: {} max: {} avg: {} σ: {}",
            to_pretty(min),
            to_pretty(max),
            to_pretty(mean),
            to_pretty(std_dev)
        )
    }
}

#[derive(Debug, Clone)]
pub struct BusCitizen {
    pub accessible_name: String,
    pub bus_name: zbus::names::OwnedBusName,
    pub accessible_proxy: AccessibleProxy<'static>,
    pub application_proxy: ApplicationProxy<'static>,

    pub stats: ResponseStats,
}

#[allow(dead_code)]
impl BusCitizen {
    pub async fn get_role(&self) -> zbus::Result<Role> {
        self.accessible_proxy.get_role().await
    }

    /// Refers to the name property of the accessible object.
    /// Not the bus name.
    pub async fn name(&self) -> zbus::Result<String> {
        self.accessible_proxy.name().await
    }

    pub fn acquire_rtt(&mut self) -> Option<Duration> {
        let start = std::time::Instant::now();
        if block_on(self.get_role()).is_ok() {
            return Some(start.elapsed());
        }

        None
    }

    pub fn update_rtt_stats(&mut self, res: Duration) {
        if self.stats.min.is_none() || self.stats.min.unwrap() > res {
            self.stats.min.replace(res);
        }
        if self.stats.max.is_none() || self.stats.max.unwrap() < res {
            self.stats.max.replace(res);
        }

        self.stats.sum += res;
        self.stats.samples += 1;
        self.stats.mean = Some(self.stats.sum / self.stats.samples);

        let diff = res - self.stats.mean.unwrap();
        self.stats.variance += diff.as_nanos() * diff.as_nanos();

        // Calculate standard deviation
        self.stats.std_dev = {
            let variance_nanos = self.stats.variance as f64;
            let variance_sqrt = variance_nanos.sqrt();
            let standard_deviation_nanos = variance_sqrt.round() as u64;
            Some(Duration::from_nanos(standard_deviation_nanos))
        };
    }
}

#[derive(Debug)]
pub struct BusCitizens {
    pub citizens: Vec<Arc<Mutex<BusCitizen>>>,
}

#[allow(dead_code)]
impl BusCitizens {
    pub async fn new(conn: &Connection) -> Result<BusCitizens> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(conn).await?;
        let bus_names = dbus_proxy.list_names().await?;

        let mut citizens: Vec<Arc<Mutex<BusCitizen>>> = Vec::with_capacity(bus_names.len());

        for bus_name in bus_names {
            if bus_name.starts_with('x') {
                let accessible_proxy = ProxyBuilder::new(conn)
                    .interface("org.a11y.atspi.Accessible")?
                    .path(ACCESSIBLE_ROOT_PATH)?
                    .destination(bus_name.clone())?
                    .build()
                    .await;

                if accessible_proxy.is_err() {
                    continue;
                };

                let accessible_proxy: AccessibleProxy = accessible_proxy.unwrap();

                // Might return an accessible_proxy even if the bus_name is not an AT-SPI2
                // application. So call get_application() to check.

                if accessible_proxy.get_application().await.is_err() {
                    continue;
                };

                let Ok(accessible_name) = accessible_proxy.name().await else {
                    continue;
                };

                let Ok(application_proxy) = zbus::ProxyBuilder::new(conn)
                    .interface("org.a11y.atspi.Application")?
                    .path(ACCESSIBLE_ROOT_PATH)?
                    .destination(bus_name.clone())?
                    .build()
                    .await
                else {
                    continue;
                };

                let citizen = BusCitizen {
                    accessible_name,
                    bus_name,
                    accessible_proxy,
                    application_proxy,
                    stats: ResponseStats::default(),
                };

                let citizen = Arc::new(Mutex::new(citizen));
                citizens.push(citizen);
            }
        }
        citizens.shrink_to_fit();

        #[cfg(debug_assertions)]
        for citizen in citizens.iter() {
            let guard = citizen.lock().unwrap();
            println!("{}: {}", guard.bus_name, guard.accessible_name);
        }

        Ok(BusCitizens { citizens })
    }

    pub fn get_citizen(&self, name: &str) -> Option<Arc<Mutex<BusCitizen>>> {
        for citizen in self.citizens.iter() {
            let guard = citizen.lock().unwrap();
            if guard.bus_name == name {
                return Some(citizen.clone());
            }
        }
        None
    }
}
