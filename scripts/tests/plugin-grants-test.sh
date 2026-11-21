#!/usr/bin/env bash
# Plugin consent and mcpServers ownership (#48), driven through the real local-studio CLI on scratch
# HOME, LOCAL_STUDIO_DATA_DIR and EUKHE_CODING_AGENT_DIR. Each scenario prints PASS or FAIL; the exit
# code is the number of failures.
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SCRATCH=$2
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
failures=0
TIMEFORMAT=%R

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

fresh() {
  S=$SCRATCH/$1
  rm -rf "$S"
  mkdir -p "$S/h" "$S/d/plugins" "$S/a" "$S/src"
  D=$S/d
  A=$S/a
  echo '{}' > "$A/settings.json"
  mkdir -p "$S/src/syncer"
  printf 'id = "syncer"\nversion = "1.0.0"\ndescription = "Runs a sync"\n' > "$S/src/syncer/plugin.toml"
}

ls_() { HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A "$BIN" "$@"; }

sync_() { ls_ plugin install "$S/src/syncer" > /dev/null 2>&1; }

manifest() {
  mkdir -p "$1/bin"
  printf 'id = "%s"\nversion = "1.0.0"\ndescription = "A test plugin"\ncapabilities = ["mcp-server"]\n[mcp]\ncommand = "%s"\n' \
    "$(basename "$1")" "${2:-bin/server}" > "$1/plugin.toml"
}

script() { printf '#!/bin/sh\necho %s\n' "$2" > "$1"; chmod 755 "$1"; }

status() {
  ls_ plugin list 2>&1 | awk -v id="$1" '$1 == id { getline; sub(/^ +/, ""); print; found = 1; exit } END { if (!found) print "did not load" }'
}

entry() {
  python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])).get("mcpServers",{}); print("yes" if sys.argv[2] in s else "no")' \
    "$A/settings.json" "$2"
}

settings() { python3 -I -c 'import json,sys; json.dump({"mcpServers": json.loads(sys.argv[2])}, open(sys.argv[1], "w"))' "$A/settings.json" "$1"; }

edited() {
  local id=$1 label=$2
  sync_
  local after
  after=$(status "$id")
  echo "  after the edit, plugin list: $after"
  echo "  after a sync, plugin-$id in mcpServers: $(entry "$A" "plugin-$id")"
  if [ "$after" != "ready" ] && [ "$(entry "$A" "plugin-$id")" = "no" ]; then pass "$label"; else fail "$label"; fi
}

echo "== second-F8: a command symlinked outside the plugin"
fresh f8
P=$D/plugins/toolbox
manifest "$P"
mkdir -p "$S/out"
script "$S/out/server" reviewed
ln -s "$S/out/server" "$P/bin/server"
echo "  grant: $(ls_ plugin grant toolbox mcp-server 2>&1 | head -1)"
echo "  plugin-toolbox in mcpServers: $(entry "$A" plugin-toolbox)"
script "$S/out/server" swapped
edited toolbox "second-F8 command symlinked outside"

echo "== second-F8: a lib folder symlinked outside the plugin"
fresh f8lib
P=$D/plugins/libbox
manifest "$P"
printf '#!/bin/sh\n. "$(dirname "$0")/../lib/run.sh"\n' > "$P/bin/server"
chmod 755 "$P/bin/server"
mkdir -p "$S/outlib"
echo 'echo reviewed' > "$S/outlib/run.sh"
ln -s "$S/outlib" "$P/lib"
echo "  grant: $(ls_ plugin grant libbox mcp-server 2>&1 | head -1)"
echo 'echo swapped' > "$S/outlib/run.sh"
edited libbox "second-F8 lib symlinked outside"

echo "== sol-F2: a command symlinked to .DS_Store"
fresh f2ds
P=$D/plugins/dsbox
manifest "$P"
script "$P/.DS_Store" reviewed
ln -s ../.DS_Store "$P/bin/server"
echo "  grant: $(ls_ plugin grant dsbox mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited dsbox "sol-F2 command symlinked to .DS_Store"

