use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use super::{install, load, sync_agent, tree_hash, Capability, Catalog, Consent, Grants, Plugin, Source};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("lsp-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create {}: {error}", dir.display()));
        Self(dir.canonicalize().unwrap_or(dir))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| panic!("mkdir {}: {error}", parent.display()));
    }
    std::fs::write(path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn served(dir: &Path, id: &str, version: &str, capabilities: &[&str], server: &str) -> PathBuf {
    let names: Vec<String> = capabilities.iter().map(|name| format!("\"{name}\"")).collect();
    write(
        &dir.join("plugin.toml"),
        &format!(
            "id = \"{id}\"\nversion = \"{version}\"\ndescription = \"A test plugin\"\ncapabilities = [{}]\n[mcp]\ncommand = \"bin/server\"\n",
            names.join(", ")
        ),
    );
    let bin = dir.join("bin/server");
    write(&bin, &format!("#!/bin/sh\necho {server}\n"));
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("chmod {}: {error}", bin.display()));
    dir.to_path_buf()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn repo(dir: &Path) -> String {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "plugin"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn catalog(id: &str, repo: &Path, rev: &str) -> Catalog {
    let tree = tree_hash(repo, &[]).unwrap_or_else(|error| panic!("{error}"));
    Catalog::parse(&format!(
        "[[plugin]]\nid = \"{id}\"\ngit = \"{}\"\nrev = \"{rev}\"\ntree = \"{tree}\"\n",
        repo.display()
    ))
    .unwrap_or_else(|error| panic!("{error}"))
}

fn installed(root: &Path, source: &Source, catalog: &Catalog) -> (Plugin, Consent) {
    let installed = install(root, source, catalog).unwrap_or_else(|errors| panic!("install {source:?}: {errors:?}"));
    (installed.plugin, installed.consent)
}

fn grant(root: &Path, id: &str, capabilities: &[Capability]) {
    let plugin = load(&root.join(id)).unwrap_or_else(|errors| panic!("{errors:?}"));
    let mut grants = Grants::load(root).unwrap_or_else(|error| panic!("{error}"));
    grants.grant(&plugin, capabilities).unwrap_or_else(|error| panic!("{error}"));
    grants.save(root).unwrap_or_else(|error| panic!("{error}"));
}

fn revoke(root: &Path, id: &str, capabilities: &[Capability]) {
    let mut grants = Grants::load(root).unwrap_or_else(|error| panic!("{error}"));
    grants.revoke(id, capabilities);
    grants.save(root).unwrap_or_else(|error| panic!("{error}"));
}

fn consent(root: &Path, id: &str) -> Consent {
    let plugin = load(&root.join(id)).unwrap_or_else(|errors| panic!("{errors:?}"));
    Grants::load(root).unwrap_or_else(|error| panic!("{error}")).consent(&plugin)
}

fn servers(agent: &Path) -> Value {
    let text = std::fs::read_to_string(agent.join("settings.json")).unwrap_or_else(|error| panic!("{error}"));
    let settings: Value = serde_json::from_str(&text).unwrap_or_else(|error| panic!("{error}"));
    settings.get("mcpServers").cloned().unwrap_or_else(|| json!({}))
}

fn sync(root: &Path, agent: &Path) {
    sync_agent(root, agent).unwrap_or_else(|error| panic!("sync: {error}"));
}

fn needs(missing: &[Capability]) -> Consent {
    Consent::NeedsConsent { missing: missing.to_vec() }
}

#[test]
fn a_same_id_install_from_another_source_needs_consent_and_gets_no_mcp_entry() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let agent = temp.0.join("agent");
    write(&agent.join("settings.json"), "{}");

    let reviewed = served(&temp.0.join("reviewed"), "toolbox", "1.0.0", &["mcp-server"], "reviewed");
    let rev = repo(&reviewed);
    let pinned = catalog("toolbox", &reviewed, &rev);
    let (_, first) = installed(&root, &Source::Catalog("toolbox".to_string()), &pinned);
    assert_eq!(first, needs(&[Capability::McpServer]));
    grant(&root, "toolbox", &[Capability::McpServer]);
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-toolbox").is_some(), "the granted plugin has no MCP entry");

    let other = served(&temp.0.join("other"), "toolbox", "1.0.0", &["mcp-server"], "other");
    let other_rev = repo(&other);
    let git = Source::Git { url: other.display().to_string(), rev: other_rev };
    let (_, from_git) = installed(&root, &git, &pinned);
    assert_eq!(from_git, needs(&[Capability::McpServer]), "a git install of another tree kept the catalog grant");
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-toolbox").is_none(), "sync wrote an MCP entry for unreviewed code");

    installed(&root, &Source::Catalog("toolbox".to_string()), &pinned);
    grant(&root, "toolbox", &[Capability::McpServer]);
    let folder = served(&temp.0.join("folder"), "toolbox", "1.0.0", &["mcp-server"], "folder");
    let (_, from_dir) = installed(&root, &Source::Dir(folder), &pinned);
    assert_eq!(from_dir, needs(&[Capability::McpServer]), "a folder install of another tree kept the catalog grant");
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-toolbox").is_none());

    installed(&root, &Source::Catalog("toolbox".to_string()), &pinned);
    grant(&root, "toolbox", &[Capability::McpServer]);
    assert_eq!(consent(&root, "toolbox"), Consent::Ready);
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-toolbox").is_some());
    write(&root.join("toolbox/bin/server"), "#!/bin/sh\necho swapped\n");
    assert_eq!(consent(&root, "toolbox"), needs(&[Capability::McpServer]), "a tree changed in place kept its grant");
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-toolbox").is_none());
}

#[test]
fn a_reinstall_of_the_same_tree_keeps_its_grant() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let reviewed = served(&temp.0.join("reviewed"), "toolbox", "1.0.0", &["mcp-server"], "reviewed");
    let rev = repo(&reviewed);
    let pinned = catalog("toolbox", &reviewed, &rev);
    installed(&root, &Source::Catalog("toolbox".to_string()), &pinned);
    grant(&root, "toolbox", &[Capability::McpServer]);
    let (_, again) = installed(&root, &Source::Catalog("toolbox".to_string()), &pinned);
    assert_eq!(again, Consent::Ready);
}

