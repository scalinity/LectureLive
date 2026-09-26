//! Stands between LectureLive and api.x.ai, so a check can take the network away from LectureLive alone
//! (M6 plan, the error table): `kill -STOP <pid>` holds every connection open with nothing passing, as a
//! dropped link does; `kill -CONT <pid>` lets traffic pass again; killing it refuses new connections. Zoom and
//! every other program keep the real network. LectureLive connects through it with
//! LECTURELIVE_API_ADDR=127.0.0.1:8443, and still checks api.x.ai's certificate.
//!   cargo run -p lecturelive-core --example netcut -- 127.0.0.1:8443 api.x.ai:443
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8443".into());
    let upstream = args.next().unwrap_or_else(|| "api.x.ai:443".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("netcut {} -> {upstream}, pid {}", listener.local_addr()?, std::process::id());
    loop {
        let (mut client, _) = listener.accept().await?;
        let up = upstream.clone();
        tokio::spawn(async move {
            if let Ok(mut server) = tokio::net::TcpStream::connect(&up).await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            }
        });
    }
}
