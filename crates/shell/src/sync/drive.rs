use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use url::Url;

use super::journal::{Batch, Upload};

const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const DRIVE: &str = "https://www.googleapis.com/drive/v3/";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const MARKER: &str = "bibleown-sync-v1";
// OAuth Desktop clients are public clients: distributed applications cannot
// keep a client secret confidential. Embed only the public Client ID and use
// PKCE for every authorization-code exchange. The build-time environment
// variable remains an override for forks and local development.
const BUNDLED_CLIENT_ID: &str =
    "108525654331-tlkmug1o0m2nefre3r2bkmn8hoopceic.apps.googleusercontent.com";

fn random_secret() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("Не получены случайные данные: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Client {
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

impl Client {
    pub fn id(&self) -> &str {
        &self.client_id
    }

    pub fn from_google_file(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct DownloadedClient {
            installed: Client,
        }
        let client: DownloadedClient =
            super::files::read_json(path)?.context("Файл OAuth не найден")?;
        client.installed.validate()
    }

    pub fn bundled() -> Result<Option<Self>> {
        let id = option_env!("BIBLEOWN_GOOGLE_CLIENT_ID").unwrap_or(BUNDLED_CLIENT_ID);
        Ok(Some(
            Self {
                client_id: id.into(),
                client_secret: option_env!("BIBLEOWN_GOOGLE_CLIENT_SECRET")
                    .unwrap_or("")
                    .into(),
            }
            .validate()?,
        ))
    }

    fn validate(self) -> Result<Self> {
        ensure!(
            self.client_id.ends_with(".apps.googleusercontent.com")
                && !self.client_id.contains(char::is_whitespace),
            "Нужен OAuth-клиент Google типа Desktop app"
        );
        Ok(self)
    }

    pub fn start_login(&self) -> Result<Login> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let redirect = format!(
            "http://127.0.0.1:{}/oauth2/callback",
            listener.local_addr()?.port()
        );
        let verifier = random_secret()?;
        let state = random_secret()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.id()),
            ("redirect_uri", &redirect),
            ("scope", SCOPE),
            ("response_type", "code"),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent select_account"),
        ]);
        Ok(Login {
            client: self.clone(),
            listener,
            redirect,
            verifier,
            state,
            url,
        })
    }

    fn token(&self, fields: &[(&str, &str)]) -> Result<Token> {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("client_id", self.id());
        if !self.client_secret.is_empty() {
            form.append_pair("client_secret", &self.client_secret);
        }
        form.extend_pairs(fields.iter().copied());
        let body = form.finish();
        let response = request(
            "POST",
            &Url::parse(TOKEN)?,
            None,
            Some(("application/x-www-form-urlencoded", body.as_bytes())),
        )?;
        if response.status == 400
            && serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|body| body["error"].as_str().map(str::to_owned))
                .as_deref()
                == Some("invalid_grant")
        {
            bail!("Срок доступа истёк. Подключите Google Drive заново");
        }
        response.json()
    }

    pub fn refresh(&self, refresh_token: &str) -> Result<Drive> {
        let token = self.token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])?;
        Drive::new(&token)
    }
}

pub struct Login {
    client: Client,
    listener: TcpListener,
    redirect: String,
    verifier: String,
    state: String,
    url: Url,
}

impl Login {
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    pub fn client_id(&self) -> &str {
        self.client.id()
    }

    pub fn wait(self) -> Result<(Drive, String)> {
        let code = self.wait_for_code()?;
        let token = self.client.token(&[
            ("code", &code),
            ("code_verifier", &self.verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.redirect),
        ])?;
        let drive = Drive::new(&token)?;
        Ok((
            drive,
            token
                .refresh_token
                .context("Google не выдал токен для фоновой синхронизации")?,
        ))
    }

