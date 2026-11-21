use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::{Capability, Consent, Grants, Plugin};

pub const SOCKET_ENV: &str = "LOCAL_STUDIO_ASK";
pub const TOKEN_ENV: &str = "LOCAL_STUDIO_ASK_TOKEN";
pub const THREAD_ENV: &str = "LOCAL_STUDIO_THREAD";
pub const NOTE_TYPE: &str = "local-studio-ask";
pub const MAX_LINE: usize = 16 * 1024;
pub const MAX_TEXT: usize = 2000;
pub const MAX_CHOICES: usize = 8;
pub const MAX_CHOICE: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Answered(String),
    Cancelled,
    Refused(String),
}

impl Outcome {
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Outcome::Answered(answer) => json!({ "status": "answered", "answer": answer }),
            Outcome::Cancelled => json!({ "status": "cancelled" }),
            Outcome::Refused(error) => json!({ "status": "refused", "error": error }),
        }
    }
}

pub struct Ask {
    pub plugin: String,
    pub thread: String,
    pub request: String,
    pub text: String,
    pub choices: Vec<String>,
    pub reply: oneshot::Sender<Outcome>,
}

pub enum Event {
    Ask(Ask),
    Gone { thread: String, request: String },
}

#[derive(Clone, Default)]
pub struct Tokens {
    inner: Arc<Mutex<TokenTable>>,
}

#[derive(Default)]
struct TokenTable {
    issued: HashMap<(String, String), String>,
    holders: HashMap<String, (String, String)>,
}

impl Tokens {
    pub fn issue(&self, plugin: &str, thread: &str) -> Result<String, String> {
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (plugin.to_string(), thread.to_string());
        if let Some(token) = table.issued.get(&key) {
            return Ok(token.clone());
        }
        let mut bytes = [0_u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|error| format!("no random token: {error}"))?;
        let token = super::install::hex(&bytes);
        table.holders.insert(digest(&token), key.clone());
        table.issued.insert(key, token.clone());
        Ok(token)
    }

    #[must_use]
    pub fn holder(&self, token: &str) -> Option<(String, String)> {
        let table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        table.holders.get(&digest(token)).cloned()
    }

    pub fn forget_thread(&self, thread: &str) {
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        table.issued.retain(|(_, owner), _| owner != thread);
        table.holders.retain(|_, (_, owner)| owner != thread);
    }
}

fn digest(token: &str) -> String {
    super::install::hex(&Sha256::digest(token.as_bytes()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: Option<String>,
    thread: Option<String>,
    text: String,
    #[serde(default)]
    choices: Vec<String>,
}

pub fn parse(line: &[u8], tokens: &Tokens) -> Result<(String, String, String, Vec<String>), String> {
    let request: Request = serde_json::from_slice(line).map_err(|error| format!("bad request: {error}"))?;
    let token = request.token.filter(|token| !token.is_empty()).ok_or("a question needs the plugin's token")?;
    let (plugin, thread) = tokens.holder(&token).ok_or("unknown token")?;
    if request.thread.as_ref().is_some_and(|asked| *asked != thread) {
        return Err("the token belongs to another thread".to_string());
    }
    let text = request.text.trim().to_string();
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return Err(format!("text must be 1 to {MAX_TEXT} characters"));
    }
    if request.choices.len() > MAX_CHOICES {
        return Err(format!("at most {MAX_CHOICES} choices"));
    }
    let mut choices: Vec<String> = Vec::with_capacity(request.choices.len());
    for choice in request.choices {
        let choice = choice.trim().to_string();
        if choice.is_empty() || choice.chars().count() > MAX_CHOICE {
            return Err(format!("each choice must be 1 to {MAX_CHOICE} characters"));
        }
        if choices.contains(&choice) {
            return Err(format!("duplicate choice: {choice}"));
        }
        choices.push(choice);
    }
    Ok((plugin, thread, text, choices))
}

#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
    let preferred = data_dir.join("ask.sock");
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    let tag: String = digest(&data_dir.to_string_lossy()).chars().take(12).collect();
    std::env::temp_dir().join(format!("ls-ask-{tag}.sock"))
}

#[must_use]
pub fn asks(plugin: &Plugin, grants: &Grants) -> bool {
    plugin.asks(Capability::AskUser) && plugin.asks(Capability::McpServer) && grants.consent(plugin) == Consent::Ready
}

