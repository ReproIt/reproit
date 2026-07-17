use super::*;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct CloudProject {
    pub name: String,
    #[serde(rename = "appId")]
    pub app_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct DeviceLogin {
    pub token: String,
    #[serde(rename = "orgId")]
    pub org_id: i64,
    #[serde(default)]
    pub projects: Vec<CloudProject>,
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = Command::new("cmd").args(["/C", "start", "", url]).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(url).status();
    result.map(|status| status.success()).unwrap_or(false)
}

/// GitHub CLI style device login: start a short-lived grant, open the browser,
/// then poll until the signed-in user approves it. The returned account token
/// is org-scoped and includes the projects visible in that organization.
pub async fn device_login(base: &str, show_progress: bool) -> Result<DeviceLogin> {
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let start = client
        .post(format!("{base}/auth/cli/device"))
        .json(&serde_json::json!({ "client": "reproit-cli" }))
        .send()
        .await
        .with_context(|| format!("starting browser login at {base}"))?;
    if !start.status().is_success() {
        anyhow::bail!("could not start browser login: {}", start.status());
    }
    let grant: Value = start.json().await.context("reading browser login grant")?;
    let device = grant["deviceCode"]
        .as_str()
        .context("cloud omitted deviceCode")?;
    let user_code = grant["userCode"]
        .as_str()
        .context("cloud omitted userCode")?;
    let url = grant["verificationUriComplete"]
        .as_str()
        .or_else(|| grant["verificationUri"].as_str())
        .context("cloud omitted verificationUri")?;
    let interval = grant["interval"].as_u64().unwrap_or(2).max(1);
    let expires = grant["expiresIn"].as_u64().unwrap_or(600);
    if show_progress {
        println!("Open this URL to authorize ReproIt:");
        println!("  {url}");
        println!("Code: {user_code}");
    }
    if !open_browser(url) && show_progress {
        println!("Your browser could not be opened automatically. Open the URL above.");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("browser authorization expired; run `reproit login` again");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let response = client
            .post(format!("{base}/auth/cli/token"))
            .json(&serde_json::json!({ "code": device }))
            .send()
            .await
            .context("waiting for browser authorization")?;
        if response.status().as_u16() == 202 {
            continue;
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("browser authorization failed ({status}): {}", body.trim());
        }
        return response
            .json()
            .await
            .context("reading authorized account token");
    }
}

pub(super) struct Cloud {
    base: String,
    key: Option<String>,
}

impl Cloud {
    pub(super) fn new(cloud: Option<String>, key: Option<String>) -> Self {
        // Defaults to the hosted cloud; set REPROIT_CLOUD_URL to point elsewhere.
        let base = cloud
            .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
            .unwrap_or_else(|| "https://cloud.reproit.com".to_string());
        // Cloud key precedence: explicit key (already resolved by `cloud_creds`)
        // > REPROIT_CLOUD_KEY (the project key, sk_live_...).
        let key = key.or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok());
        Cloud {
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    pub(super) async fn get(&self, path: &str) -> Result<Value> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let mut req = client.get(format!("{}{}", self.base, path));
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {}{}", self.base, path))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("cloud {} -> {}: {}", path, status, body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
    }

    /// POST a JSON body to an arbitrary cloud path, mirroring `get`:
    /// bearer-auth when a key is present, bail with a clear message on a
    /// non-2xx, and parse the (JSON) response body. Used by the triage SET
    /// path.
    pub(super) async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let mut req = client.post(format!("{}{}", self.base, path)).json(body);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("POST {}{}", self.base, path))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("cloud {} -> {}: {}", path, status, body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
    }

    /// PUT a JSON body, mirroring `post`. The integrations endpoint
    /// (`PUT /v1/apps/:app/integrations`, which binds the dispatch repo/token)
    /// is the one onboarding write that is a PUT rather than a POST.
    pub(super) async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let mut req = client.put(format!("{}{}", self.base, path)).json(body);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("PUT {}{}", self.base, path))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("cloud {} -> {}: {}", path, status, body.trim());
        }
        // Some PUTs return an empty body on success; tolerate that.
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
    }
}

