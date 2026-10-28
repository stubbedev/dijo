//! `dijo health`: the check a container runs to tell whether its server is up, as the image
//! carries no curl to ask `/healthz` with.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Empty;
use hyper::header::HOST;
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::cli::HealthArgs;
use crate::server::{LIVENESS_PATH, READINESS_PATH};

/// Asks the server for its health, reporting on standard error why it is not healthy.
pub fn check(args: &HealthArgs) -> bool {
    match probe(args) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("dijo: health: {error}");
            false
        }
    }
}

fn probe(args: &HealthArgs) -> Result<(), String> {
    let address = reachable(args.listen)?;
    let path = if args.ready {
        READINESS_PATH
    } else {
        LIVENESS_PATH
    };
    let url = format!("http://{address}{path}");
    let timeout = Duration::from_secs(args.timeout.get());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start the I/O runtime: {e}"))?;
    let status = runtime
        .block_on(async { tokio::time::timeout(timeout, get(address, path)).await })
        .map_err(|_| format!("{url} did not answer within {}s", timeout.as_secs()))?
        .map_err(|e| format!("{url}: {e}"))?;
    if status == StatusCode::OK {
        Ok(())
    } else {
        Err(format!("{url} answered {status}"))
    }
}

/// The address a server listening on `listen` is reached at from this host: an unspecified
/// address listens on loopback too.
fn reachable(listen: SocketAddr) -> Result<SocketAddr, String> {
    if listen.port() == 0 {
        return Err(format!(
            "{listen} names no port to check; give the port the server listens on"
        ));
    }
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Ok(SocketAddr::new(ip, listen.port()))
}

async fn get(address: SocketAddr, path: &str) -> Result<StatusCode, String> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("Failed to connect: {e}"))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(stream))
            .await
            .map_err(|e| format!("Failed to start HTTP: {e}"))?;
    tokio::spawn(connection);
    let request = Request::get(path)
        .header(HOST, address.to_string())
        .body(Empty::new())
        .map_err(|e| format!("Failed to build the request: {e}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| format!("Failed to read the answer: {e}"))?;
    Ok(response.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn an_unspecified_address_is_reached_through_loopback() {
        assert_eq!(
            reachable(address("0.0.0.0:9998")),
            Ok(address("127.0.0.1:9998"))
        );
        assert_eq!(reachable(address("[::]:9998")), Ok(address("[::1]:9998")));
    }

    #[test]
    fn a_specific_address_is_reached_as_it_is() {
        assert_eq!(
            reachable(address("10.0.0.5:8080")),
            Ok(address("10.0.0.5:8080"))
        );
    }

    #[test]
    fn port_zero_names_no_server_to_check() {
        assert!(reachable(address("127.0.0.1:0")).is_err());
    }
}