pub fn acp_servers(
    plugins: &[Plugin],
    grants: &Grants,
    socket: &Path,
    thread: &str,
    tokens: &Tokens,
) -> (Vec<Value>, Vec<String>) {
    let asking: Vec<Plugin> = plugins.iter().filter(|plugin| asks(plugin, grants)).cloned().collect();
    let mut servers = Vec::new();
    let mut errors = Vec::new();
    for (name, entry) in super::mcp_servers(&asking, grants) {
        let Some(plugin) = asking.iter().find(|plugin| plugin.mcp_name() == name) else { continue };
        let mut env = Map::new();
        let mut missing = Vec::new();
        for (key, reference) in entry.get("env").and_then(Value::as_object).into_iter().flatten() {
            let host = reference.get("env").and_then(Value::as_str).unwrap_or_default();
            match std::env::var(host) {
                Ok(value) => {
                    env.insert(key.clone(), Value::String(value));
                }
                Err(_) => missing.push(host.to_string()),
            }
        }
        if !missing.is_empty() {
            errors.push(format!("{name} is off in this thread: {} is not set", missing.join(", ")));
            continue;
        }
        let token = match tokens.issue(&plugin.id, thread) {
            Ok(token) => token,
            Err(error) => {
                errors.push(format!("{name} cannot ask you questions: {error}"));
                continue;
            }
        };
        env.insert(SOCKET_ENV.to_string(), Value::String(socket.to_string_lossy().into_owned()));
        env.insert(TOKEN_ENV.to_string(), Value::String(token));
        env.insert(THREAD_ENV.to_string(), Value::String(thread.to_string()));
        servers.push(json!({
            "type": "stdio",
            "name": name,
            "command": entry["command"],
            "args": entry["args"],
            "cwd": entry["cwd"],
            "env": env,
        }));
    }
    (servers, errors)
}

#[cfg(unix)]
pub use server::serve;

#[cfg(unix)]
mod server {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::{oneshot, Semaphore};

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{parse, Ask, Event, Outcome, Tokens, MAX_LINE};

    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_CONNECTIONS: usize = 64;
    static NEXT: AtomicU64 = AtomicU64::new(1);

    pub fn serve(
        path: &Path,
        tokens: Tokens,
        events: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> std::io::Result<tokio::task::JoinHandle<()>> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket()) {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let own = std::fs::metadata(path)?.uid();
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        Ok(tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
                let (tokens, events) = (tokens.clone(), Arc::clone(&events));
                tokio::spawn(async move {
                    connection(stream, own, &tokens, &events).await;
                    drop(permit);
                });
            }
        }))
    }

    async fn connection(stream: UnixStream, own: u32, tokens: &Tokens, events: &Arc<dyn Fn(Event) + Send + Sync>) {
        if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(own) {
            return;
        }
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read.take(MAX_LINE as u64));
        let mut line = Vec::new();
        let read_line = tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut line)).await;
        let outcome = match read_line {
            Ok(Ok(_)) if line.ends_with(b"\n") => match parse(&line, tokens) {
                Ok((plugin, thread, text, choices)) => {
                    let request = format!("ask-{}", NEXT.fetch_add(1, Ordering::Relaxed));
                    let (reply, answer) = oneshot::channel();
                    events(Event::Ask(Ask {
                        plugin,
                        thread: thread.clone(),
                        request: request.clone(),
                        text,
                        choices,
                        reply,
                    }));
                    let mut rest = reader.into_inner().into_inner();
                    let mut sink = [0_u8; 256];
                    let mut answer = answer;
                    loop {
                        tokio::select! {
                            outcome = &mut answer => break outcome.unwrap_or(Outcome::Cancelled),
                            read = rest.read(&mut sink) => if matches!(read, Ok(0) | Err(_)) {
                                events(Event::Gone { thread, request });
                                return;
                            },
                        }
                    }
                }
                Err(error) => Outcome::Refused(error),
            },
            Ok(Ok(_)) => Outcome::Refused(format!("send one JSON line under {MAX_LINE} bytes")),
            Ok(Err(error)) => Outcome::Refused(format!("read: {error}")),
            Err(_) => Outcome::Refused("no request within 5 s".to_string()),
        };
        let mut text = outcome.to_value().to_string();
        text.push('\n');
        let _ = write.write_all(text.as_bytes()).await;
        let _ = write.shutdown().await;
    }
}
