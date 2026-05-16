// Shared integration-test scaffolding. Spawns a real daemon (with a real
// Chromium) on a random port and exposes a small async client.

use std::collections::HashMap;
use std::sync::Arc;

use acb_policy::CompiledPolicy;
use acb_daemon::{auth, start, RunningDaemon, StartConfig};

pub struct TestDaemon {
    pub running: RunningDaemon,
    pub token: String,
    pub base: String,
    // Keep the tempdir alive for the lifetime of the daemon.
    pub _profile: tempfile::TempDir,
}

impl TestDaemon {
    pub async fn spawn(policy: Arc<CompiledPolicy>) -> Self {
        let token = auth::generate_token();
        let profile = tempfile::tempdir().expect("tempdir");
        let cfg = StartConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            policy,
            token: token.clone(),
            token_path: None,
            chrome_binary: None,
            headless: true,
            user_data_dir: Some(profile.path().to_path_buf()),
        };
        let running = start(cfg).await.expect("daemon start");
        let base = format!("http://{}", running.addr);
        Self {
            running,
            token,
            base,
            _profile: profile,
        }
    }

    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap()
    }

    pub async fn shutdown(self) {
        let _ = self.running.shutdown.send(());
        let _ = self.running.join.await;
    }
}

/// Spin up a tiny in-memory file server serving the given map under
/// `http://127.0.0.1:<port>/<path>`. Returns (base_url, shutdown_tx).
/// `Content-Type` is sniffed crudely from the extension.
pub async fn spawn_static(files: HashMap<String, Vec<u8>>) -> (String, tokio::sync::oneshot::Sender<()>) {
    use axum::body::Body;
    use axum::http::{header, HeaderValue, StatusCode};
    use axum::response::Response;
    use axum::routing::any;
    use axum::Router;

    let files = Arc::new(files);
    let app = Router::new().route(
        "/*path",
        any({
            let files = files.clone();
            move |axum::extract::Path(path): axum::extract::Path<String>| {
                let files = files.clone();
                async move {
                    let key = format!("/{path}");
                    match files.get(&key) {
                        Some(bytes) => {
                            let ct = if key.ends_with(".html") {
                                "text/html; charset=utf-8"
                            } else if key.ends_with(".css") {
                                "text/css; charset=utf-8"
                            } else if key.ends_with(".js") {
                                "application/javascript; charset=utf-8"
                            } else {
                                "application/octet-stream"
                            };
                            Response::builder()
                                .header(header::CONTENT_TYPE, HeaderValue::from_static(ct))
                                .body(Body::from(bytes.clone()))
                                .unwrap()
                        }
                        None => Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Body::from("not found"))
                            .unwrap(),
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    (format!("http://{addr}"), tx)
}

pub fn fixture_policy(rules_yaml: &str) -> Arc<CompiledPolicy> {
    let yaml = format!(
        r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/test.log"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "./var/test-profile"
  viewport: {{ width: 1024, height: 768 }}
  screencast: {{ format: "jpeg", quality: 60, max_fps: 8 }}
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file", "chrome", "about", "blob", "ws", "wss"]
  bypass_service_worker: true
rules:
{rules_yaml}
"#
    );
    Arc::new(acb_policy::load::load_policy(&yaml).expect("test policy"))
}