echo "== first-F3 / sol-F1: untagged entries whose cwd, not command, is under the plugins root"
fresh f3
mytool="{\"type\": \"stdio\", \"command\": \"/usr/bin/python3\", \"cwd\": \"$D/plugins/mytool\"}"
manager="{\"type\": \"stdio\", \"command\": \"/usr/local/bin/manager\", \"cwd\": \"$D/plugins/x\"}"
escape="{\"type\": \"stdio\", \"command\": \"$D/plugins/../../h/tool\"}"
settings "{\"plugin-mytool\": $mytool, \"plugin-manager\": $manager, \"plugin-escape\": $escape}"
sync_
kept="plugin-mytool=$(entry "$A" plugin-mytool) plugin-manager=$(entry "$A" plugin-manager) plugin-escape=$(entry "$A" plugin-escape)"
echo "  after a sync: $kept"
if [ "$kept" = "plugin-mytool=yes plugin-manager=yes plugin-escape=yes" ]; then pass "first-F3/sol-F1 user entries kept"; else fail "first-F3/sol-F1 user entries kept"; fi
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
settings "{\"plugin-toolbox\": {\"type\": \"stdio\", \"command\": \"$P/bin/old-server\"}}"
sync_
echo "  an untagged entry whose command is under the root, after a sync: $(entry "$A" plugin-toolbox)"
if [ "$(entry "$A" plugin-toolbox)" = "no" ]; then pass "first-F3 pre-tag entry still migrated"; else fail "first-F3 pre-tag entry still migrated"; fi

echo "== first-F1: a plugin folder that holds a 200 MB file"
fresh f1
P=$D/plugins/bigbox
manifest "$P"
script "$P/bin/server" box
mkdir -p "$P/data"
dd if=/dev/urandom of="$P/data/blob.bin" bs=1048576 count=200 2> /dev/null
echo "  plugin grant bigbox mcp-server, seconds:"
time (ls_ plugin grant bigbox mcp-server 2>&1 | head -1 | sed 's/^/    /')
echo "  plugin list (one scan), seconds:"
time (ls_ plugin list > /dev/null 2>&1)
big=$(status bigbox)
echo "  plugin list: $big"
case $big in *"file over 10 MB: data/blob.bin"*) pass "first-F1 size limit at consent";; *) fail "first-F1 size limit at consent";; esac

echo "== first-F1: a git checkout plugin with a 200 MB pack"
mkdir -p "$S/origin/gitbig"
manifest "$S/origin/gitbig"
script "$S/origin/gitbig/bin/server" box
git -C "$S/origin/gitbig" init -q -b main && git -C "$S/origin/gitbig" add -A && git -C "$S/origin/gitbig" commit -qm v1
git clone -q "$S/origin/gitbig" "$D/plugins/gitbig"
dd if=/dev/urandom of="$D/plugins/gitbig/.git/objects/pack/big.pack" bs=1048576 count=200 2> /dev/null
ls_ plugin grant gitbig mcp-server > /dev/null 2>&1
echo "  plugin list (one scan), seconds:"
time (ls_ plugin list > /dev/null 2>&1)
gitbig=$(status gitbig)
echo "  plugin list: $gitbig"
case $gitbig in *"file over 10 MB: .git/objects/pack/big.pack"*) pass "first-F1 size limit covers .git";; *) fail "first-F1 size limit covers .git";; esac

checkout() {
  mkdir -p "$S/origin/$1"
  manifest "$S/origin/$1" "${2:-bin/server}"
  script "$S/origin/$1/bin/server" box
  git -C "$S/origin/$1" init -q -b main && git -C "$S/origin/$1" add -A && git -C "$S/origin/$1" commit -qm v1
}

upstream() {
  echo "$2" >> "$S/origin/$1/README"
  git -C "$S/origin/$1" add -A && git -C "$S/origin/$1" commit -qm "$2"
}

ended() {
  local id=$1 label=$2 now
  sync_
  now=$(status "$id")
  echo "  plugin list: $now, plugin-$id in mcpServers: $(entry "$A" "plugin-$id")"
  case $now in "needs consent"*) [ "$(entry "$A" "plugin-$id")" = "no" ] && { pass "$label"; return; } ;; esac
  fail "$label"
}

echo "== first-F2: a git command in a checkout plugin ends consent cleanly"
fresh f2git
checkout gitbox
P=$D/plugins/gitbox
git clone -q "$S/origin/gitbox" "$P"
echo "  grant: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
upstream gitbox v2
git -C "$P" fetch -q
ended gitbox "first-F2 git fetch ends consent"
echo "  grant again: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
touch "$P/plugin.toml"
git -C "$P" status > /dev/null
echo "  after git status: $(status gitbox)"
echo "  grant again: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
git -C "$P" config core.pager "sh -c 'echo swapped'"
edited gitbox "first-F2 .git/config stays covered"

