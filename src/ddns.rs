use std::net::Ipv6Addr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::crypto::ring;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

const CF_API_BASE: &str = "https://api.cloudflare.com/client/v4";
const ECHO_URLS: [&str; 2] = ["https://api6.ipify.org/", "https://ipv6.icanhazip.com/"];
type DdnsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub ddns: Vec<DdnsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DdnsConfig {
    zone: String,
    zone_id: String,
    records: Vec<String>,
    api_token: String,
    #[serde(default = "default_interval")]
    interval_secs: u64,
}

fn default_interval() -> u64 {
    60
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        let config: Self = toml::from_str(&text).context("解析 TOML 配置失败")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        for entry in &self.ddns {
            ensure!(!entry.zone.trim().is_empty(), "[[ddns]] zone 不能为空");
            ensure!(
                !entry.zone_id.trim().is_empty(),
                "[[ddns]] zone_id 不能为空"
            );
            ensure!(
                !entry.api_token.trim().is_empty(),
                "[[ddns]] api_token 不能为空 (zone={})",
                entry.zone
            );
            ensure!(
                !entry.records.is_empty(),
                "[[ddns]] records 不能为空 (zone={})",
                entry.zone
            );
            ensure!(
                entry.records.iter().all(|name| !name.trim().is_empty()),
                "[[ddns]] records 中存在空名称 (zone={})",
                entry.zone
            );
        }
        Ok(())
    }
}

pub async fn run(cfg: DdnsConfig) {
    let interval = Duration::from_secs(cfg.interval_secs.max(10));
    let client = match build_client() {
        Ok(client) => client,
        Err(error) => {
            warn!(zone = %cfg.zone, %error, "初始化 DDNS HTTP 客户端失败");
            return;
        }
    };
    info!(zone = %cfg.zone, records = ?cfg.records, interval_secs = interval.as_secs(), "DDNS 任务已启动");

    let mut last_ip = None;
    loop {
        match fetch_public_ipv6(&client, &ECHO_URLS).await {
            Ok(ip) if last_ip != Some(ip) => match sync_all(&client, CF_API_BASE, &cfg, ip).await {
                Ok(()) => {
                    info!(zone = %cfg.zone, %ip, "DDNS 同步完成");
                    last_ip = Some(ip);
                }
                Err(error) => {
                    warn!(zone = %cfg.zone, error = %format!("{error:#}"), "DDNS 同步失败，将重试")
                }
            },
            Ok(_) => debug!(zone = %cfg.zone, "公网 IPv6 未变化"),
            Err(error) => {
                warn!(zone = %cfg.zone, error = %format!("{error:#}"), "获取公网 IPv6 失败")
            }
        }
        tokio::time::sleep(interval).await;
    }
}

