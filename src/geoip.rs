use std::{net::IpAddr, path::Path};

use anyhow::{Context, Result};

/// Geographic information resolved for a client's public IP.
#[derive(Debug, Clone, Default)]
pub struct Geo {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Wrapper around a MaxMind GeoLite2 (free) database.
///
/// Lookups are local and in-process, so no client IP ever leaves the host
/// (unlike the original app, which linked out to a third-party geo service).
pub struct GeoDb {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl GeoDb {
    pub fn open(path: &Path) -> Result<Self> {
        let reader = maxminddb::Reader::open_readfile(path)
            .with_context(|| format!("opening GeoIP database {}", path.display()))?;
        Ok(GeoDb { reader })
    }

    pub fn lookup(&self, ip: &str) -> Option<Geo> {
        let addr: IpAddr = ip.parse().ok()?;
        let city: maxminddb::geoip2::City = self.reader.lookup(addr).ok()?;

        let (country_code, country_name) = city
            .country
            .as_ref()
            .map(|c| {
                let code = c.iso_code.map(str::to_string);
                let name = c
                    .names
                    .as_ref()
                    .and_then(|n| n.get("en"))
                    .map(|s| s.to_string());
                (code, name)
            })
            .unwrap_or((None, None));

        let (lat, lon) = city
            .location
            .as_ref()
            .map(|l| (l.latitude, l.longitude))
            .unwrap_or((None, None));

        Some(Geo {
            country_code,
            country_name,
            lat,
            lon,
        })
    }
}
