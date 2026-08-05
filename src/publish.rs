use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    config::AppConfig,
    credentials::{
        YOUTUBE_CLIENT_SECRET, YOUTUBE_REFRESH_TOKEN, delete_secret, read_secret, write_secret,
    },
    model::{Platform, PublishJob},
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublishError {
    #[error("select at least one destination")]
    NoDestination,
    #[error("{0} does not support {1} visibility")]
    UnsupportedVisibility(&'static str, &'static str),
}

#[derive(Default)]
pub struct PublishQueue {
    jobs: VecDeque<PublishJob>,
}

impl PublishQueue {
    pub fn enqueue(&mut self, job: PublishJob) -> Result<(), PublishError> {
        let enabled: Vec<_> = job.targets.iter().filter(|target| target.enabled).collect();
        if enabled.is_empty() {
            return Err(PublishError::NoDestination);
        }
        for target in enabled {
            if !target
                .platform
                .supported_visibilities()
                .contains(&target.visibility)
            {
                return Err(PublishError::UnsupportedVisibility(
                    target.platform.label(),
                    target.visibility.label(),
                ));
            }
        }
        self.jobs.push_back(job);
        Ok(())
    }

    pub fn jobs(&self) -> impl Iterator<Item = &PublishJob> {
        self.jobs.iter()
    }

    pub fn job_mut(&mut self, id: Uuid) -> Option<&mut PublishJob> {
        self.jobs.iter_mut().find(|job| job.id == id)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

pub fn connection_help(platform: Platform) -> &'static str {
    match platform {
        Platform::YouTube => {
            "Requires Google OAuth with the youtube.upload scope. Public uploads require an audited API project."
        }
        Platform::Odysee => {
            "Publishes through the local LBRY SDK. LBRY claims are public; private and unlisted visibility are not available."
        }
    }
}

#[derive(Debug)]
pub enum YouTubeAuthEvent {
    Connected,
    Failed(String),
}

#[derive(Debug)]
pub enum PublishEvent {
    Progress {
        job_id: Uuid,
        platform: Platform,
        progress: f32,
    },
    Complete {
        job_id: Uuid,
        links: Vec<String>,
    },
    Failed {
        job_id: Uuid,
        error: String,
    },
}

#[derive(Clone)]
pub struct PublishTask {
    pub job: PublishJob,
    pub config: AppConfig,
}

pub struct PublishWorker {
    sender: Sender<PublishTask>,
    receiver: Receiver<PublishEvent>,
}

impl Default for PublishWorker {
    fn default() -> Self {
        let (task_sender, task_receiver) = mpsc::channel::<PublishTask>();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(task) = task_receiver.recv() {
                run_publish_task(task, &event_sender);
            }
        });
        Self {
            sender: task_sender,
            receiver: event_receiver,
        }
    }
}

impl PublishWorker {
    pub fn enqueue(&self, task: PublishTask) -> Result<(), String> {
        self.sender
            .send(task)
            .map_err(|_| "the upload worker has stopped".to_owned())
    }

    pub fn try_iter(&self) -> impl Iterator<Item = PublishEvent> + '_ {
        self.receiver.try_iter()
    }
}

pub fn youtube_connected() -> bool {
    read_secret(YOUTUBE_REFRESH_TOKEN)
        .ok()
        .flatten()
        .is_some_and(|token| !token.is_empty())
}

pub fn disconnect_youtube() -> Result<(), String> {
    delete_secret(YOUTUBE_REFRESH_TOKEN).map_err(|error| error.to_string())?;
    delete_secret(YOUTUBE_CLIENT_SECRET).map_err(|error| error.to_string())
}

pub fn start_youtube_oauth(
    client_id: String,
    client_secret: String,
    sender: Sender<YouTubeAuthEvent>,
) -> Result<(), String> {
    if client_id.trim().is_empty() {
        return Err("Enter a Google OAuth desktop client ID first".into());
    }
    if !client_secret.is_empty() {
        write_secret(YOUTUBE_CLIENT_SECRET, &client_secret).map_err(|error| error.to_string())?;
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("Could not open the OAuth callback: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let state = Uuid::new_v4().simple().to_string();
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut auth_url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|error| error.to_string())?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", client_id.trim())
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "https://www.googleapis.com/auth/youtube.upload")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    open::that_detached(auth_url.as_str())
        .map_err(|error| format!("Could not open the Google sign-in page: {error}"))?;

    thread::spawn(move || {
        let result = finish_youtube_oauth(
            listener,
            &client_id,
            &client_secret,
            &redirect_uri,
            &state,
            &verifier,
        );
        let _ = sender.send(match result {
            Ok(()) => YouTubeAuthEvent::Connected,
            Err(error) => YouTubeAuthEvent::Failed(error),
        });
    });
    Ok(())
}

