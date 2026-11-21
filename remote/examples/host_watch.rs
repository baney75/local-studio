use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{DaemonLaunch, Studio, StudioConfig, Update, Waker};
use serde_json::{Map, Value};

const WITHIN: Duration = Duration::from_secs(3);

struct Quiet;

impl Waker for Quiet {
    fn wake(&self) {}
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn health(port: u16) -> Option<u16> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let request = format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    answer.split_whitespace().nth(1).and_then(|code| code.parse().ok())
}

fn refused(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
}

fn until(step: &str, mut done: impl FnMut() -> bool) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        if done() {
            println!("ok   {step} ({} ms)", started.elapsed().as_millis());
            return Ok(());
        }
        if started.elapsed() >= WITHIN {
            bail!("timed out after {} s: {step}", WITHIN.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn install(bin: &Path, data: &Path, port: u16) -> anyhow::Result<()> {
    let output = Command::new(bin)
        .args(["install", "--no-load", "--json", "--port", &port.to_string(), "--data-dir"])
        .arg(data)
        .stdin(Stdio::null())
        .output()?;
    let printed = String::from_utf8_lossy(&output.stdout);
    ensure!(output.status.success(), "install failed: {printed}{}", String::from_utf8_lossy(&output.stderr));
    let value: Value = serde_json::from_str(printed.trim()).with_context(|| format!("install printed {printed}"))?;
    ensure!(value.get("state").and_then(Value::as_str) == Some("written"), "install answered {value}");
    println!("ok   install --no-load wrote serve.json for port {port}");
    Ok(())
}

fn watch(studio: &Arc<Studio>, bin: &Path, data: &Path) -> anyhow::Result<()> {
    let serve_json = data.join("remote").join("serve.json");
    ensure!(!serve_json.exists(), "{} exists before the check", serve_json.display());
    let host = local_studio_remote::host::start(Arc::clone(studio), Some(data))?.context("start returned no handle")?;
    let port = free_port()?;
    std::thread::sleep(Duration::from_millis(1500));
    ensure!(refused(port), "127.0.0.1:{port} answers before serve.json exists");
    println!("ok   with no serve.json the app serves nothing: 127.0.0.1:{port} refuses");
    install(bin, data, port)?;
    until("the running app serves /v1/health after serve.json appears", || health(port) == Some(200))?;
    std::fs::remove_file(&serve_json)?;
    until("the port refuses after serve.json is removed", || refused(port))?;
    let again = free_port()?;
    install(bin, data, again)?;
    until("the app serves again when serve.json comes back", || health(again) == Some(200))?;
    host.stop();
    ensure!(refused(again), "127.0.0.1:{again} still answers after the handle stopped");
    println!("ok   stopping the handle closes the listener");
    Ok(())
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn stop_daemon(socket: &Path, known: Option<u32>) -> anyhow::Result<()> {
    let mut pid = known;
    if let Ok((client, _events)) = DaemonClient::connect(socket).await {
        pid =
            client.hello().get("supervisorPid").and_then(Value::as_u64).and_then(|pid| u32::try_from(pid).ok()).or(pid);
        let command = DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() };
        client.request_ok(command).await.ok();
        client.close();
    }
    let Some(pid) = pid else { return Ok(()) };
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        ensure!(Instant::now() < deadline, "daemon {pid} is still running after Shutdown");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("ok   local daemon {pid} stopped");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: host_watch <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let eukhe = std::fs::canonicalize(PathBuf::from(args.next().context(usage)?))?;
    let root = PathBuf::from(args.next().context(usage)?);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let bin = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("local-studio-serve"))
        .filter(|bin| bin.is_file())
        .context("build local-studio-serve first: cargo build -p local-studio-remote --bins --examples")?;
    let (home, data, agent, socket) = (root.join("home"), root.join("data"), root.join("agent"), root.join("d.sock"));
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&agent)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: Some(data.to_string_lossy().into_owned()),
        launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
        controller_main: None,
        take_over_serve: true,
    };
    let studio = Studio::new(config, Arc::new(Quiet)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let result = watch(&studio, &bin, &data);
    let daemon = studio.tick().into_iter().rev().find_map(|update| match update {
        Update::Daemon { pid, .. } => pid,
        _ => None,
    });
    studio.shutdown();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let stopped = runtime.block_on(stop_daemon(&socket, daemon));
    let result = match (result, stopped) {
        (Err(error), Err(stop)) => {
            println!("FAIL {stop:#}");
            Err(error)
        }
        (result, stopped) => result.and(stopped),
    };
    match &result {
        Ok(()) => println!("PASS host watch"),
        Err(error) => println!("FAIL {error:#}"),
    }
    result
}
