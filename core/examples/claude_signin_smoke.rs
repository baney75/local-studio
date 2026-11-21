use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, Delivery, ModelKey, ModelOption, Project, PromptDraft, ProviderCredential,
    ProviderStatus, Settings, Studio, StudioConfig, ThreadSettings, Update, Waker, WorktreeChoice,
};
use serde_json::Map;

const STEP_TIMEOUT: Duration = Duration::from_secs(90);
const REFUSAL_TIMEOUT: Duration = Duration::from_secs(20);
const ANTHROPIC: &str = "anthropic";
const CLAUDE_CODE: &str = "claude-code";
const SIGNED_IN: &str = r#"{"anthropic":{"type":"oauth","refresh":"","access":"","expires":4102444800000}}"#;
const API_KEY: &str = r#"{"anthropic":{"type":"api_key","key":"sk-ant-local-studio-fake-not-real"}}"#;
const HAIKU: &str = "claude-haiku-5-5";
const DEFAULT_HAIKU: &str = r#"{"defaultProvider":"anthropic","defaultModel":"claude-haiku-5-5"}"#;
const STUB: &str = "smoke-stub";
/// A keyless endpoint on a closed loopback port: listed and selectable, and no model ever answers on it.
const STUB_MODELS: &str = r#"{"providers":{"smoke-stub":{"name":"Smoke stub","baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"none","models":[{"id":"stub-1","name":"stub-1"}]}}}"#;
const DEFAULT_STUB: &str = r#"{"defaultProvider":"smoke-stub","defaultModel":"stub-1"}"#;

#[derive(Default)]
struct Signal {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Waker for Signal {
    fn wake(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.ready.notify_one();
    }
}

impl Signal {
    fn wait(&self, timeout: Duration) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
}

#[derive(Default)]
struct View {
    connection: Option<Connection>,
    models: Vec<ModelOption>,
    providers: Option<Vec<ProviderStatus>>,
    errors: Vec<String>,
    failed: Vec<(String, String)>,
    done: Vec<String>,
    started: Vec<(String, String)>,
    notices: Vec<String>,
    default_seen: bool,
    default: Option<ModelKey>,
    projects: Vec<Project>,
    settings: Option<Settings>,
    threads: Vec<String>,
}

impl View {
    fn eukhe_loaded(&self) -> bool {
        self.models.iter().any(|model| model.provider != CLAUDE_CODE && model.provider != "codex")
    }

    fn listed(&self, provider: &str) -> Vec<&ModelOption> {
        self.models.iter().filter(|model| model.provider == provider).collect()
    }

    fn anthropic(&self) -> Option<&ProviderStatus> {
        self.providers.as_ref()?.iter().find(|provider| provider.id == ANTHROPIC)
    }

    fn failure(&self, request: &str) -> Option<&str> {
        self.failed.iter().find(|(id, _)| id == request).map(|(_, message)| message.as_str())
    }

    fn thread(&self, draft: &str) -> Option<&str> {
        self.started.iter().find(|(started, _)| started == draft).map(|(_, thread)| thread.as_str())
    }

    fn refused_notice(&self, after: usize) -> Option<&str> {
        self.notices[after..].iter().find(|text| text.contains("Claude Code")).map(String::as_str)
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    config: StudioConfig,
}

impl Harness {
    fn open(config: StudioConfig) -> anyhow::Result<Self> {
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config.clone(), Arc::clone(&signal) as Arc<dyn Waker>)?;
        Ok(Self { studio, signal, view: View::default(), config })
    }

    fn connect(&mut self) -> anyhow::Result<()> {
        self.pump_until("daemon launched and connected", |view| matches!(view.connection, Some(Connection::Ready)))
    }

    /// Shuts this Studio down, runs `between`, and opens a second Studio on the same data dir and daemon.
    fn restart(&mut self, between: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
        self.studio.shutdown();
        println!("ok   the Studio shut down");
        between()?;
        let next = Self::open(self.config.clone())?;
        *self = next;
        self.connect()
    }

    fn pump_until(&mut self, step: &str, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        self.pump_within(step, STEP_TIMEOUT, done)
    }

    fn pump_within(&mut self, step: &str, timeout: Duration, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            for update in self.studio.tick() {
                self.apply(update);
            }
            if done(&self.view) {
                println!("ok   {step}");
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("timed out waiting for: {step}");
            };
            self.signal.wait(left.min(Duration::from_millis(500)));
        }
    }

