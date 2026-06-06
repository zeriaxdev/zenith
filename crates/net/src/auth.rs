//! Microsoft → Xbox Live → XSTS → Minecraft authentication.
//!
//! Mojang accounts no longer exist; logging in to Minecraft means signing in
//! with a Microsoft account and exchanging that token through Xbox Live and
//! XSTS for a Minecraft session token. We use the OAuth 2.0 **device code**
//! flow: the user opens a URL, types a short code, and we poll in the
//! background until they finish — no embedded browser required.
//!
//! You must supply your own Azure application (client) id. See the notes in
//! the README / chat for how to register one. Set it via the `ZENITH_CLIENT_ID`
//! environment variable, or replace `CLIENT_ID` below.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};
use zenith_core::Session;

/// Replace this, or set the `ZENITH_CLIENT_ID` env var.
const CLIENT_ID: &str = "YOUR_AZURE_CLIENT_ID";
const SCOPE: &str = "XboxLive.signin offline_access";

pub fn client_id() -> String {
    std::env::var("ZENITH_CLIENT_ID").unwrap_or_else(|_| CLIENT_ID.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

// --- Step 1: request a device code --------------------------------------
pub fn request_device_code(client_id: &str) -> Result<DeviceCode> {
    if client_id == CLIENT_ID {
        bail!("No Azure client id set. Register an app and set ZENITH_CLIENT_ID.");
    }
    let resp = ureq::post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
        .send_form(&[("client_id", client_id), ("scope", SCOPE)]);
    match resp {
        Ok(r) => {
            let dc = r.into_json::<DeviceCode>()?;
            zenith_core::log::info(format!(
                "Device code {} — verify at {} (expires {}s)",
                dc.user_code, dc.verification_uri, dc.expires_in
            ));
            Ok(dc)
        }
        Err(ureq::Error::Status(_, r)) => {
            bail!("devicecode request failed: {}", r.into_string().unwrap_or_default())
        }
        Err(e) => Err(e.into()),
    }
}

// --- Step 2: poll until the user signs in -------------------------------
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
}

/// Blocks (with sleeps) until the user authorizes, then returns the Microsoft
/// access token. Intended to run on a background thread.
pub fn poll_for_token(client_id: &str, dc: &DeviceCode) -> Result<String> {
    zenith_core::log::info(format!("Polling for sign-in (every {}s)…", dc.interval));
    let deadline = Instant::now() + Duration::from_secs(dc.expires_in);
    let mut interval = Duration::from_secs(dc.interval.max(1));
    loop {
        if Instant::now() >= deadline {
            bail!("Sign-in timed out. Please try again.");
        }
        std::thread::sleep(interval);

        let resp = ureq::post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
            .send_form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &dc.device_code),
            ]);
        match resp {
            Ok(r) => {
                zenith_core::log::info("Microsoft token acquired.");
                return Ok(r.into_json::<TokenResponse>()?.access_token);
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                let err: TokenError =
                    serde_json::from_str(&body).unwrap_or(TokenError { error: "unknown".into() });
                match err.error.as_str() {
                    "authorization_pending" => {
                        continue;
                    }
                    "slow_down" => {
                        interval += Duration::from_secs(5);
                        continue;
                    }
                    "authorization_declined" => bail!("Sign-in was declined."),
                    "expired_token" => bail!("Sign-in code expired. Please try again."),
                    other => {
                        zenith_core::log::warn(format!("Token poll error {code}: {other}"));
                        bail!("Sign-in error: {other}");
                    }
                }
            }
            Err(e) => {
                zenith_core::log::error(format!("Token poll transport error: {e}"));
                return Err(e.into());
            }
        }
    }
}

// --- Step 3: Microsoft token → Minecraft session ------------------------
#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Deserialize)]
struct McToken {
    access_token: String,
}

#[derive(Deserialize)]
struct Profile {
    id: String,
    name: String,
}

fn post_json<T: serde::de::DeserializeOwned>(url: &str, body: serde_json::Value) -> Result<T> {
    match ureq::post(url).set("Accept", "application/json").send_json(body) {
        Ok(r) => Ok(r.into_json::<T>()?),
        Err(ureq::Error::Status(code, r)) => {
            bail!("{url} returned {code}: {}", r.into_string().unwrap_or_default())
        }
        Err(e) => Err(e.into()),
    }
}

pub fn minecraft_login(ms_access_token: &str) -> Result<Session> {
    zenith_core::log::info("Authenticating with Xbox Live…");
    // Xbox Live
    let xbl: XboxResponse = post_json(
        "https://user.auth.xboxlive.com/user/authenticate",
        serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={ms_access_token}"),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }),
    )?;
    let uhs = xbl
        .display_claims
        .xui
        .first()
        .ok_or_else(|| anyhow!("missing user hash"))?
        .uhs
        .clone();

    // XSTS
    let xsts: XboxResponse = post_json(
        "https://xsts.auth.xboxlive.com/xsts/authorize",
        serde_json::json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl.token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        }),
    )?;

    // Minecraft services
    let mc: McToken = post_json(
        "https://api.minecraftservices.com/authentication/login_with_xbox",
        serde_json::json!({
            "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token),
        }),
    )?;

    // Profile (also verifies the account owns the game)
    let profile: Profile = match ureq::get("https://api.minecraftservices.com/minecraft/profile")
        .set("Authorization", &format!("Bearer {}", mc.access_token))
        .call()
    {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(404, _)) => {
            bail!("This Microsoft account does not own Minecraft: Java Edition.")
        }
        Err(ureq::Error::Status(code, r)) => {
            bail!("profile request returned {code}: {}", r.into_string().unwrap_or_default())
        }
        Err(e) => return Err(e.into()),
    };

    Ok(Session {
        username: profile.name,
        uuid: profile.id,
        access_token: mc.access_token,
    })
}