#[test]
fn a_capability_dropped_and_added_back_asks_again() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let source = temp.0.join("src/toolbox");
    let both = ["mcp-server", "screen-capture"];

    served(&source, "toolbox", "1.0.0", &both, "one");
    installed(&root, &Source::Dir(source.clone()), &Catalog::default());
    grant(&root, "toolbox", &[Capability::McpServer, Capability::ScreenCapture]);
    assert_eq!(consent(&root, "toolbox"), Consent::Ready);

    served(&source, "toolbox", "2.0.0", &["mcp-server"], "two");
    let (_, two) = installed(&root, &Source::Dir(source.clone()), &Catalog::default());
    assert_eq!(two, Consent::Ready, "an update from the same source that asks for nothing new asked again");

    served(&source, "toolbox", "3.0.0", &both, "three");
    let (_, three) = installed(&root, &Source::Dir(source.clone()), &Catalog::default());
    assert_eq!(three, needs(&[Capability::ScreenCapture]), "a capability granted to 1.0.0 carried over to 3.0.0");
    grant(&root, "toolbox", &[Capability::McpServer]);
    assert_eq!(consent(&root, "toolbox"), needs(&[Capability::ScreenCapture]));
    grant(&root, "toolbox", &[Capability::ScreenCapture]);
    assert_eq!(consent(&root, "toolbox"), Consent::Ready);
}

#[test]
fn a_slice_two_grant_without_a_tree_needs_consent_again() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    served(&root.join("toolbox"), "toolbox", "1.0.0", &["mcp-server"], "old");
    write(
        &root.join("grants.json"),
        r#"{"plugins": {"toolbox": {"version": "1.0.0", "capabilities": ["mcp-server"]}}}"#,
    );
    assert_eq!(consent(&root, "toolbox"), needs(&[Capability::McpServer]));
}