    fn apply(&mut self, update: Update) {
        let view = &mut self.view;
        match update {
            Update::Connection { state } => view.connection = Some(state),
            Update::Models { models, .. } => view.models = models,
            Update::Providers { providers, errors, .. } => {
                view.providers = Some(providers);
                view.errors = errors;
            }
            Update::RequestFailed { request, message } => view.failed.push((request, message)),
            Update::RequestDone { request, .. } => view.done.push(request),
            Update::ThreadStarted { draft, thread } => view.started.push((draft, thread)),
            Update::Notice { level, text } => {
                println!("     notice {level:?}: {text}");
                view.notices.push(text);
            }
            Update::DaemonDefault { model, .. } => {
                view.default_seen = true;
                view.default = model;
            }
            Update::Projects { projects } => view.projects = projects,
            Update::Settings { settings } => view.settings = Some(settings),
            Update::Threads { threads } => view.threads = threads.into_iter().map(|row| row.id).collect(),
            _ => {}
        }
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn unset(worktree: WorktreeChoice) -> ThreadSettings {
    ThreadSettings { provider: None, model: None, thinking_level: None, access: AccessMode::Supervised, worktree }
}

fn on(model: &str, worktree: WorktreeChoice) -> ThreadSettings {
    ThreadSettings {
        provider: Some(ANTHROPIC.to_string()),
        model: Some(model.to_string()),
        thinking_level: None,
        access: AccessMode::Supervised,
        worktree,
    }
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new("git").arg("-C").arg(dir).args(args).output()?;
    ensure!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write_private(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?
        .write_all(content.as_bytes())?;
    Ok(())
}

fn replace_private(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::remove_file(path).with_context(|| path.display().to_string())?;
    write_private(path, content)
}

fn agent_dir() -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("EUKHE_CODING_AGENT_DIR").context("agent dir")?))
}

/// Whether any file under `dir` holds `text`, so a refused prompt can be shown never to reach a session.
fn any_file_holds(dir: &Path, text: &str) -> anyhow::Result<bool> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(false) };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            if any_file_holds(&path, text)? {
                return Ok(true);
            }
        } else if std::fs::read(&path).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(text)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Waits until nothing accepts on `socket`, so the next Studio launches a fresh daemon.
