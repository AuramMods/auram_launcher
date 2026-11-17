use crate::jobs::Job;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    pub uuid: String,
    pub xuid: String,
}
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    pub client_id: String,
    pub refresh_token: String,
    pub minecraft_token: String,
    pub expires_at: i64,
    pub profile: Option<Profile>,
}
pub trait Vault: Send + Sync {
    fn load(&self) -> Result<Option<Credentials>>;
    fn save(&self, value: &Credentials) -> Result<()>;
    fn clear(&self) -> Result<()>;
}
pub struct NativeVault;
impl NativeVault {
    fn entry() -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(
            if cfg!(feature = "qa") {
                "art.arcane.auram.qa"
            } else if cfg!(debug_assertions) {
                "art.arcane.auram.dev"
            } else {
                "art.arcane.auram.launcher"
            },
            "microsoft",
        )?)
    }
}
impl Vault for NativeVault {
    fn load(&self) -> Result<Option<Credentials>> {
        match Self::entry()?.get_password() {
            Ok(value) => Ok(Some(serde_json::from_str(&value)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, value: &Credentials) -> Result<()> {
        Self::entry()?.set_password(&serde_json::to_string(value)?)?;
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
#[derive(Clone)]
pub struct Auth {
    client: reqwest::Client,
    vault: Arc<dyn Vault>,
    gate: Arc<Mutex<()>>,
    token_url: String,
    service_base: String,
    xbl_url: String,
    xsts_url: String,
}
#[derive(Deserialize)]
struct OAuthToken {
    access_token: String,
    refresh_token: Option<String>,
}
impl Auth {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            vault: Arc::new(NativeVault),
            gate: Arc::new(Mutex::new(())),
            token_url: TOKEN.into(),
            service_base: "https://api.minecraftservices.com".into(),
            xbl_url: "https://user.auth.xboxlive.com/user/authenticate".into(),
            xsts_url: "https://xsts.auth.xboxlive.com/xsts/authorize".into(),
        }
    }
    pub fn profile(&self) -> Result<Option<Profile>> {
        Ok(self.vault.load()?.and_then(|v| v.profile))
    }
    pub async fn sign_out(&self) -> Result<()> {
        let _guard = self.gate.lock().await;
        self.vault.clear()
    }
    pub async fn session(&self, client_id: &str, job: &Job) -> Result<Credentials> {
        let _guard = self.gate.lock().await;
        let mut stored = self
            .vault
            .load()?
            .context("Sign in to your Microsoft account first")?;
        if stored.client_id != client_id {
            bail!("Sign in again after changing the Microsoft application ID");
        }
        if stored.expires_at > chrono::Utc::now().timestamp() + 90 && stored.profile.is_some() {
            return Ok(stored);
        }
        job.progress("auth", "Refreshing Microsoft session", 0, 0);
        let token = self
            .oauth(
                &[
                    ("client_id", client_id),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &stored.refresh_token),
                    ("scope", "XboxLive.signin offline_access"),
                ],
                job,
            )
            .await?;
        if let Some(refresh) = token.refresh_token {
            stored.refresh_token = refresh;
            self.vault.save(&stored)?;
        }
        self.exchange(stored, &token.access_token, job).await
    }
    pub async fn sign_in(
        &self,
        client_id: &str,
        job: &Job,
        open: impl FnOnce(&str) -> Result<()>,
    ) -> Result<Profile> {
        let _guard = self.gate.lock().await;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://localhost:{}/oauth/callback",
            listener.local_addr()?.port()
        );
        let verifier = random_secret();
        let state = random_secret();
        let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        let mut url =
            url::Url::parse("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize")?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("scope", "XboxLive.signin offline_access"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("prompt", "select_account"),
        ]);
        open(url.as_str())?;
        job.progress("auth", "Complete Microsoft sign-in in your browser", 0, 0);
        let code = tokio::select! {_=job.cancel.cancelled()=>bail!("Sign-in cancelled"),result=tokio::time::timeout(Duration::from_secs(300),receive_code(listener,&state))=>result.context("Microsoft sign-in timed out")??};
        let token = self
            .oauth(
                &[
                    ("client_id", client_id),
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &verifier),
                ],
                job,
            )
            .await?;
        let credentials = Credentials {
            client_id: client_id.into(),
            refresh_token: token
                .refresh_token
                .context("Microsoft did not issue an offline refresh token")?,
            ..Default::default()
        };
        self.vault.save(&credentials)?;
        self.exchange(credentials, &token.access_token, job)
            .await?
            .profile
            .context("Minecraft profile is missing")
    }
    async fn oauth(&self, form: &[(&str, &str)], job: &Job) -> Result<OAuthToken> {
        let response = tokio::select! {_=job.cancel.cancelled()=>bail!("Sign-in cancelled"),result=self.client.post(&self.token_url).form(form).send()=>result?};
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            let error = value["error"].as_str().unwrap_or("authentication_failed");
            if error == "invalid_grant" || error == "interaction_required" {
                bail!("Microsoft requires you to sign in again ({error})");
            }
            bail!(
                "Microsoft authentication failed: {error} (HTTP {})",
                status.as_u16()
            );
        }
        Ok(serde_json::from_value(value)?)
    }
    async fn post(&self, url: &str, body: Value, job: &Job) -> Result<Value> {
        job.check()?;
        let response = tokio::select! {_=job.cancel.cancelled()=>bail!("Sign-in cancelled"),result=self.client.post(url).json(&body).send()=>result?};
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            let detail = match value["XErr"].as_u64() {
                Some(2148916233) => "Create an Xbox profile for this Microsoft account",
                Some(2148916235) => "Xbox Live is unavailable for this account's region",
                Some(2148916236..=2148916238) => {
                    "Xbox family settings require account-owner action"
                }
                _ => "Account or application authorization was rejected",
            };
            bail!(
                "{detail} (HTTP {}, Xbox error {})",
                status.as_u16(),
                value["XErr"].as_u64().unwrap_or(0)
            );
        }
        Ok(value)
    }
    async fn exchange(
        &self,
        mut stored: Credentials,
        access: &str,
        job: &Job,
    ) -> Result<Credentials> {
        job.progress("auth", "Signing in to Xbox and Minecraft", 0, 0);
        let xbl=self.post(&self.xbl_url,json!({"Properties":{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":format!("d={access}")},"RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"}),job).await?;
        let xsts=self.post(&self.xsts_url,json!({"Properties":{"SandboxId":"RETAIL","UserTokens":[xbl["Token"].as_str().context("Xbox token missing")?]},"RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"}),job).await?;
        let claims = &xsts["DisplayClaims"]["xui"][0];
        let hash = claims["uhs"].as_str().context("Xbox user hash missing")?;
        let xuid = claims["xid"]
            .as_str()
            .or_else(|| xbl["DisplayClaims"]["xui"][0]["xid"].as_str())
            .unwrap_or("")
            .to_string();
        let login=self.post(&format!("{}/authentication/login_with_xbox",self.service_base),json!({"identityToken":format!("XBL3.0 x={hash};{}",xsts["Token"].as_str().context("XSTS token missing")?)}),job).await?;
        let minecraft_token = login["access_token"]
            .as_str()
            .context("Minecraft access token missing")?
            .to_string();
        let expiry = login["expires_in"]
            .as_i64()
            .context("Minecraft token expiry missing")?;
        let entitlement = self
            .client
            .get(format!("{}/entitlements/mcstore", self.service_base))
            .bearer_auth(&minecraft_token)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        if entitlement["items"]
            .as_array()
            .is_none_or(|items| items.is_empty())
        {
            bail!("This Microsoft account does not have a Minecraft Java entitlement");
        }
        let response = self
            .client
            .get(format!("{}/minecraft/profile", self.service_base))
            .bearer_auth(&minecraft_token)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("Set up a Minecraft Java profile before launching");
        }
        let value: Value = response.error_for_status()?.json().await?;
        stored.profile = Some(Profile {
            name: value["name"]
                .as_str()
                .context("Minecraft name missing")?
                .into(),
            uuid: value["id"]
                .as_str()
                .context("Minecraft UUID missing")?
                .into(),
            xuid,
        });
        stored.minecraft_token = minecraft_token;
        stored.expires_at = chrono::Utc::now().timestamp() + expiry;
        self.vault.save(&stored)?;
        Ok(stored)
    }
}
fn random_secret() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
fn callback_code(target: &str, state: &str) -> Result<Option<String>> {
    let url = url::Url::parse(&format!("http://localhost{target}"))?;
    if url.path() != "/oauth/callback" {
        return Ok(None);
    }
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if params.get("state").is_none_or(|value| value != state) {
        return Ok(None);
    }
    if let Some(error) = params.get("error") {
        bail!("Microsoft sign-in was not completed ({error})");
    }
    Ok(params.get("code").cloned())
}
async fn receive_code(listener: TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(3), async {
            while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
                let size = stream.read(&mut buffer).await?;
                if size == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..size]);
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            continue;
        }
        let request = String::from_utf8_lossy(&request);
        let mut words = request.split_whitespace();
        if words.next() != Some("GET") {
            continue;
        }
        let result = callback_code(words.next().unwrap_or("/"), state);
        let success = matches!(&result, Ok(Some(_)));
        let body = if success {
            "Sign-in received. You can return to Auram."
        } else {
            "Sign-in callback rejected. Return to Auram to retry."
        };
        let status = if success { "200 OK" } else { "400 Bad Request" };
        let _=stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
        match result {
            Ok(Some(code)) => return Ok(code),
            Err(error) => return Err(error),
            _ => {}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_require_expected_state_and_path() {
        assert!(
            callback_code("/oauth/callback?state=wrong&code=secret", "expected")
                .unwrap()
                .is_none()
        );
        assert!(
            callback_code("/elsewhere?state=expected&code=secret", "expected")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            callback_code("/oauth/callback?state=expected&code=secret", "expected").unwrap(),
            Some("secret".into())
        );
        assert!(callback_code(
            "/oauth/callback?state=expected&error=access_denied",
            "expected"
        )
        .is_err());
    }
    #[test]
    fn secrets_have_pkce_entropy() {
        let a = random_secret();
        let b = random_secret();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
    }
    #[derive(Default)]
    struct MemoryVault(std::sync::Mutex<Option<Credentials>>);
    impl Vault for MemoryVault {
        fn load(&self) -> Result<Option<Credentials>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, value: &Credentials) -> Result<()> {
            *self.0.lock().unwrap() = Some(value.clone());
            Ok(())
        }
        fn clear(&self) -> Result<()> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }
    #[tokio::test]
    async fn refresh_is_serialized_rotated_and_retained_on_transient_failure() {
        use axum::{
            routing::{get, post},
            Json, Router,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        let refreshes = Arc::new(AtomicUsize::new(0));
        let counter = refreshes.clone();
        let router=Router::new()
            .route("/token",post(move|| {let counter=counter.clone();async move {counter.fetch_add(1,Ordering::SeqCst);Json(json!({"access_token":"msa","refresh_token":"rotated"}))}}))
            .route("/xbl",post(||async {Json(json!({"Token":"xbl"}))}))
            .route("/xsts",post(||async {Json(json!({"Token":"xsts","DisplayClaims":{"xui":[{"uhs":"hash","xid":"xbox-id"}]}}))}))
            .route("/authentication/login_with_xbox",post(||async {Json(json!({"access_token":"minecraft","expires_in":7200}))}))
            .route("/entitlements/mcstore",get(||async {Json(json!({"items":[{"name":"game_minecraft"}]}))}))
            .route("/minecraft/profile",get(||async {Json(json!({"name":"Player","id":"uuid"}))}));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let vault = Arc::new(MemoryVault::default());
        vault
            .save(&Credentials {
                client_id: "client".into(),
                refresh_token: "original".into(),
                ..Default::default()
            })
            .unwrap();
        let mut auth = Auth::new(reqwest::Client::new());
        auth.vault = vault.clone();
        auth.token_url = format!("{base}/token");
        auth.xbl_url = format!("{base}/xbl");
        auth.xsts_url = format!("{base}/xsts");
        auth.service_base = base;
        let job = Job::default();
        let (one, two) = tokio::join!(auth.session("client", &job), auth.session("client", &job));
        let profile = one.unwrap().profile.unwrap();
        assert_eq!(profile.xuid, "xbox-id");
        assert_eq!(two.unwrap().minecraft_token, "minecraft");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(vault.load().unwrap().unwrap().refresh_token, "rotated");
        let mut expired = vault.load().unwrap().unwrap();
        expired.expires_at = 0;
        vault.save(&expired).unwrap();
        auth.xbl_url = "http://127.0.0.1:1/unavailable".into();
        assert!(auth.session("client", &job).await.is_err());
        assert_eq!(vault.load().unwrap().unwrap().refresh_token, "rotated");
        auth.sign_out().await.unwrap();
        assert!(vault.load().unwrap().is_none());
        server.abort();
    }
}
