//! HTTP `/healthz` endpoint for transponder liveness/readiness.
//!
//! Reads the shared `subscribed` flag flipped by `SubscribeMessageSource` on
//! every successful subscribe (true) and on every stream error (false).
//! Idle workspaces stay healthy because the flag is independent of message
//! traffic. h2 keepalive on the tonic Channel detects half-open streams and
//! surfaces them as `next()` errors, which flip the flag back to false.

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

pub(crate) async fn serve(subscribed: Arc<AtomicBool>, port: u16) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %addr, "healthz failed to bind, probe disabled");
            return;
        }
    };
    tracing::info!(addr = %addr, "healthz listening");
    loop {
        let (tcp, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "healthz accept failed");
                continue;
            }
        };
        let io = TokioIo::new(tcp);
        let flag = subscribed.clone();
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let flag = flag.clone();
                        async move { Ok::<_, Infallible>(handle(req, flag)) }
                    }),
                )
                .await;
        });
    }
}

fn handle(req: Request<Incoming>, subscribed: Arc<AtomicBool>) -> Response<Full<Bytes>> {
    let (status, body) = evaluate(req.uri().path(), subscribed.load(Ordering::Relaxed));
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(body)))
        .expect("static response build")
}

fn evaluate(path: &str, subscribed: bool) -> (StatusCode, String) {
    if path != "/healthz" {
        return (StatusCode::NOT_FOUND, String::new());
    }
    if !subscribed {
        return (StatusCode::SERVICE_UNAVAILABLE, "not subscribed".into());
    }
    (StatusCode::OK, "ok".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_returns_503_when_not_subscribed() {
        let (status, body) = evaluate("/healthz", false);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, "not subscribed");
    }

    #[test]
    fn evaluate_returns_200_when_subscribed() {
        let (status, body) = evaluate("/healthz", true);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[test]
    fn evaluate_returns_404_on_other_paths_when_subscribed() {
        let (status, _) = evaluate("/random", true);
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn evaluate_returns_404_on_other_paths_when_not_subscribed() {
        let (status, _) = evaluate("/random", false);
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