echo "== first-F2: a manifest command inside .git"
fresh f2cmd
P=$D/plugins/gitcmd
manifest "$P" .git/server
mkdir -p "$P/.git"
script "$P/.git/server" reviewed
echo "  grant: $(ls_ plugin grant gitcmd mcp-server 2>&1 | head -1)"
script "$P/.git/server" swapped
edited gitcmd "first-F2 command inside .git stays covered"

echo "== first-F2: a command symlinked into .git/objects"
fresh f2obj
mkdir -p "$S/origin/gitobj"
manifest "$S/origin/gitobj"
git -C "$S/origin/gitobj" init -q -b main && git -C "$S/origin/gitobj" add -A && git -C "$S/origin/gitobj" commit -qm v1
P=$D/plugins/gitobj
git clone -q "$S/origin/gitobj" "$P"
script "$P/.git/objects/payload" reviewed
mkdir -p "$P/bin"
ln -s ../.git/objects/payload "$P/bin/server"
echo "  grant: $(ls_ plugin grant gitobj mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited gitobj "first-F2 symlink into .git/objects stays covered"

echo "== first-F2: plugin install <dir> from a checkout keeps consent through git commands"
fresh f2dev
checkout devbox
git clone -q "$S/origin/devbox" "$S/work"
echo "  install: $(ls_ plugin install "$S/work" 2>&1 | head -1)"
echo "  grant: $(ls_ plugin grant devbox mcp-server 2>&1 | head -1)"
upstream devbox v2
git -C "$S/work" fetch -q
touch "$S/work/plugin.toml"
git -C "$S/work" status > /dev/null
sync_
dev=$(status devbox)
echo "  after git fetch and git status in the checkout: $dev, plugin-devbox in mcpServers: $(entry "$A" plugin-devbox)"
if [ "$dev" = "ready" ] && [ "$(entry "$A" plugin-devbox)" = "yes" ]; then pass "first-F2 installed copy keeps consent"; else fail "first-F2 installed copy keeps consent"; fi

echo "== first-F2: files that vanish while consent reads the folder"
fresh f2race
checkout racebox
P=$D/plugins/racebox
git clone -q "$S/origin/racebox" "$P"
echo "  grant: $(ls_ plugin grant racebox mcp-server 2>&1 | head -1)"
python3 -I -c '
import os, sys
d = sys.argv[1]
while True:
    for i in range(200):
        open(os.path.join(d, "churn%d.lock" % i), "w").close()
    for i in range(200):
        os.unlink(os.path.join(d, "churn%d.lock" % i))
' "$P/.git" &
churn=$!
seen=""
for _ in $(seq 40); do
  now=$(status racebox)
  case $now in "needs consent"* | ready) ;; *) seen="$now"; break ;; esac
done
kill "$churn"
wait "$churn" 2> /dev/null
echo "  40 scans while git-like lock files come and go: ${seen:-every one ready or needs consent}"
if [ -z "$seen" ]; then pass "first-F2 vanished file is no error"; else fail "first-F2 vanished file is no error"; fi

echo "== first-F1: lib -> .git with command lib/objects/payload"
fresh hf1
checkout libgit lib/objects/payload
P=$D/plugins/libgit
git clone -q "$S/origin/libgit" "$P"
ln -s .git "$P/lib"
script "$P/.git/objects/payload" reviewed
echo "  grant: $(ls_ plugin grant libgit mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited libgit "first-F1 lib -> .git payload stays covered"

echo "== sol-F1: bin/repo -> ../.git with command bin/repo/objects/server"
fresh hsol1
checkout repobox bin/repo/objects/server
P=$D/plugins/repobox
git clone -q "$S/origin/repobox" "$P"
ln -s ../.git "$P/bin/repo"
script "$P/.git/objects/server" reviewed
echo "  grant: $(ls_ plugin grant repobox mcp-server 2>&1 | head -1)"
script "$P/.git/objects/server" swapped
edited repobox "sol-F1 bin/repo -> ../.git server stays covered"

