//! Where LectureLive's requests to api.x.ai connect, when a check puts a forwarder in between (M6 plan, the
//! error table): `LECTURELIVE_API_ADDR=127.0.0.1:8443` sends the STT stream, recovery and the notes there, TLS
//! still checking the real host, so the network can be taken away from LectureLive alone.
use std::net::SocketAddr;

pub const API_ADDR_VAR: &str = "LECTURELIVE_API_ADDR";

pub fn api_addr() -> Option<SocketAddr> {
    std::env::var(API_ADDR_VAR).ok()?.parse().ok()
}

/// A client that connects to `connect_to` for `url`'s host, when one is given.
pub fn resolved(b: reqwest::ClientBuilder, url: &str, connect_to: Option<SocketAddr>) -> reqwest::ClientBuilder {
    match (connect_to, reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string))) {
        (Some(addr), Some(host)) => b.resolve(&host, addr),
        _ => b,
    }
}