    fn wait_for_code(&self) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            let (mut stream, _) = match self.listener.accept() {
                Ok(result) => result,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            let mut line = String::new();
            let mut reader = BufReader::new((&stream).take(8192));
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let path = if fields.next() == Some("GET") {
                fields.next()
            } else {
                None
            };
            let mut code = None;
            let mut state = None;
            if let Some(path) = path.filter(|path| path.starts_with("/oauth2/callback?"))
                && let Ok(url) = Url::parse(&format!("http://127.0.0.1{path}"))
            {
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "code" => code = Some(value.into_owned()),
                        "state" => state = Some(value.into_owned()),
                        _ => {}
                    }
                }
            }
            let valid = state.as_deref() == Some(&self.state);
            let (status, message) = if valid && code.is_some() {
                ("200 OK", "Ответ Google получен. Вернитесь в BibleOWN.")
            } else {
                ("400 Bad Request", "Запрос авторизации отклонён.")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}",
                message.len()
            );
            if valid {
                return code.context("Вход в Google отменён");
            }
        }
        bail!("Время подключения Google Drive истекло")
    }
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

#[derive(Clone)]
pub struct Drive {
    token: String,
    expires: Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    #[serde(default)]
    app_properties: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeList {
    changes: Vec<Change>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}

#[derive(Deserialize)]
struct Change {
    file: Option<DriveFile>,
    #[serde(default)]
    removed: bool,
}

impl Drive {
    fn new(token: &Token) -> Result<Self> {
        ensure!(
            !token.access_token.is_empty()
                && token
                    .access_token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte)),
            "Google вернул неверный токен"
        );
        Ok(Self {
            token: token.access_token.clone(),
            expires: Instant::now()
                + Duration::from_secs(token.expires_in.saturating_sub(60).min(3600)),
        })
    }

    pub fn is_fresh(&self) -> bool {
        Instant::now() < self.expires
    }

    pub fn account_id(&self) -> Result<String> {
        let value: Value = self.get("about", &[("fields", "user(permissionId)")])?;
        Ok(value["user"]["permissionId"]
            .as_str()
            .context("Не удалось определить Google Drive")?
            .to_owned())
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let mut url = Url::parse(DRIVE)?.join(path)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        request("GET", &url, Some(&self.token), None)?.json()
    }

    fn file(&self, id: &str) -> Result<Batch> {
        ensure!(
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Неверный ID файла Google Drive"
        );
        self.get(&format!("files/{id}"), &[("alt", "media")])
    }

    pub fn download(&self, cursor: Option<&str>) -> Result<(String, Vec<Batch>)> {
        let mut files = std::collections::BTreeSet::new();
        let cursor = if let Some(cursor) = cursor {
            cursor.to_owned()
        } else {
            let result: Value = self.get("changes/startPageToken", &[])?;
            let cursor = result["startPageToken"]
                .as_str()
                .context("Google Drive не вернул позицию журнала")?
                .to_owned();
            let mut page = String::new();
            loop {
                let mut query = vec![
                    ("spaces", "appDataFolder"),
                    ("fields", "nextPageToken,files(id,appProperties)"),
                    ("pageSize", "1000"),
                    (
                        "q",
                        "trashed = false and appProperties has { key='format' and value='bibleown-sync-v1' }",
                    ),
                ];
                if !page.is_empty() {
                    query.push(("pageToken", &page));
                }
                let result: FileList = self.get("files", &query)?;
                for file in result.files {
                    files.insert(file.id);
                }
                match result.next_page_token {
                    Some(next) => page = next,
                    None => break,
                }
            }
            cursor
        };

        let mut page = cursor;
        loop {
            let result: ChangeList = self.get(
                "changes",
                &[
                    ("spaces", "appDataFolder"),
                    (
                        "fields",
                        "nextPageToken,newStartPageToken,changes(removed,file(id,appProperties))",
                    ),
                    ("pageToken", &page),
                    ("pageSize", "1000"),
                ],
            )?;
            for change in result.changes {
                if !change.removed
                    && let Some(file) = change.file
                    && file
                        .app_properties
                        .get("format")
                        .is_some_and(|tag| tag == MARKER)
                {
                    files.insert(file.id);
                }
            }
            match result.next_page_token {
                Some(next) => page = next,
                None => {
                    let cursor = result
                        .new_start_page_token
                        .context("Google Drive не вернул позицию журнала")?;
                    let batches = files
                        .iter()
                        .map(|id| self.file(id))
                        .collect::<Result<Vec<_>>>()?;
                    return Ok((cursor, batches));
                }
            }
        }
    }

    pub fn new_id(&self) -> Result<String> {
        let result: Value = self.get(
            "files/generateIds",
            &[("space", "appDataFolder"), ("count", "1")],
        )?;
        Ok(result["ids"][0]
            .as_str()
            .context("Google Drive не выдал ID файла")?
            .to_owned())
    }

    pub fn upload(&self, upload: &Upload) -> Result<()> {
        let boundary = random_secret()?;
        let meta = json!({
            "id": upload.drive_id,
            "name": format!("{MARKER}-{}.json", upload.drive_id),
            "parents": ["appDataFolder"],
            "mimeType": "application/json",
            "appProperties": { "format": MARKER }
        });
        let mut body = Vec::new();
        write!(
            body,
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/json\r\n\r\n"
        )?;
        serde_json::to_writer(&mut body, &upload.batch)?;
        write!(body, "\r\n--{boundary}--\r\n")?;
        let mut url = Url::parse(UPLOAD)?;
        url.query_pairs_mut()
            .append_pair("uploadType", "multipart")
            .append_pair("fields", "id");
        let content_type = format!("multipart/related; boundary={boundary}");
        let response = request(
            "POST",
            &url,
            Some(&self.token),
            Some((&content_type, &body)),
        )?;
        if response.status == 409 {
            let existing = self.file(&upload.drive_id)?;
            ensure!(
                existing.version == upload.batch.version
                    && existing.revisions == upload.batch.revisions,
                "Существующий пакет Google Drive отличается от отправленного"
            );
            return Ok(());
        }
        response.check()
    }
}

struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    fn check(&self) -> Result<()> {
        ensure!(
            (200..300).contains(&self.status),
            "Google Drive вернул HTTP {}",
            self.status
        );
        Ok(())
    }

    fn json<T: DeserializeOwned>(&self) -> Result<T> {
        self.check()?;
        Ok(serde_json::from_slice(&self.body).context("Не удалось прочитать ответ Google")?)
    }
}

fn request(
    method: &str,
    url: &Url,
    token: Option<&str>,
    body: Option<(&str, &[u8])>,
) -> Result<Response> {
    // curl is provided by the supported macOS and Windows installations.
    // On Linux it is a runtime dependency. Disable user curlrc and pass the
    // authorization header through stdin, never as a process argument.
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/curl");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("curl");
    #[cfg(target_os = "windows")]
    let mut command = {
        let root = std::env::var_os("SystemRoot").context("Не найдена папка Windows")?;
        let mut command = Command::new(std::path::PathBuf::from(root).join("System32/curl.exe"));
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
        command
    };
    command
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--globoff",
            "--proto",
            "=https",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "--max-filesize",
            "134217728",
            "--request",
            method,
            "--write-out",
            "%{http_code}",
            "--url",
            url.as_str(),
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut data = None;
    let mut form = None;
    if let Some((content_type, bytes)) = body {
        command
            .arg("--header")
            .arg(format!("Content-Type: {content_type}"));
        if content_type == "application/x-www-form-urlencoded" {
            form = Some(std::str::from_utf8(bytes)?);
        } else {
            let mut temp = tempfile::NamedTempFile::new()?;
            temp.write_all(bytes)?;
            temp.flush()?;
            let mut path = std::ffi::OsString::from("@");
            path.push(temp.path());
            command.arg("--data-binary").arg(path);
            data = Some(temp);
        }
    }
    let mut child = command.spawn().context("Не удалось запустить curl")?;
    let config_result = (|| -> Result<()> {
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(token) = token {
                writeln!(stdin, "header = \"Authorization: Bearer {token}\"")?;
            }
            if let Some(form) = form {
                // All values have been URL-encoded. Keeping the token form on
                // stdin also keeps refresh tokens out of temporary files.
                writeln!(stdin, "data = \"{form}\"")?;
            }
        }
        Ok(())
    })();
    let output = child.wait_with_output()?;
    drop(data);
    config_result?;
    ensure!(
        output.status.success(),
        "Нет связи с Google Drive (curl {})",
        output.status
    );
    let mut body = output.stdout;
    let end = body.len().checked_sub(3).context("Нет ответа от Google")?;
    let status = std::str::from_utf8(&body[end..])?.parse()?;
    body.truncate(end);
    Ok(Response { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_desktop_client_is_available_without_json_file() {
        let client = Client::bundled().unwrap().unwrap();
        assert!(client.id().ends_with(".apps.googleusercontent.com"));
        assert!(!client.id().contains(char::is_whitespace));
    }
}
