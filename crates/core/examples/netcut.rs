//! Stands between LectureLive and api.x.ai, so a check can take the network away from LectureLive alone
//! (M6 plan, the error table). Zoom and every other program keep the real network: only a program started with
//! LECTURELIVE_API_ADDR=127.0.0.1:8443 connects through it, and it still checks api.x.ai's certificate.
//!   cargo run -p lecturelive-core --example netcut -- 127.0.0.1:8443 api.x.ai:443 /path/to/control
//! It is steered by the control file, never by signals sent to a process: its first word is the link's state,
//! read every 100 ms, and anything else, or no file, is `up`.
//!   up       traffic passes
//!   hold     every connection stays open with nothing passing, as a dropped Wi-Fi link does
//!   refuse   open connections are closed and new ones are refused at once, as a refused connection is
//! Without a control file it passes everything.
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Link {
    Up,
    Hold,
    Refuse,
}

fn link_in(path: &str) -> Link {
    match std::fs::read_to_string(path).unwrap_or_default().split_whitespace().next() {
        Some("hold") => Link::Hold,
        Some("refuse") => Link::Refuse,
        _ => Link::Up,
    }
}

/// One direction of a connection. What it has read waits while the link is held, and the connection ends when it is refused.
async fn pump(mut from: impl AsyncRead + Unpin, mut to: impl AsyncWrite + Unpin, mut link: watch::Receiver<Link>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = tokio::select! {
            r = from.read(&mut buf) => match r {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            },
            _ = link.wait_for(|l| *l == Link::Refuse) => break,
        };
        let passes = link.wait_for(|l| *l != Link::Hold).await.map(|l| *l == Link::Up).unwrap_or(false);
        if !passes || to.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8443".into());
    let upstream = args.next().unwrap_or_else(|| "api.x.ai:443".into());
    let control = args.next();
    let (set, link) = watch::channel(Link::Up);
    if let Some(path) = control.clone() {
        tokio::spawn(async move {
            loop {
                let now = link_in(&path);
                set.send_if_modified(|l| std::mem::replace(l, now) != now);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("netcut {} -> {upstream}, control {}", listener.local_addr()?, control.as_deref().unwrap_or("none (always up)"));
    loop {
        let (client, _) = listener.accept().await?;
        let (up, link) = (upstream.clone(), link.clone());
        tokio::spawn(async move {
            if *link.borrow() == Link::Refuse {
                return; // dropped at once: the connection is refused
            }
            // A held link holds a new connection too, before anything reaches the server.
            let mut waiting = link.clone();
            if waiting.wait_for(|l| *l != Link::Hold).await.map(|l| *l == Link::Up).unwrap_or(false) {
                if let Ok(server) = tokio::net::TcpStream::connect(&up).await {
                    let (cr, cw) = client.into_split();
                    let (sr, sw) = server.into_split();
                    tokio::join!(pump(cr, sw, link.clone()), pump(sr, cw, link));
                }
            }
        });
    }
}
