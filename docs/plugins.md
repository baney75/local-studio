# Plugin consent

What a capability grant covers, when it carries over to an update, and which `mcpServers` entries Local Studio owns. The code is `core/src/plugins/grants.rs`, `core/src/plugins/install.rs` and `sync_mcp_servers` in `core/src/plugins.rs`.

## Grants

`<data dir>/plugins/grants.json` holds one record per plugin id: the version, the granted capabilities, the installed tree hash (`tree`, the same sha256 as `local-studio plugin hash`) and the source the code came from (`catalog <id>`, `git <url>` or `folder <path>`). A plugin that asks for no capabilities is always ready. A plugin that asks for any is ready only when:

- the record's `tree` equals the hash of the plugin folder as it is now, and
- every capability it asks for is in the record.

Anything else is "needs consent": its skills, prompts and commands still work, and it gets no MCP server. Local Studio hashes the folder each time it checks consent, so a folder changed in place, or replaced by hand, needs consent again. That hash covers the whole folder, `.git` included (`plugin hash` skips a top-level `.git` for catalog pins), and skips only `.DS_Store` files, which Finder writes and no runtime loads. A plugin whose `[mcp]` command or args name a `.DS_Store` file does not load. A plugin that keeps state should keep it outside its own folder; Local Studio's entry for its server sets `PYTHONDONTWRITEBYTECODE`, so Python writes no `__pycache__` there.

Grants from before this rule have no `tree`, so each plugin with capabilities asks once more after the upgrade. Granting it again records the tree and leaves the source empty (unknown origin). The next catalog install of that id counts as an update of it; an install from git or a folder drops it.

## Installs and updates

Every `local-studio plugin install` records the new tree and source:

- **Same tree** (a reinstall of the same bytes, from any source): the grant stays as it was, source included.
- **Same source, new tree** (an update): the grant keeps only the capabilities that both the granted version and the new version ask for. An update that asks for nothing new stays ready without a prompt, as slice 2 promised. A capability the new version asks for that the grant does not hold needs consent, including one that an earlier version had and a later one dropped: 1.0.0 granted `mcp-server` and `screen-capture`, 2.0.0 dropped `screen-capture`, so 3.0.0 asks for `screen-capture` again.
- **Another source, new tree**: the grant is dropped and every capability needs consent. Installing `gantrel` from a folder or another git URL never inherits the grant given to the catalog's `gantrel`. Install prints a line naming both sources when it drops a grant.

The source is the catalog id, the git URL without its commit, or the folder's absolute path. `plugin grant` on a folder whose tree has no install record (one copied in by hand) records it as `folder <plugin dir>`, unless the record is a grant from before this rule (see above).

`plugin revoke <id>` with no capabilities clears every capability and keeps the tree and source, so a later update from the same source still counts as an update.

## MCP server entries

Local Studio writes each ready plugin's server into `<agent dir>/settings.json` `mcpServers` as `plugin-<id>` with `"localStudioPlugin": true` and `"PYTHONDONTWRITEBYTECODE": {"env": "PATH"}` in its `env` (Eukhe takes env values only as host variable names, and Python needs any non-empty value). Eukhe ignores the extra field. On each sync Local Studio removes or rewrites only entries it wrote:

- an entry tagged `"localStudioPlugin": true`, or
- an untagged entry named `plugin-<name>` whose `command` or `cwd` lies inside the plugins folder (entries from before the tag, including one whose plugin moved its command or no longer loads).

Every other entry stays, whatever its name, including `plugin-manager` or an untagged `plugin-<name>` that runs code from outside the plugins folder. If a ready plugin's name is already taken by an entry Local Studio did not write, the user's entry stays, that plugin's server is off, and sync reports the name as an error (a notice in the app, an `error:` line in the CLI).

## Questions from plugins

A ready plugin that asks for both `mcp-server` and `ask-user` can ask the person a question and get the person's answer, never the model's. The code is `core/src/plugins/ask.rs` (socket, tokens, server entries) and `core/src/studio/ask.rs` (cards, answers, Stop).

- **Delivery.** The `settings.json` entry above never carries the ask variables. Before Local Studio sends a prompt to an idle session, it registers each asking plugin's server with that session as an ACP MCP server (`ReplaceAcpMcpServers`, owner `local-studio-ask`) under the same name `plugin-<id>`, with the same command, args and cwd, its host `env` references resolved, and three more variables: `LOCAL_STUDIO_ASK` (the socket path), `LOCAL_STUDIO_ASK_TOKEN` (32 random bytes as hex, one per plugin and thread) and `LOCAL_STUDIO_THREAD`. Eukhe looks up ACP servers before `settings.json`, so this entry wins in Local Studio threads. Other Eukhe frontends get the plain entry, so there the plugin has no `LOCAL_STUDIO_ASK` and must treat that as not confirmed. A plugin without the `ask-user` grant never gets these variables, and neither does the model's bash.
- **Socket.** `<data dir>/ask.sock`, or a short path in the temp folder when that one is too long for a Unix socket. Mode 0600; connections from another user are dropped. Send one JSON line within 5 s, under 16 KiB: `{"token": "...", "thread": "...", "text": "...", "choices": ["Yes", "No"]}`. `thread` is optional and must match the token's thread. `text` is 1 to 2000 characters, `choices` at most 8 unique labels of 1 to 120 characters, and any other field is refused. Keep the connection open. The reply is one JSON line: `{"status": "answered", "answer": "..."}`, `{"status": "cancelled"}` or `{"status": "refused", "error": "..."}`. Closing the connection first withdraws the question.
- **Who answers.** The socket has no answer message. Only `Studio::answer_questions`, which the desktop apps call when the person clicks a choice, presses Return on one or sends typed text in the card, and which a phone reaches through the Remote op `answer_questions`, settles a question. The answer is a choice label or the typed text.
- **Card.** The thread shows a question card labeled with the plugin's id and a plugin icon (manifests carry no icon of their own), counts as needing the person, and raises a `needs_answer` notification. An open question blocks only the plugin's tool call.
- **Stop and close.** Stop, archiving the thread or deleting it answers every open question in it `cancelled`. A plugin must treat `cancelled` as not confirmed.
- **Transcript.** Once the run is idle, Local Studio appends a displayed note to the session (custom type `local-studio-ask`): `<plugin> asked: <text>. You answered: <answer>` (no extra period when the question ends in one), or `cancelled`. The model sees it in its next request.
- **Limits.** Eukhe gives ACP stdio servers no `callTimeoutMs`, so the model's MCP call gives up after the kernel's default 60 s; the card stays until the person answers or the thread stops, and the plugin gets that answer on its open connection. The model's bash runs as the same user as the plugin, so a model that goes looking (`ps eww` on the plugin process, or the kernel's view of the session's MCP config) can find the token. With it, it can only raise a question as that plugin. It still cannot answer one.