fn finish_youtube_oauth(
    listener: TcpListener,
    client_id: &str,
    supplied_client_secret: &str,
    redirect_uri: &str,
    expected_state: &str,
    verifier: &str,
) -> Result<(), String> {
    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("Google sign-in callback failed: {error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut request = [0_u8; 16 * 1024];
    let size = stream
        .read(&mut request)
        .map_err(|error| format!("Could not read the Google callback: {error}"))?;
    let request = String::from_utf8_lossy(&request[..size]);
    let callback_path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "Google returned an invalid callback".to_owned())?;
    let callback = Url::parse(&format!("http://127.0.0.1{callback_path}"))
        .map_err(|error| format!("Google returned an invalid callback URL: {error}"))?;
    let parameters = callback
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let returned_state = parameters
        .get("state")
        .ok_or_else(|| "Google callback did not contain the security state".to_owned())?;
    if returned_state.as_ref() != expected_state {
        return Err("Google callback security state did not match".into());
    }
    if let Some(error) = parameters.get("error") {
        return Err(format!("Google sign-in was not completed: {error}"));
    }
    let code = parameters
        .get("code")
        .ok_or_else(|| "Google callback did not contain an authorization code".to_owned())?;
    let client_secret = if supplied_client_secret.is_empty() {
        read_secret(YOUTUBE_CLIENT_SECRET)
            .map_err(|error| error.to_string())?
            .unwrap_or_default()
    } else {
        supplied_client_secret.to_owned()
    };
    let mut form = vec![
        ("client_id", client_id.to_owned()),
        ("code", code.to_string()),
        ("code_verifier", verifier.to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("grant_type", "authorization_code".to_owned()),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }
    let response = Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .map_err(|error| format!("Could not exchange the Google authorization code: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Could not read Google's token response: {error}"))?;
    if !status.is_success() {
        return Err(format!("Google token exchange failed ({status}): {body}"));
    }
    let token: GoogleTokenResponse =
        serde_json::from_str(&body).map_err(|error| format!("Invalid Google token: {error}"))?;
    let refresh_token = token
        .refresh_token
        .filter(|token| !token.is_empty())
        .or_else(|| read_secret(YOUTUBE_REFRESH_TOKEN).ok().flatten())
        .ok_or_else(|| {
            "Google did not return a refresh token; disconnect and try again".to_owned()
        })?;
    write_secret(YOUTUBE_REFRESH_TOKEN, &refresh_token).map_err(|error| error.to_string())?;
    let html = concat!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n",
        "<!doctype html><title>Xyra connected</title><body style='background:#0a0c11;color:#f1f3f8;font-family:sans-serif;padding:48px'>",
        "<h1>YouTube connected to Xyra</h1><p>You can close this tab and return to Xyra.</p></body>"
    );
    let _ = stream.write_all(html.as_bytes());
    Ok(())
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn run_publish_task(task: PublishTask, events: &Sender<PublishEvent>) {
    let mut links = Vec::new();
    for target in task.job.targets.iter().filter(|target| target.enabled) {
        let _ = events.send(PublishEvent::Progress {
            job_id: task.job.id,
            platform: target.platform,
            progress: 0.05,
        });
        let result = match target.platform {
            Platform::YouTube => upload_youtube(&task, target.visibility.youtube_value(), events),
            Platform::Odysee => upload_odysee(&task, events),
        };
        match result {
            Ok(link) => links.push(link),
            Err(error) => {
                let _ = events.send(PublishEvent::Failed {
                    job_id: task.job.id,
                    error,
                });
                return;
            }
        }
    }
    let _ = events.send(PublishEvent::Complete {
        job_id: task.job.id,
        links,
    });
}

fn google_access_token(config: &AppConfig) -> Result<String, String> {
    if config.youtube_client_id.trim().is_empty() {
        return Err("YouTube is not configured: add a Google OAuth desktop client ID".into());
    }
    let refresh_token = read_secret(YOUTUBE_REFRESH_TOKEN)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "YouTube is not connected".to_owned())?;
    let client_secret = read_secret(YOUTUBE_CLIENT_SECRET)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let mut form = vec![
        ("client_id", config.youtube_client_id.clone()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token".to_owned()),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }
    let response = Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .map_err(|error| format!("Could not refresh YouTube authorization: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Could not read Google's token response: {error}"))?;
    if !status.is_success() {
        return Err(format!("YouTube authorization failed ({status}): {body}"));
    }
    serde_json::from_str::<GoogleTokenResponse>(&body)
        .map(|token| token.access_token)
        .map_err(|error| format!("Google returned an invalid access token: {error}"))
}

fn upload_youtube(
    task: &PublishTask,
    visibility: &str,
    events: &Sender<PublishEvent>,
) -> Result<String, String> {
    let access_token = google_access_token(&task.config)?;
    let size = task
        .job
        .clip_path
        .metadata()
        .map_err(|error| format!("Could not read the clip before uploading: {error}"))?
        .len();
    let client = Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post("https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status")
        .bearer_auth(&access_token)
        .header("X-Upload-Content-Length", size)
        .header("X-Upload-Content-Type", "video/mp4")
        .json(&json!({
            "snippet": {
                "title": task.job.title,
                "description": task.job.description,
                "categoryId": "20"
            },
            "status": {
                "privacyStatus": visibility,
                "selfDeclaredMadeForKids": false
            }
        }))
        .send()
        .map_err(|error| format!("Could not start the YouTube upload: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("YouTube rejected the upload ({status}): {body}"));
    }
    let upload_url = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "YouTube did not return a resumable upload URL".to_owned())?
        .to_owned();
    let _ = events.send(PublishEvent::Progress {
        job_id: task.job.id,
        platform: Platform::YouTube,
        progress: 0.2,
    });
    let file = File::open(&task.job.clip_path)
        .map_err(|error| format!("Could not open the clip for YouTube: {error}"))?;
    let response = client
        .put(upload_url)
        .bearer_auth(access_token)
        .header(reqwest::header::CONTENT_LENGTH, size)
        .header(reqwest::header::CONTENT_TYPE, "video/mp4")
        .body(file)
        .send()
        .map_err(|error| format!("YouTube upload failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Could not read YouTube's upload response: {error}"))?;
    if !status.is_success() {
        return Err(format!("YouTube upload failed ({status}): {body}"));
    }
    let id = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("id")?.as_str().map(str::to_owned))
        .ok_or_else(|| "YouTube uploaded the file but did not return a video ID".to_owned())?;
    let _ = events.send(PublishEvent::Progress {
        job_id: task.job.id,
        platform: Platform::YouTube,
        progress: 1.0,
    });
    Ok(format!("https://youtu.be/{id}"))
}

fn upload_odysee(task: &PublishTask, events: &Sender<PublishEvent>) -> Result<String, String> {
    let name = odysee_claim_name(&task.job.title, task.job.id);
    let mut params = json!({
        "name": name,
        "bid": task.config.odysee_bid,
        "file_path": task.job.clip_path,
        "title": task.job.title,
        "description": task.job.description,
        "validate_file": true,
        "optimize_file": false,
        "blocking": true,
        "preview": false
    });
    if !task.config.odysee_channel_id.trim().is_empty() {
        params["channel_id"] = Value::String(task.config.odysee_channel_id.trim().to_owned());
    }
    let response = Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| error.to_string())?
        .post(&task.config.odysee_api_url)
        .json(&json!({"method": "publish", "params": params}))
        .send()
        .map_err(|error| {
            format!(
                "Could not reach the local LBRY SDK at {}: {error}",
                task.config.odysee_api_url
            )
        })?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Could not read the LBRY SDK response: {error}"))?;
    if !status.is_success() {
        return Err(format!("LBRY SDK publish failed ({status}): {body}"));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("LBRY SDK returned invalid JSON: {error}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!("LBRY SDK publish failed: {error}"));
    }
    let claim_id = value
        .pointer("/result/outputs/0/claim_id")
        .or_else(|| value.pointer("/result/claim_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let _ = events.send(PublishEvent::Progress {
        job_id: task.job.id,
        platform: Platform::Odysee,
        progress: 1.0,
    });
    if claim_id.is_empty() {
        Ok(format!("lbry://{name}"))
    } else {
        Ok(format!("https://odysee.com/{name}:{claim_id}"))
    }
}

fn odysee_claim_name(title: &str, id: Uuid) -> String {
    let mut name = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    name = name.trim_matches('-').chars().take(32).collect();
    if name.is_empty() {
        name = "xyra-clip".into();
    }
    format!("{name}-{}", &id.simple().to_string()[..8])
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::{PublishStatus, PublishTarget, Visibility};

    fn job(targets: Vec<PublishTarget>) -> PublishJob {
        PublishJob {
            id: Uuid::new_v4(),
            clip_id: Uuid::new_v4(),
            clip_path: "clip.mp4".into(),
            title: "test".into(),
            description: String::new(),
            targets,
            created_at: Utc::now(),
            status: PublishStatus::Queued,
        }
    }

    #[test]
    fn youtube_accepts_unlisted() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::YouTube);
        target.enabled = true;
        target.visibility = Visibility::Unlisted;
        assert!(queue.enqueue(job(vec![target])).is_ok());
    }

    #[test]
    fn odysee_rejects_false_privacy_promise() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::Odysee);
        target.enabled = true;
        target.visibility = Visibility::Private;
        assert_eq!(
            queue.enqueue(job(vec![target])),
            Err(PublishError::UnsupportedVisibility("Odysee", "Private"))
        );
    }

    #[test]
    fn odysee_rejects_unlisted() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::Odysee);
        target.enabled = true;
        target.visibility = Visibility::Unlisted;
        assert_eq!(
            queue.enqueue(job(vec![target])),
            Err(PublishError::UnsupportedVisibility("Odysee", "Unlisted"))
        );
    }
}