#[test]
fn settings_keep_mcp_servers_local_studio_did_not_write() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let agent = temp.0.join("agent");
    let user = json!({
        "plugin-manager": {"type": "stdio", "command": "/usr/local/bin/manager"},
        "my-server": {"type": "stdio", "command": "/usr/local/bin/mine", "args": ["serve"]},
    });
    write(&agent.join("settings.json"), &json!({ "defaultModel": "m", "mcpServers": user }).to_string());
    let kept = |step: &str| {
        let servers = servers(&agent);
        for name in ["plugin-manager", "my-server"] {
            assert_eq!(servers.get(name), user.get(name), "{step} dropped the user's {name} entry: {servers}");
        }
    };

    std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    sync(&root, &agent);
    kept("an empty scan");

    let source = served(&temp.0.join("src/toolbox"), "toolbox", "1.0.0", &["mcp-server"], "box");
    installed(&root, &Source::Dir(source), &Catalog::default());
    sync(&root, &agent);
    kept("an install");
    grant(&root, "toolbox", &[Capability::McpServer]);
    sync(&root, &agent);
    kept("a grant");
    assert_eq!(servers(&agent)["plugin-toolbox"]["localStudioPlugin"], json!(true));
    revoke(&root, "toolbox", &[]);
    sync(&root, &agent);
    kept("a revoke");
    assert!(servers(&agent).get("plugin-toolbox").is_none(), "the revoked plugin kept its entry");

    let old = served(&temp.0.join("src/old"), "old", "1.0.0", &["mcp-server"], "old");
    installed(&root, &Source::Dir(old), &Catalog::default());
    grant(&root, "old", &[Capability::McpServer]);
    sync(&root, &agent);
    assert!(servers(&agent).get("plugin-old").is_some());
    std::fs::remove_dir_all(root.join("old")).unwrap_or_else(|error| panic!("{error}"));
    sync(&root, &agent);
    kept("a removal");
    assert!(servers(&agent).get("plugin-old").is_none(), "the removed plugin's tagged entry stayed");
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(agent.join("settings.json")).unwrap_or_default())
            .unwrap_or_default()["defaultModel"],
        json!("m")
    );
}

#[test]
fn untagged_entries_count_as_ours_only_when_they_run_code_under_the_plugins_root() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let agent = temp.0.join("agent");
    let source = served(&temp.0.join("src/toolbox"), "toolbox", "1.0.0", &["mcp-server"], "box");
    let (plugin, _) = installed(&root, &Source::Dir(source), &Catalog::default());
    let command = plugin.dir.join("bin/server").canonicalize().unwrap_or_else(|error| panic!("{error}"));
    let written = json!({"type": "stdio", "command": command.to_string_lossy(), "args": [], "cwd": plugin.dir.canonicalize().unwrap_or_default().to_string_lossy()});
    let gone = json!({"type": "stdio", "command": "/opt/gone/bin/server"});
    write(
        &agent.join("settings.json"),
        &json!({ "mcpServers": { "plugin-toolbox": written, "plugin-gone": gone } }).to_string(),
    );
    sync(&root, &agent);
    let servers = servers(&agent);
    assert!(
        servers.get("plugin-toolbox").is_none(),
        "an entry Local Studio wrote before tags stayed after consent ended"
    );
    assert_eq!(servers.get("plugin-gone"), Some(&gone), "an untagged entry for no installed plugin was removed");

    let moved = json!({"type": "stdio", "command": plugin.dir.join("bin/old-server").to_string_lossy()});
    write(&root.join("broken/plugin.toml"), "id = \"broken\"\n");
    let broken = json!({"type": "stdio", "command": "/usr/bin/env", "cwd": root.join("broken").to_string_lossy()});
    write(
        &agent.join("settings.json"),
        &json!({ "mcpServers": { "plugin-toolbox": moved, "plugin-broken": broken } }).to_string(),
    );
    sync(&root, &agent);
    assert_eq!(servers_at(&agent, "plugin-toolbox"), None, "plugin-toolbox stayed after its command moved");
    assert_eq!(
        servers_at(&agent, "plugin-broken"),
        Some(broken),
        "an entry with only its cwd under the root was removed"
    );

    let mine = json!({"type": "stdio", "command": "/usr/local/bin/toolbox"});
    write(&agent.join("settings.json"), &json!({ "mcpServers": { "plugin-toolbox": mine } }).to_string());
    grant(&root, "toolbox", &[Capability::McpServer]);
    let error = sync_agent(&root, &agent).err().unwrap_or_default();
    assert!(error.contains("plugin-toolbox"), "the skipped plugin entry raised no error: {error:?}");
    assert_eq!(servers_at(&agent, "plugin-toolbox"), Some(mine), "a user's own plugin-toolbox entry was overwritten");
}