echo "== second-S1: command .Git/objects/payload on a case-insensitive disk"
fresh hs1
checkout casebox .Git/objects/payload
P=$D/plugins/casebox
git clone -q "$S/origin/casebox" "$P"
script "$P/.git/objects/payload" reviewed
[ -f "$P/.Git/objects/payload" ] && echo "  this disk is case-insensitive" || echo "  this disk is case-sensitive, so .Git does not reach .git"
echo "  grant: $(ls_ plugin grant casebox mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited casebox "second-S1 .Git payload stays covered"

echo "== second-S1: command .ds_store and a symlink to .DS_STORE"
fresh hs1ds
P=$D/plugins/lowerds
manifest "$P" .ds_store
script "$P/.DS_Store" reviewed
echo "  grant: $(ls_ plugin grant lowerds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited lowerds "second-S1 command .ds_store refused"
P=$D/plugins/upperds
manifest "$P"
script "$P/.DS_Store" reviewed
ln -s ../.DS_STORE "$P/bin/server"
echo "  grant: $(ls_ plugin grant upperds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited upperds "second-S1 symlink to .DS_STORE refused"
P=$D/plugins/foldds
manifest "$P" $'.DS_\xc5\xbftore'
script "$P/.DS_Store" reviewed
echo "  grant (command .DS_ with a long s, which APFS folds to .DS_Store): $(ls_ plugin grant foldds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited foldds "second-S1 command .DS_Store spelled with a long s refused"

echo "== second-S3: a socket and a FIFO in the plugin folder"
fresh s3
P=$D/plugins/sockbox
manifest "$P"
script "$P/bin/server" box
python3 -I -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$P/run.sock"
mkfifo "$P/run.fifo"
echo "  grant: $(ls_ plugin grant sockbox mcp-server 2>&1 | head -1)"
sync_
sock=$(status sockbox)
echo "  plugin list: $sock, plugin-sockbox in mcpServers: $(entry "$A" plugin-sockbox)"
if [ "$sock" = "ready" ] && [ "$(entry "$A" plugin-sockbox)" = "yes" ]; then pass "second-S3 socket and FIFO keep consent"; else fail "second-S3 socket and FIFO keep consent"; fi
for kind in sock fifo; do
  P=$D/plugins/run$kind
  manifest "$P" "run.$kind"
  if [ $kind = sock ]; then python3 -I -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$P/run.sock"; else mkfifo "$P/run.fifo"; fi
  ls_ plugin grant "run$kind" mcp-server > /dev/null 2>&1
  sync_
  named=$(status "run$kind")
  echo "  command run.$kind: $named, plugin-run$kind in mcpServers: $(entry "$A" "plugin-run$kind")"
  if [ "$named" != "ready" ] && [ "$(entry "$A" "plugin-run$kind")" = "no" ]; then pass "second-S3 command run.$kind gets no entry"; else fail "second-S3 command run.$kind gets no entry"; fi
done

echo "== second-F9: a tagged entry copied under a name without the plugin- prefix"
fresh f9
settings '{"toolbox-debug": {"type": "stdio", "command": "/usr/local/bin/toolbox", "localStudioPlugin": true}}'
sync_
echo "  after a sync, toolbox-debug in mcpServers: $(entry "$A" toolbox-debug)"
if [ "$(entry "$A" toolbox-debug)" = "yes" ]; then pass "second-F9 copied tagged entry kept"; else fail "second-F9 copied tagged entry kept"; fi

echo "== first-F5: a grant with a relative data dir records an absolute folder"
fresh f5
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
(cd "$S" && HOME=$S/h LOCAL_STUDIO_DATA_DIR=d EUKHE_CODING_AGENT_DIR=$A "$BIN" plugin grant toolbox mcp-server > /dev/null 2>&1)
source=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["plugins"]["toolbox"]["source"])' "$D/plugins/grants.json")
echo "  grants.json source: $source"
case $source in "folder /"*) pass "first-F5 absolute folder source";; *) fail "first-F5 absolute folder source";; esac

echo "== first-F6: plugin list names a grant of unknown origin"
fresh f6
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
echo '{"plugins": {"toolbox": {"version": "1.0.0", "capabilities": ["mcp-server"]}}}' > "$D/plugins/grants.json"
ls_ plugin grant toolbox mcp-server > /dev/null 2>&1
line=$(ls_ plugin list 2>&1 | grep 'granted mcp-server')
echo "  plugin list: '$line'"
case $line in *"from ") fail "first-F6 empty source named";; *"from "?*) pass "first-F6 empty source named";; *) fail "first-F6 empty source named";; esac

echo "failures: $failures"
exit "$failures"