fn build_client() -> anyhow::Result<DdnsClient> {
    let https = HttpsConnectorBuilder::new()
        .with_provider_and_webpki_roots(Arc::new(ring::default_provider()))
        .context("初始化 TLS 配置失败")?
        .https_or_http()
        .enable_http1()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

async fn fetch_public_ipv6(client: &DdnsClient, urls: &[&str]) -> anyhow::Result<Ipv6Addr> {
    let mut last_error = String::new();
    for url in urls {
        let response = match client.request(get(url)).await {
            Ok(response) => response,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .context("读取 IPv6 回显响应失败")?
            .to_bytes();
        if !status.is_success() {
            last_error = format!("HTTP {status}");
            continue;
        }
        match std::str::from_utf8(&bytes)
            .context("IPv6 回显响应不是 UTF-8")?
            .trim()
            .parse::<Ipv6Addr>()
        {
            Ok(ip) => return Ok(ip),
            Err(_) => last_error = format!("HTTP {status}: 回显响应不是 IPv6 地址"),
        }
    }
    bail!("所有 IPv6 回显服务都失败: {last_error}")
}

async fn sync_all(
    client: &DdnsClient,
    api_base: &str,
    cfg: &DdnsConfig,
    ip: Ipv6Addr,
) -> anyhow::Result<()> {
    for name in &cfg.records {
        let url = format!(
            "{api_base}/zones/{}/dns_records?type=AAAA&name={}",
            cfg.zone_id,
            encode_query_value(name)
        );
        let result = cf_request(client, Method::GET, &url, cfg.api_token.trim(), None).await?;
        let record = result
            .as_array()
            .and_then(|records| records.first())
            .with_context(|| format!("Cloudflare 上没有 {name} 的 AAAA 记录，请先在控制台创建"))?;
        let record_id = record["id"]
            .as_str()
            .with_context(|| format!("{name} 的 Cloudflare 记录响应缺少 id"))?;
        if record["content"].as_str() == Some(ip.to_string().as_str()) {
            debug!(%name, "AAAA 记录已是最新");
            continue;
        }

        let body = json!({
            "type": "AAAA",
            "name": name,
            "content": ip.to_string(),
            "ttl": record.get("ttl").cloned().unwrap_or(json!(1)),
            "proxied": record.get("proxied").cloned().unwrap_or(json!(false)),
        });
        let url = format!("{api_base}/zones/{}/dns_records/{record_id}", cfg.zone_id);
        cf_request(
            client,
            Method::PUT,
            &url,
            cfg.api_token.trim(),
            Some(body.to_string()),
        )
        .await?;
        info!(zone = %cfg.zone, %name, %ip, "Cloudflare AAAA 记录已更新");
    }
    Ok(())
}

async fn cf_request(
    client: &DdnsClient,
    method: Method,
    url: &str,
    token: &str,
    body: Option<String>,
) -> anyhow::Result<Value> {
    let builder = Request::builder()
        .method(method)
        .uri(url)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let request = match body {
        Some(body) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .context("构造 Cloudflare 请求失败")?,
        None => builder
            .body(Full::default())
            .context("构造 Cloudflare 请求失败")?,
    };
    let response = client
        .request(request)
        .await
        .context("请求 Cloudflare API 失败")?;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("读取 Cloudflare 响应失败")?
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Cloudflare 响应失败（HTTP {status}）"))?;
    if !status.is_success() || value["success"] != json!(true) {
        bail!("Cloudflare API 拒绝（HTTP {status}）: {}", value["errors"]);
    }
    Ok(value["result"].clone())
}

fn get(url: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .uri(url)
        .body(Full::default())
        .expect("有效的回显 URL")
}

fn encode_query_value(value: &str) -> String {
    value.replace('*', "%2A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto;

    #[derive(Default)]
    struct MockState {
        content: std::collections::HashMap<String, String>,
        puts: Vec<Value>,
        queries: Vec<String>,
    }

    async fn mock_cloudflare() -> (String, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState {
            content: [
                ("example.com".to_owned(), "2001:db8::1".to_owned()),
                ("*.example.com".to_owned(), "2001:db8::1".to_owned()),
            ]
            .into(),
            ..MockState::default()
        }));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server_state = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let state = server_state.clone();
                let service = service_fn(move |request: Request<Incoming>| {
                    let state = state.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        let response = match (parts.method, parts.uri.path().to_owned()) {
                            (Method::GET, path) if path.ends_with("/dns_records") => {
                                let mut state = state.lock().unwrap();
                                let query = parts.uri.query().unwrap_or_default().to_owned();
                                state.queries.push(query.clone());
                                let name = query
                                    .split("name=")
                                    .nth(1)
                                    .unwrap_or_default()
                                    .replace("%2A", "*");
                                let content = state.content.get(&name).cloned().unwrap_or_default();
                                drop(state);
                                json_response(json!({"success": true, "result": [{
                                    "id": "record-id", "content": content,
                                    "ttl": 300, "proxied": false
                                }]}))
                            }
                            (Method::PUT, path) if path.ends_with("/record-id") => {
                                let value: Value = serde_json::from_slice(&body).unwrap();
                                let mut state = state.lock().unwrap();
                                state.content.insert(
                                    value["name"].as_str().unwrap().to_owned(),
                                    value["content"].as_str().unwrap().to_owned(),
                                );
                                state.puts.push(value.clone());
                                json_response(json!({"success": true, "result": value}))
                            }
                            _ => hyper::Response::builder()
                                .status(404)
                                .body(Full::default())
                                .unwrap(),
                        };
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                tokio::spawn(async move {
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        (format!("http://{addr}"), state)
    }

    fn json_response(value: Value) -> hyper::Response<Full<Bytes>> {
        hyper::Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(value.to_string())))
            .unwrap()
    }

    fn test_config() -> DdnsConfig {
        DdnsConfig {
            zone: "example.com".into(),
            zone_id: "zone-id".into(),
            records: vec!["example.com".into(), "*.example.com".into()],
            api_token: "test-token".into(),
            interval_secs: 60,
        }
    }

    #[test]
    fn example_config_contains_both_zones_and_requires_real_tokens() {
        let mut config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert!(config.validate().is_err(), "空 Token 应在启动时被拒绝");
        assert_eq!(config.ddns.len(), 2);
        assert_eq!(config.ddns[0].zone, "example.com");
        assert_eq!(config.ddns[1].zone, "example2.com");
        assert_eq!(config.ddns[0].records, ["example.com", "*.example.com"]);
        assert_eq!(config.ddns[1].records, ["example2.com", "*.example2.com"]);
        for entry in &mut config.ddns {
            entry.zone_id = "test-zone".to_owned();
            entry.api_token = "test-token".to_owned();
        }
        config.validate().unwrap();
    }

    #[tokio::test]
    async fn updates_all_records_preserves_settings_and_skips_current_ip() {
        let _ = ring::default_provider().install_default();
        let client = build_client().unwrap();
        let (api_base, state) = mock_cloudflare().await;
        let cfg = test_config();
        let ip = "2001:db8::25".parse().unwrap();

        sync_all(&client, &api_base, &cfg, ip).await.unwrap();
        {
            let state = state.lock().unwrap();
            assert_eq!(state.puts.len(), 2);
            assert!(
                state
                    .puts
                    .iter()
                    .all(|put| put["content"] == "2001:db8::25")
            );
            assert!(
                state
                    .puts
                    .iter()
                    .all(|put| put["ttl"] == 300 && put["proxied"] == false)
            );
            assert_eq!(state.queries[1], "type=AAAA&name=%2A.example.com");
        }
        sync_all(&client, &api_base, &cfg, ip).await.unwrap();
        assert_eq!(state.lock().unwrap().puts.len(), 2);
    }
}
