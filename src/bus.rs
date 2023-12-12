use crate::{Result, ACCESSIBLE_ROOT_PATH};
use atspi::{
    proxy::{accessible::AccessibleProxy, application::ApplicationProxy},
    Role,
};
use float_pretty_print::PrettyPrintFloat;
use std::{fmt::Formatter, sync::Arc, time::Duration};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use zbus::{names::BusName, Connection, ProxyBuilder};

#[derive(Debug, Clone, Default)]
pub struct ResponseStats {
    pub samples: u32,
    pub sum: Duration,
    pub min: Option<Duration>,
    pub max: Option<Duration>,
    pub mean: Option<Duration>,
    pub sosd: u128, // sum of squared differences
    pub std_dev: Option<Duration>,
}

// Pretty print ResponseTimeStats.:
impl std::fmt::Display for ResponseStats {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Convert to the appropriate unit. Three digits and three decimals.
        let to_pretty = |duration: Duration| -> String {
            if duration.as_secs() > 0 {
                let millis = duration.as_millis() as f64 / 1_000.0;
                format!("{:10}s", PrettyPrintFloat(millis))
            } else if duration.as_millis() > 0 {
                let micros = duration.as_micros() as f64 / 1_000.0;
                format!("{:10}ms", PrettyPrintFloat(micros))
            } else if duration.as_micros() > 0 {
                let nanos = duration.as_nanos() as f64 / 1_000.0;
                format!("{:10}us", PrettyPrintFloat(nanos))
            } else if duration.as_nanos() > 0 {
                let nanos = duration.as_nanos();
                format!("{:10}ns", nanos)
            } else {
                format!("{:10}ns", PrettyPrintFloat(0.0f64))
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

#[derive(Debug)]
pub struct Server {
    pub accessible_name: String,
    pub bus_name: zbus::names::OwnedBusName,
    pub accessible_proxy: AccessibleProxy<'static>,
    pub application_proxy: ApplicationProxy<'static>,

    pub stats: ResponseStats,
}

#[allow(dead_code)]
impl Server {
    pub async fn get_role(&self) -> zbus::Result<Role> {
        self.accessible_proxy.get_role().await
    }

    /// Refers to the name property of the accessible object.
    /// Not the bus name.
    pub async fn name(&self) -> zbus::Result<String> {
        self.accessible_proxy.name().await
    }

    pub async fn acquire_rtt(&self) -> Option<Duration> {
        let deadline = Duration::from_millis(50);
        let start = std::time::Instant::now();

        if timeout(deadline, self.get_role()).await.is_ok() {
            return Some(start.elapsed());
        }

        None
    }

    pub fn update_rtt_stats(&mut self, res: Duration) {
        if self.stats.min.is_none() || res < self.stats.min.unwrap() {
            self.stats.min.replace(res);
        }
        if self.stats.max.is_none() || res > self.stats.max.unwrap() {
            self.stats.max.replace(res);
        }

        self.stats.sum += res;
        self.stats.samples += 1;

        let mean = self.stats.sum / self.stats.samples;
        self.stats.mean.replace(mean);

        // let diff = res.abs_diff(mean); // unstable feature
        let diff = if res > mean { res - mean } else { mean - res };

        // calculate sum of squared differences, "sosd"
        self.stats.sosd += diff.as_nanos() * diff.as_nanos();

        let variance_nanos = self.stats.sosd as f64 / self.stats.samples as f64;

        let std_dev = variance_nanos.sqrt().round() as u64;
        self.stats.std_dev.replace(Duration::from_nanos(std_dev));
    }
}

#[derive(Debug)]
pub struct Servers {
    pub bus: Vec<Arc<AsyncMutex<Server>>>,
}

impl Servers {
    pub async fn new(conn: &Connection) -> Result<Servers> {
        let registry_as_accessible: AccessibleProxy = ProxyBuilder::new(conn)
            .interface("org.a11y.atspi.Accessible")?
            .path(ACCESSIBLE_ROOT_PATH)?
            .destination("org.a11y.atspi.Registry")?
            .build()
            .await?;

        // Registry considers all accessible programs on the bus its children.
        let a11ies = registry_as_accessible.get_children().await?;
        let mut bus: Vec<Arc<AsyncMutex<Server>>> = Vec::with_capacity(a11ies.len());

        for a11y in a11ies {
            let name = a11y.name.clone();
            let name = name.trim().to_string(); // Remove whitespace.
            let accessible_proxy: AccessibleProxy = ProxyBuilder::new(conn)
                .interface("org.a11y.atspi.Accessible")?
                .path(ACCESSIBLE_ROOT_PATH)?
                .destination(name.clone())?
                .build()
                .await?;

            // Skip if the accessible application does not expose a `name` property.
            let Ok(accessible_name) = accessible_proxy.name().await else {
                continue;
            };

            let Ok(application_proxy) = zbus::ProxyBuilder::new(conn)
                .interface("org.a11y.atspi.Application")?
                .path(ACCESSIBLE_ROOT_PATH)?
                .destination(name.clone())?
                .build()
                .await
            else {
                continue;
            };

            let bus_name = BusName::try_from(a11y.name.clone())?;

            let server = Server {
                accessible_name,
                bus_name: bus_name.into(),
                accessible_proxy,
                application_proxy,
                stats: ResponseStats::default(),
            };

            let server = Arc::new(AsyncMutex::new(server));
            bus.push(server);
        }

        Ok(Servers { bus })
    }

    #[allow(dead_code)]
    pub fn get_server(&self, name: &str) -> Option<Arc<AsyncMutex<Server>>> {
        for server in self.bus.iter() {
            let guard = server.blocking_lock();
            if guard.bus_name == name {
                return Some(server.clone());
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn remove_server(&mut self, name: &str) {
        self.bus.retain(|server| {
            let guard = server.blocking_lock();
            guard.bus_name != name
        });
    }
}