fn daemon_gone(socket: &Path) -> anyhow::Result<()> {
    let deadline = Instant::now() + REFUSAL_TIMEOUT;
    while std::os::unix::net::UnixStream::connect(socket).is_ok() {
        ensure!(Instant::now() < deadline, "the daemon still answers on {}", socket.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("ok   the daemon is gone");
    Ok(())
}

fn stop_daemon(socket: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(socket).await?;
        client
            .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
            .await?;
        println!("ok   daemon stopped");
        anyhow::Ok(())
    })
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: claude_signin_smoke <eukhe> [scratch dir, default /tmp/ls-signin]";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let base = args.next().map_or_else(|| PathBuf::from("/tmp/ls-signin"), PathBuf::from);
    let root = base.join(format!("run-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(&root)?;
    std::env::set_var("LOCAL_STUDIO_CODEX_BIN", root.join("no-codex"));
    std::env::set_var("LOCAL_STUDIO_CLAUDE_BIN", root.join("no-claude"));
    run(&daemon, &root.join("oauth"), SIGNED_IN, Some(DEFAULT_HAIKU), signed_in)?;
    run(&daemon, &root.join("key"), API_KEY, None, api_key)?;
    run(&daemon, &root.join("project"), SIGNED_IN, Some(DEFAULT_STUB), project_default_refused)?;
    println!("PASS claude signin smoke ({})", root.display());
    Ok(())
}

fn run(
    daemon: &Path,
    root: &Path,
    auth: &str,
    eukhe_settings: Option<&str>,
    check: fn(&mut Harness) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let agent = root.join("agent");
    let home = root.join("home");
    std::fs::create_dir_all(&agent)?;
    std::fs::create_dir_all(&home)?;
    write_private(&agent.join("auth.json"), auth)?;
    write_private(&agent.join("models.json"), STUB_MODELS)?;
    if let Some(settings) = eukhe_settings {
        write_private(&agent.join("settings.json"), settings)?;
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    let socket = root.join("d.sock");
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
        controller_main: None,
    };
    let outcome = Harness::open(config).and_then(|mut harness| {
        let outcome = harness
            .connect()
            .and_then(|()| {
                harness.studio.load_models();
                harness.studio.load_providers();
                harness.pump_until("Eukhe's model catalog and the providers reached the GUI", |view| {
                    view.eukhe_loaded() && view.anthropic().is_some() && view.default_seen
                })
            })
            .and_then(|()| check(&mut harness));
        harness.studio.shutdown();
        outcome
    });
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped
}

fn signed_in(harness: &mut Harness) -> anyhow::Result<()> {
    println!("     auth.json marks anthropic as oauth with empty tokens");
    let view = &harness.view;
    let claude = view.listed(CLAUDE_CODE);
    ensure!(!claude.is_empty(), "the Claude Code provider is not listed");
    println!("ok   Claude Code stays listed: {:?}", claude.iter().map(|model| &model.id).collect::<Vec<_>>());
    let anthropic = view.listed(ANTHROPIC);
    ensure!(
        anthropic.is_empty(),
        "Eukhe offers {} anthropic models on a subscription sign-in: {:?}",
        anthropic.len(),
        anthropic.iter().map(|model| &model.id).collect::<Vec<_>>()
    );
    println!("ok   no Eukhe anthropic model is listed");
    let line = view
        .errors
        .iter()
        .find(|error| error.contains("Claude Code") && error.contains("claude CLI sign-in"))
        .context(format!("no line explains why and points to Claude Code: {:?}", view.errors))?;
    println!("ok   the providers list explains: {line}");
    let status = view.anthropic().context("anthropic status")?;
    ensure!(
        status.credential == ProviderCredential::SignedIn && !status.configured,
        "anthropic shows as ready: {status:?}"
    );
    println!("ok   the Anthropic row is not ready");
    harness.studio.start_thread("anthropic-draft".to_string(), None, prompt("ping"), on(HAIKU, WorktreeChoice::Local));
    let refusal = refused_start(harness, "anthropic-draft", "an Eukhe thread on anthropic")?;
    println!("ok   starting an Eukhe anthropic thread is refused: {refusal}");
    default_model_refused(harness)?;
    worktree_left_alone(harness)?;
    title_skipped(harness)?;
    commit_message_skipped(harness)?;
    project_default_allowed(harness)?;
    Ok(())
}

fn refused_start(harness: &mut Harness, draft: &str, what: &str) -> anyhow::Result<String> {
    harness.pump_until(&format!("{what} is answered"), |view| {
        view.failure(draft).is_some() || view.thread(draft).is_some()
    })?;
    let refusal = harness
        .view
        .failure(draft)
        .map(str::to_string)
        .context(format!("{what} started on the Claude subscription sign-in"))?;
    ensure!(refusal.contains("Claude Code"), "refusal does not point to Claude Code: {refusal}");
    Ok(refusal)
}

fn default_model_refused(harness: &mut Harness) -> anyhow::Result<()> {
    let default = harness.view.default.clone();
    println!("     Eukhe's settings.json default is {default:?}; Local Studio has no default provider");
    ensure!(
        default.as_ref().is_some_and(|key| key.provider == ANTHROPIC),
        "the daemon default is not anthropic, so the scenario proves nothing: {default:?}"
    );
    harness.studio.start_thread("default-draft".to_string(), None, prompt("ping"), unset(WorktreeChoice::Local));
    let refusal = refused_start(harness, "default-draft", "an Eukhe thread with no provider")?;
    println!("ok   a thread on Eukhe's anthropic default is refused: {refusal}");
    Ok(())
}

/// Makes a git repo named `name` beside the data dir with `files` committed, adds it as a project, and returns
/// its path and project id.
fn project_repo(harness: &mut Harness, name: &str, files: &[(&str, &str)]) -> anyhow::Result<(PathBuf, String)> {
    let data = PathBuf::from(std::env::var_os("LOCAL_STUDIO_DATA_DIR").context("data dir")?);
    let repo = data.parent().context("run dir")?.join(name);
    std::fs::create_dir_all(&repo)?;
    git(&repo, &["init", "--quiet", "--initial-branch", "main"])?;
    for (path, content) in files {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().context("file dir")?)?;
        std::fs::write(&path, content)?;
    }
    git(&repo, &["add", "--all"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=smoke",
            "-c",
            "user.email=smoke@localhost",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "start",
        ],
    )?;
    let repo = std::fs::canonicalize(&repo)?;
    harness.studio.add_project(repo.to_string_lossy().into_owned());
    harness.pump_until(&format!("the scratch repo {name} is a project"), |view| {
        view.projects.iter().any(|project| Path::new(&project.path) == repo)
    })?;
    let project =
        harness.view.projects.iter().find(|project| Path::new(&project.path) == repo).context("project")?.id.clone();
    Ok((repo, project))
}

/// Starts `settings` in a new worktree of `repo`, requires the refusal, and requires no branch or worktree left.
fn worktree_refused(
    harness: &mut Harness,
    repo: &Path,
    project: String,
    draft: &str,
    branch: &str,
    settings: ThreadSettings,
    what: &str,
) -> anyhow::Result<String> {
    let before = git(repo, &["branch", "--list", "local-studio/*"])?;
    let settings =
        ThreadSettings { worktree: WorktreeChoice::NewWorktree { branch: branch.to_string(), base: None }, ..settings };
    harness.studio.start_thread(draft.to_string(), Some(project), prompt("ping"), settings);
    let refusal = refused_start(harness, draft, what)?;
    let after = git(repo, &["branch", "--list", "local-studio/*"])?;
    ensure!(before == after, "a refused start left branches: before {before:?}, after {after:?}");
    let worktrees = git(repo, &["worktree", "list", "--porcelain"])?;
    ensure!(worktrees.matches("worktree ").count() == 1, "a refused start left a worktree:\n{worktrees}");
    Ok(refusal)
}

fn worktree_left_alone(harness: &mut Harness) -> anyhow::Result<()> {
    let (repo, project) = project_repo(harness, "repo", &[])?;
    let what = "an Eukhe anthropic thread in a new worktree";
    let refusal = worktree_refused(
        harness,
        &repo,
        project,
        "worktree-draft",
        "local-studio/1",
        on(HAIKU, WorktreeChoice::Local),
        what,
    )?;
    println!("ok   a refused worktree start leaves no local-studio/* branch or worktree: {refusal}");
    Ok(())
}

/// The global Eukhe default is `anthropic`, but this project's `.eukhe/settings.json` picks the stub, so the
/// thread's session runs on the stub and must start.
fn project_default_allowed(harness: &mut Harness) -> anyhow::Result<()> {
    let (_, project) = project_repo(harness, "stub-project", &[(".eukhe/settings.json", DEFAULT_STUB)])?;
    println!("     Eukhe's global default is anthropic; the project's .eukhe/settings.json default is {STUB}");
    let after = harness.view.notices.len();
    harness.studio.start_thread("stub-draft".to_string(), Some(project), prompt(""), unset(WorktreeChoice::Local));
    harness.pump_until("a thread on the project's own non-anthropic default is answered", |view| {
        view.thread("stub-draft").is_some() || view.failure("stub-draft").is_some()
    })?;
    if let Some(failure) = harness.view.failure("stub-draft") {
        bail!("a thread on the project's {STUB} default was refused: {failure}");
    }
    ensure!(
        harness.view.refused_notice(after).is_none(),
        "a thread on the project's {STUB} default drew the refusal: {:?}",
        &harness.view.notices[after..]
    );
    println!("ok   a thread on the project's {STUB} default starts while the global default is anthropic");
    Ok(())
}

fn title_skipped(harness: &mut Harness) -> anyhow::Result<()> {
    let mut settings = harness.view.settings.clone().context("no settings update")?;
    settings.text_generation_provider = Some(ANTHROPIC.to_string());
    settings.text_generation_model = Some(HAIKU.to_string());
    harness.studio.update_settings(settings);
    harness.pump_until("the text generation model is anthropic", |view| {
        view.settings.as_ref().is_some_and(|settings| settings.text_generation_provider.as_deref() == Some(ANTHROPIC))
    })?;
    let after = harness.view.notices.len();
    harness.studio.regenerate_title("no-such-thread".to_string());
    harness.pump_until("title generation on anthropic is answered", |view| {
        view.notices[after..].iter().any(|text| text.contains("title"))
    })?;
    let notice = harness
        .view
        .refused_notice(after)
        .map(str::to_string)
        .context(format!("title generation ran on the Claude sign-in: {:?}", &harness.view.notices[after..]))?;
    println!("ok   title generation on anthropic is skipped with a notice: {notice}");
    Ok(())
}

fn commit_message_skipped(harness: &mut Harness) -> anyhow::Result<()> {
    let mut settings = harness.view.settings.clone().context("no settings update")?;
    settings.text_generation_provider = None;
    settings.text_generation_model = None;
    settings.source_control_provider = None;
    settings.source_control_model = Some(HAIKU.to_string());
    harness.studio.update_settings(settings);
    harness.pump_until("the writer model is set with no provider, so Eukhe's anthropic default answers", |view| {
        view.settings.as_ref().is_some_and(|settings| {
            settings.source_control_model.is_some() && settings.text_generation_provider.is_none()
        })
    })?;
    let repo = harness.view.projects.first().context("project")?.path.clone();
    std::fs::write(Path::new(&repo).join("CHANGED.md"), "changed\n")?;
    let after = harness.view.notices.len();
    harness.studio.commit("commit-draft".to_string(), repo, None);
    harness.pump_until("a commit with no message is answered", |view| {
        view.done.iter().any(|id| id == "commit-draft") || view.failure("commit-draft").is_some()
    })?;
    let notice = harness.view.refused_notice(after).map(str::to_string).context(format!(
        "the commit message was written on the Claude sign-in: {:?}",
        &harness.view.notices[after..]
    ))?;
    println!("ok   the generated commit message is skipped with a notice: {notice}");
    Ok(())
}

fn api_key(harness: &mut Harness) -> anyhow::Result<()> {
    println!("     auth.json holds a fake anthropic API key");
    let view = &harness.view;
    ensure!(!view.listed(CLAUDE_CODE).is_empty(), "the Claude Code provider is not listed");
    println!("ok   Claude Code stays listed");
    let anthropic = view.listed(ANTHROPIC);
    ensure!(
        !anthropic.is_empty() && anthropic.iter().all(|model| model.configured),
        "an anthropic API key does not list its models: {anthropic:?}"
    );
    println!("ok   {} anthropic models are listed on the API key", anthropic.len());
    ensure!(
        !view.errors.iter().any(|error| error.contains("Claude Code")),
        "an API key shows the subscription line: {:?}",
        view.errors
    );
    let status = view.anthropic().context("anthropic status")?;
    ensure!(status.credential == ProviderCredential::StoredKey && status.configured, "anthropic status {status:?}");
    println!("ok   the Anthropic row is ready with a saved key");
    harness.studio.start_thread("restart-draft".to_string(), None, prompt(""), on(HAIKU, WorktreeChoice::Local));
    harness.pump_until("a second anthropic thread starts on the API key with no prompt sent", |view| {
        view.thread("restart-draft").is_some() || view.failure("restart-draft").is_some()
    })?;
    let restarted =
        harness.view.thread("restart-draft").context("the second API key thread did not start")?.to_string();
    existing_thread_refused(harness)?;
    restarted_thread_refused(harness, &restarted)
}

/// The upgrade case: a thread made on anthropic before the sign-in marker, reopened by a fresh Studio after it,
/// first with the daemon (and the thread's resident session) kept, then with the daemon stopped too.
fn restarted_thread_refused(harness: &mut Harness, thread: &str) -> anyhow::Result<()> {
    let auth = agent_dir()?.join("auth.json");
    let socket = PathBuf::from(harness.config.socket_path.clone().context("socket")?);
    for (round, stop) in [("daemon kept", false), ("daemon stopped", true)] {
        replace_private(&auth, API_KEY)?;
        println!("     {round}: auth.json holds the fake API key again; thread {thread} is on anthropic");
        harness.restart(|| {
            if stop {
                stop_daemon(&socket)?;
                daemon_gone(&socket)?;
            }
            replace_private(&auth, SIGNED_IN)?;
            println!("     auth.json now marks anthropic as oauth; a second Studio opens on the same data dir");
            Ok(())
        })?;
        harness.pump_until("the reopened Studio lists the anthropic thread", |view| {
            view.threads.iter().any(|id| id == thread)
        })?;
        let request = format!("restart-send-{stop}");
        harness.studio.send(request.clone(), thread.to_string(), prompt("ping"), Delivery::Queue);
        harness
            .pump_within("a send on the reopened anthropic thread is refused", REFUSAL_TIMEOUT, |view| {
                view.failure(&request).is_some()
            })
            .context("the send after a restart was accepted and went to Eukhe's anthropic model")?;
        let refusal = harness.view.failure(&request).unwrap_or_default().to_string();
        ensure!(refusal.contains("Claude Code"), "the send after a restart failed for another reason: {refusal}");
        println!("ok   {round}: sending on an anthropic thread reopened after a restart is refused: {refusal}");
    }
    Ok(())
}

/// The global Eukhe default is the stub, but this project's `.eukhe/settings.json` picks `anthropic`: a thread
/// with no provider there must be refused before any prompt reaches its session, locally and in a new worktree.
fn project_default_refused(harness: &mut Harness) -> anyhow::Result<()> {
    let default = harness.view.default.clone();
    println!("     Eukhe's global settings.json default is {default:?}; Local Studio has no default provider");
    ensure!(
        default.as_ref().is_some_and(|key| key.provider == STUB),
        "the global daemon default is not the stub, so the scenario proves nothing: {default:?}"
    );
    let (repo, project) = project_repo(harness, "anthropic-project", &[(".eukhe/settings.json", DEFAULT_HAIKU)])?;
    println!("     the project's .eukhe/settings.json default is anthropic");
    let text = "ping-project-default";
    harness.studio.start_thread(
        "project-draft".to_string(),
        Some(project.clone()),
        prompt(text),
        unset(WorktreeChoice::Local),
    );
    let refusal = refused_start(harness, "project-draft", "an Eukhe thread on the project's anthropic default")?;
    println!("ok   a thread on the project's anthropic default is refused: {refusal}");
    let what = "an Eukhe thread on the project's anthropic default in a new worktree";
    let refusal = worktree_refused(
        harness,
        &repo,
        project,
        "project-worktree",
        "local-studio/2",
        unset(WorktreeChoice::Local),
        what,
    )?;
    println!("ok   in a new worktree it is refused, leaving no local-studio/* branch or worktree: {refusal}");
    let sessions = agent_dir()?.join("sessions");
    ensure!(!any_file_holds(&sessions, text)?, "the refused prompt reached a session under {}", sessions.display());
    println!("ok   the refused prompt reached no session");
    Ok(())
}

fn existing_thread_refused(harness: &mut Harness) -> anyhow::Result<()> {
    harness.studio.start_thread("key-draft".to_string(), None, prompt(""), on(HAIKU, WorktreeChoice::Local));
    harness.pump_until("an anthropic thread starts on the API key with no prompt sent", |view| {
        view.thread("key-draft").is_some() || view.failure("key-draft").is_some()
    })?;
    let thread = harness.view.thread("key-draft").context("the API key thread did not start")?.to_string();
    let agent = PathBuf::from(std::env::var_os("EUKHE_CODING_AGENT_DIR").context("agent dir")?);
    std::fs::remove_file(agent.join("auth.json"))?;
    write_private(&agent.join("auth.json"), SIGNED_IN)?;
    println!("     auth.json now marks anthropic as oauth; the thread's model is still anthropic");
    harness.studio.send("key-send".to_string(), thread.clone(), prompt("ping"), Delivery::Queue);
    harness
        .pump_within("a send on the existing anthropic thread is refused", REFUSAL_TIMEOUT, |view| {
            view.failure("key-send").is_some()
        })
        .context("the send was accepted and went to Eukhe's anthropic model")?;
    let refusal = harness.view.failure("key-send").unwrap_or_default().to_string();
    ensure!(refusal.contains("Claude Code"), "the send failed for another reason: {refusal}");
    println!("ok   sending on an existing anthropic thread is refused: {refusal}");
    let after = harness.view.notices.len();
    harness.studio.fork_thread(thread);
    harness.pump_until("forking the anthropic thread is answered", |view| {
        view.notices.len() > after || view.started.iter().any(|(draft, _)| draft.starts_with("fork:"))
    })?;
    let notice = harness
        .view
        .refused_notice(after)
        .map(str::to_string)
        .context(format!("the fork was not refused: {:?}", &harness.view.notices[after..]))?;
    println!("ok   forking an existing anthropic thread is refused: {notice}");
    Ok(())
}