fn servers_at(agent: &Path, name: &str) -> Option<Value> {
    servers(agent).get(name).cloned()
}

#[test]
fn files_the_runtime_or_finder_write_do_not_end_consent() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let agent = temp.0.join("agent");
    write(&agent.join("settings.json"), "{}");
    let source = served(&temp.0.join("src/toolbox"), "toolbox", "1.0.0", &["mcp-server"], "box");
    installed(&root, &Source::Dir(source), &Catalog::default());
    grant(&root, "toolbox", &[Capability::McpServer]);
    write(&root.join("toolbox/.DS_Store"), "finder");
    write(&root.join("toolbox/bin/.DS_Store"), "finder");
    assert_eq!(consent(&root, "toolbox"), Consent::Ready, "a .DS_Store file ended consent");
    sync(&root, &agent);
    assert_eq!(servers(&agent)["plugin-toolbox"]["env"]["PYTHONDONTWRITEBYTECODE"], json!({"env": "PATH"}));

    let manifest = root.join("toolbox/plugin.toml");
    let text = std::fs::read_to_string(&manifest).unwrap_or_default();
    write(&manifest, &text.replace("command = \"bin/server\"", "command = \"bin/server\"\nargs = [\"bin/.DS_Store\"]"));
    assert!(load(&root.join("toolbox")).is_err(), "a server that runs an unhashed .DS_Store file loaded");
}

#[test]
fn a_command_inside_dot_git_is_covered_by_consent() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let dir = root.join("toolbox");
    served(&dir, "toolbox", "1.0.0", &["mcp-server"], "box");
    let text = std::fs::read_to_string(dir.join("plugin.toml")).unwrap_or_default();
    write(&dir.join("plugin.toml"), &text.replace("bin/server", ".git/server"));
    write(&dir.join(".git/server"), "#!/bin/sh\necho reviewed\n");
    grant(&root, "toolbox", &[Capability::McpServer]);
    assert_eq!(consent(&root, "toolbox"), Consent::Ready);
    write(&dir.join(".git/server"), "#!/bin/sh\necho swapped\n");
    assert_eq!(consent(&root, "toolbox"), needs(&[Capability::McpServer]), "an edit inside .git kept consent");
}

#[test]
fn a_slice_two_grant_renewed_once_survives_a_catalog_update() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let slice_two = r#"{"plugins": {"toolbox": {"version": "1.0.0", "capabilities": ["mcp-server"]}}}"#;
    let reviewed = served(&temp.0.join("reviewed"), "toolbox", "1.0.0", &["mcp-server"], "one");
    let one = catalog("toolbox", &reviewed, &repo(&reviewed));
    installed(&root, &Source::Catalog("toolbox".to_string()), &one);
    served(&reviewed, "toolbox", "2.0.0", &["mcp-server"], "two");
    git(&reviewed, &["commit", "-q", "-am", "two"]);
    let two = catalog("toolbox", &reviewed, &git(&reviewed, &["rev-parse", "HEAD"]));

    write(&root.join("grants.json"), slice_two);
    grant(&root, "toolbox", &[Capability::McpServer]);
    let (_, updated) = installed(&root, &Source::Catalog("toolbox".to_string()), &two);
    assert_eq!(updated, Consent::Ready, "a slice 2 grant asked twice");

    write(&root.join("grants.json"), slice_two);
    grant(&root, "toolbox", &[Capability::McpServer]);
    let folder = served(&temp.0.join("folder"), "toolbox", "3.0.0", &["mcp-server"], "folder");
    let (_, other) = installed(&root, &Source::Dir(folder), &two);
    assert_eq!(other, needs(&[Capability::McpServer]), "a renewed slice 2 grant carried over to a folder install");
}