/// Raw GET against the cloud errors namespace: `/v1/errors/:app{suffix}`. Used
/// by legacy cluster/cohort export paths (`cloud findings --export`, etc.) to
/// surface the unrendered JSON those views are built from. Fails
/// gracefully: a connection error or non-2xx surfaces as an anyhow error with a
/// clear message (Cloud::get already bails), never a panic.
pub async fn raw(
    app: &str,
    suffix: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<Value> {
    let c = Cloud::new(cloud, key);
    c.get(&format!("/v1/errors/{app}{suffix}")).await
}

/// Raw bucket list payload: `/v1/apps/:app/buckets`. This is the bucket-first
/// export surface behind `cloud query --export` and `cloud buckets --json`.
pub async fn raw_buckets(app: &str, cloud: Option<String>, key: Option<String>) -> Result<Value> {
    let c = Cloud::new(cloud, key);
    c.get(&format!("/v1/apps/{app}/buckets")).await
}

/// Tester-marked captures that have not yet passed the clean-launch replay
/// gate. They share the bucket identity and package endpoints, but Cloud keeps
/// them outside the confirmed bug feed until a `reproduced` verdict arrives.
pub async fn pending_captures(
    app: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<Vec<Value>> {
    let payload = raw_buckets(app, cloud, key).await?;
    Ok(payload["pendingCaptures"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// Resolve the project that owns a bucket using the signed-in account scope.
/// The bucket package is the authority and includes `appId`; callers use this
/// only to reach project-scoped management endpoints after global resolution.
pub async fn bucket_app(
    bucket: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<String> {
    let c = Cloud::new(cloud, key);
    if let Some(app) = crate::crosscut::load_cloud_app(&crate::crosscut::token_path()) {
        if c.get(&format!("/v1/apps/{app}/buckets/{bucket}"))
            .await
            .is_ok()
        {
            return Ok(app);
        }
    }
    let package = c.get(&format!("/v1/buckets/{bucket}")).await?;
    package["appId"]
        .as_str()
        .map(String::from)
        .context("cloud bucket package omitted appId")
}

/// Validate a cloud/project key for `cloud login` by hitting an AUTHENTICATED
/// endpoint, so login proves the key actually WORKS (not just that the host is
/// up). With an `app`, validates against `GET /v1/apps/:app/buckets` (the
/// loop's real entrypoint); without one, against `GET /v1/me`. A 401/403 is a
/// clear "bad key" error; any other non-2xx surfaces the status. On success
/// returns a short human description of what the key resolved to.
pub async fn validate_login(base: &str, key: &str, app: Option<&str>) -> Result<String> {
    let base = base.trim_end_matches('/');
    let path = match app {
        Some(a) => format!("/v1/apps/{a}/buckets"),
        None => "/v1/me".to_string(),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    let req = client.get(format!("{base}{path}")).bearer_auth(key);
    let resp = req
        .send()
        .await
        .with_context(|| format!("validating key against {base}{path}"))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        anyhow::bail!("the cloud rejected the key ({status}); check it is a valid sk_live_... key");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{base}{path} -> {status}: {}", body.trim());
    }
    // Describe what the key resolved to, without assuming a shape (the two
    // endpoints differ). For /v1/me: orgId + project count; for the buckets list:
    // the bucket count. Best-effort: a 2xx already proved the key is accepted.
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    match app {
        Some(a) => {
            let n = body["buckets"]
                .as_array()
                .map(|a| a.len())
                .or_else(|| body.as_array().map(|a| a.len()));
            match n {
                Some(n) => Ok(format!("key accepted for app {a} ({n} buckets)")),
                None => Ok(format!("key accepted for app {a}")),
            }
        }
        None => {
            // orgId is an integer (tenant.org_id) but tolerate a string too.
            let org = body["orgId"]
                .as_i64()
                .map(|n| n.to_string())
                .or_else(|| body["orgId"].as_str().map(String::from));
            let projects = body["projectCount"]
                .as_u64()
                .or_else(|| body["projects"].as_array().map(|items| items.len() as u64))
                .or_else(|| body["projects"].as_u64());
            match (org, projects) {
                (Some(o), Some(p)) => Ok(format!("key accepted (org {o}, {p} projects)")),
                (Some(o), None) => Ok(format!("key accepted (org {o})")),
                _ => Ok("key accepted".to_string()),
            }
        }
    }
}
