#!/usr/bin/env python3
"""
mcmove — move worlds / mods / configs from a local Modrinth (or any) Minecraft
instance into a Pelican / Pterodactyl game server over SFTP.

Why SFTP: it pushes from your machine to the server, works remotely, and files
land owned by the server user automatically — no root, no chown footguns.

Quick start:
    pip install paramiko
    python3 mcmove.py add-server          # paste the panel's "SFTP Details" once
    python3 mcmove.py                      # interactive wizard
    python3 mcmove.py list                 # list saved servers

Config + backups live in ~/.config/mcmove/.
"""
import argparse
import getpass
import hashlib
import json
import os
import posixpath
import shutil
import stat
import sys
import re
import tarfile
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

try:
    import tomllib  # py3.11+
except ImportError:
    tomllib = None

CONFIG_DIR = Path(os.path.expanduser("~/.config/mcmove"))
CONFIG_FILE = CONFIG_DIR / "servers.json"
BACKUP_DIR = CONFIG_DIR / "backups"
STATE_DIR = CONFIG_DIR / "state"
MODRINTH_API = "https://api.modrinth.com/v2"
USER_AGENT = "mcmove/0.2 (github.com/zeriaxdev/mcmove)"

try:
    import paramiko
except ImportError:
    paramiko = None


# ----------------------------------------------------------------------------- ui helpers
def die(msg, code=1):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


def clean_path(p):
    """Normalize a pasted path: strip quotes and shell backslash-escapes, expand ~.

    Lets users drag-drop or tab-complete a path (e.g. 'Application\\ Support' or
    a quoted '~/My Folder') and have it just work.
    """
    p = p.strip()
    if len(p) >= 2 and p[0] == p[-1] and p[0] in ("'", '"'):
        p = p[1:-1]
    else:
        p = re.sub(r"\\(.)", r"\1", p)  # \X -> X  (unescape spaces, parens, etc.)
    return os.path.expanduser(p)


def need_paramiko():
    if paramiko is None:
        die("paramiko is required. Install it with:\n    pip install paramiko")


def ask(prompt, default=None):
    suffix = f" [{default}]" if default not in (None, "") else ""
    val = input(f"{prompt}{suffix}: ").strip()
    return val or (default or "")


def confirm(prompt, default=True):
    d = "Y/n" if default else "y/N"
    val = input(f"{prompt} ({d}): ").strip().lower()
    if not val:
        return default
    return val in ("y", "yes")


def pick_one(prompt, options):
    """options: list of (label, value). Returns value or None."""
    if not options:
        return None
    print(prompt)
    for i, (label, _) in enumerate(options, 1):
        print(f"  {i}) {label}")
    while True:
        raw = input("select #: ").strip()
        if not raw:
            return None
        if raw.isdigit() and 1 <= int(raw) <= len(options):
            return options[int(raw) - 1][1]
        print("  invalid choice")


def pick_many(prompt, options):
    """options: list of (label, value). Returns list of selected values."""
    print(prompt + "  (comma-separated numbers, e.g. 1,3)")
    for i, (label, _) in enumerate(options, 1):
        print(f"  {i}) {label}")
    raw = input("select: ").strip()
    chosen = []
    for tok in raw.replace(" ", "").split(","):
        if tok.isdigit() and 1 <= int(tok) <= len(options):
            chosen.append(options[int(tok) - 1][1])
    return chosen


# ----------------------------------------------------------------------------- config
def load_config():
    if CONFIG_FILE.exists():
        return json.loads(CONFIG_FILE.read_text())
    return {"servers": {}}


def save_config(cfg):
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_FILE.write_text(json.dumps(cfg, indent=2))


def parse_sftp_url(url):
    """Parse 'sftp://admin.100b3b70@node1.example.com:2022' -> (host, port, username)."""
    url = url.strip()
    if "://" not in url:
        url = "sftp://" + url
    u = urllib.parse.urlparse(url)
    if not u.hostname:
        return None
    return u.hostname, (u.port or 2022), (urllib.parse.unquote(u.username) if u.username else "")


def cmd_add_server(args):
    cfg = load_config()
    print("Add a server. Grab these from the panel: your server → Settings → SFTP Details.\n")

    # Fast path: paste the whole sftp://user@host:port string the panel gives you.
    url = getattr(args, "url", None) or ask(
        "Paste SFTP URL (sftp://user@host:port), or leave blank to type fields manually", "")
    host = port = username = ""
    if url:
        parsed = parse_sftp_url(url)
        if not parsed:
            die(f"could not parse SFTP URL: {url}")
        host, port, username = parsed
        port = str(port)
        print(f"  parsed → host={host} port={port} username={username}")

    name = ask("Profile name (e.g. survival)", username.split(".")[-1] if username else "")
    if not name:
        die("name required")
    host = host or ask("SFTP host")
    port = port or ask("SFTP port", "2022")
    username = username or ask("SFTP username (looks like admin.ab12cd34)")
    key_path = ask("Path to SSH private key (blank = use password each run)", "")
    cfg["servers"][name] = {
        "host": host,
        "port": int(port or 2022),
        "username": username,
        "key_path": clean_path(key_path) if key_path else "",
    }
    save_config(cfg)
    print(f"\nSaved '{name}'. Password is never stored — you'll be prompted at connect time"
          if not key_path else f"\nSaved '{name}'.")


def cmd_remove_server(args):
    cfg = load_config()
    if args.name not in cfg["servers"]:
        die(f"no such server: {args.name}")
    del cfg["servers"][args.name]
    save_config(cfg)
    print(f"removed {args.name}")


def cmd_list(args):
    cfg = load_config()
    if not cfg["servers"]:
        print("No servers configured. Add one with:  python3 mcmove.py add-server")
        return
    print("Configured servers:")
    for name, s in cfg["servers"].items():
        auth = "key" if s.get("key_path") else "password"
        src = s.get("last_src", "")
        print(f"  {name:16} {s['username']}@{s['host']}:{s['port']}  ({auth})"
              + (f"  src={src}" if src else ""))


def _select_server(cfg, name):
    if not cfg["servers"]:
        die("no servers configured; add one with:  python3 mcmove.py add-server")
    if not name:
        name = pick_one(
            "Target server:",
            [(f"{n}  ({s['username']}@{s['host']})", n) for n, s in cfg["servers"].items()],
        )
    if not name or name not in cfg["servers"]:
        die("no server selected")
    return name, cfg["servers"][name]


def cmd_sync(args):
    """Patch only the mods on a server to match a local instance."""
    cfg = load_config()
    name, profile = _select_server(cfg, args.server)
    src = clean_path(args.src or ask(
        "Path to your local Modrinth/Minecraft instance", profile.get("last_src", "")))
    if not os.path.isdir(src):
        die(f"not a folder: {src}")
    sync_mods(profile, name, src, args.dry_run)
    if not args.dry_run:
        profile["last_src"] = src
        cfg["servers"][name] = profile
        save_config(cfg)


# ----------------------------------------------------------------------------- sftp core
def connect(profile):
    need_paramiko()
    transport = paramiko.Transport((profile["host"], int(profile.get("port", 2022))))
    try:
        if profile.get("key_path"):
            pkey = load_key(profile["key_path"])
            transport.connect(username=profile["username"], pkey=pkey)
        else:
            pw = getpass.getpass(f"Panel password for {profile['username']}: ")
            transport.connect(username=profile["username"], password=pw)
    except paramiko.AuthenticationException:
        transport.close()
        die("authentication failed — check username/password (it's your PANEL password).")
    except Exception as e:  # noqa
        transport.close()
        die(f"could not connect: {e}")
    return transport, paramiko.SFTPClient.from_transport(transport)


def load_key(path):
    for cls in (paramiko.Ed25519Key, paramiko.ECDSAKey, paramiko.RSAKey):
        try:
            return cls.from_private_key_file(path)
        except Exception:  # noqa
            continue
    die(f"could not load private key: {path}")


def remote_exists(sftp, path):
    try:
        sftp.stat(path)
        return True
    except IOError:
        return False


def sftp_mkdirs(sftp, remote):
    parts = [p for p in remote.strip("/").split("/") if p]
    cur = ""
    for p in parts:
        cur += "/" + p
        if not remote_exists(sftp, cur):
            try:
                sftp.mkdir(cur)
            except IOError:
                pass


def sftp_walk(sftp, remote):
    dirs, files = [], []
    for entry in sftp.listdir_attr(remote):
        rp = posixpath.join(remote, entry.filename)
        (dirs if stat.S_ISDIR(entry.st_mode) else files).append(rp)
    yield remote, dirs, files
    for d in dirs:
        yield from sftp_walk(sftp, d)


def rm_rf(sftp, remote):
    if not remote_exists(sftp, remote):
        return
    for entry in sftp.listdir_attr(remote):
        rp = posixpath.join(remote, entry.filename)
        if stat.S_ISDIR(entry.st_mode):
            rm_rf(sftp, rp)
            sftp.rmdir(rp)
        else:
            sftp.remove(rp)


def upload_dir(sftp, local, remote):
    local = Path(local)
    files = [p for p in local.rglob("*") if p.is_file()]
    sftp_mkdirs(sftp, remote)
    total = len(files)
    for i, p in enumerate(files, 1):
        rel = p.relative_to(local).as_posix()
        rp = posixpath.join(remote, rel)
        d = posixpath.dirname(rp)
        if d:
            sftp_mkdirs(sftp, d)
        sftp.put(str(p), rp)
        print(f"\r  ↑ {remote}: {i}/{total} files", end="", flush=True)
    print()


def upload_files(sftp, local_files, remote):
    sftp_mkdirs(sftp, remote)
    total = len(local_files)
    for i, p in enumerate(local_files, 1):
        sftp.put(str(p), posixpath.join(remote, Path(p).name))
        print(f"\r  ↑ {remote}: {i}/{total} files", end="", flush=True)
    print()


def download_dir(sftp, remote, local):
    local = Path(local)
    for root, _dirs, files in sftp_walk(sftp, remote):
        rel = posixpath.relpath(root, remote)
        ldir = local if rel == "." else local / rel
        ldir.mkdir(parents=True, exist_ok=True)
        for f in files:
            sftp.get(f, str(ldir / posixpath.basename(f)))


# ----------------------------------------------------------------------------- actions
def backup_remote(sftp, server_name, targets):
    present = [t for t in targets if remote_exists(sftp, t)]
    if not present:
        print("  (nothing to back up yet)")
        return None
    ts = time.strftime("%Y%m%d-%H%M%S")
    staging = BACKUP_DIR / f"{server_name}-{ts}"
    for t in present:
        print(f"  ⇣ backing up {t} ...")
        download_dir(sftp, t, staging / t.strip("/"))
    tar_path = str(staging) + ".tar.gz"
    with tarfile.open(tar_path, "w:gz") as tar:
        tar.add(staging, arcname=staging.name)
    shutil.rmtree(staging)
    print(f"  ✓ backup saved: {tar_path}")
    return tar_path


def set_level_name(sftp, name):
    path = "/server.properties"
    content = ""
    if remote_exists(sftp, path):
        with sftp.open(path, "r") as f:
            content = f.read().decode("utf-8", "replace")
    out, found = [], False
    for ln in content.splitlines():
        if ln.startswith("level-name="):
            out.append(f"level-name={name}")
            found = True
        else:
            out.append(ln)
    if not found:
        out.append(f"level-name={name}")
    with sftp.open(path, "w") as f:
        f.write(("\n".join(out) + "\n").encode())
    print(f"  ✓ set level-name={name} in server.properties")


def move_mods(sftp, src_instance, clear):
    mods_dir = Path(src_instance) / "mods"
    if not mods_dir.is_dir():
        print(f"  ! no mods/ folder in {src_instance}, skipping")
        return
    jars = sorted([p for p in mods_dir.glob("*.jar")])
    if not jars:
        print("  ! no .jar files in mods/, skipping")
        return
    if clear:
        print("  ✗ clearing remote /mods ...")
        rm_rf(sftp, "/mods")
    print(f"  moving {len(jars)} mods ...")
    upload_files(sftp, jars, "/mods")


def move_config(sftp, src_instance):
    cfg_dir = Path(src_instance) / "config"
    if not cfg_dir.is_dir():
        print(f"  ! no config/ folder in {src_instance}, skipping")
        return
    print("  moving config/ ...")
    upload_dir(sftp, cfg_dir, "/config")


# ----------------------------------------------------------------------------- mod sync (patcher)
def sha1_of(path):
    h = hashlib.sha1()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _http_json(url, payload=None):
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    data = None
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers,
                                 method="POST" if payload is not None else "GET")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())


def modrinth_sides(hashes):
    """sha1 -> (project_id, server_side) via Modrinth. Best-effort; {} on failure."""
    if not hashes:
        return {}
    try:
        vf = _http_json(f"{MODRINTH_API}/version_files",
                        {"hashes": hashes, "algorithm": "sha1"})
    except Exception:  # noqa  (offline / rate-limited -> fall back to jar metadata)
        return {}
    proj_by_hash, pids = {}, set()
    for h, ver in vf.items():
        pid = ver.get("project_id")
        if pid:
            proj_by_hash[h] = pid
            pids.add(pid)
    side_by_pid = {}
    if pids:
        try:
            ids = urllib.parse.quote(json.dumps(sorted(pids)))
            for p in _http_json(f"{MODRINTH_API}/projects?ids={ids}"):
                side_by_pid[p["id"]] = p.get("server_side")
        except Exception:  # noqa
            pass
    return {h: (pid, side_by_pid.get(pid)) for h, pid in proj_by_hash.items()}


def read_jar_meta(path):
    """Offline fallback: (modid, env) from a jar. env in {client, server, both, None}."""
    try:
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())
            if "fabric.mod.json" in names:
                data = json.loads(z.read("fabric.mod.json").decode("utf-8", "replace"))
                env = {"client": "client", "server": "server"}.get(data.get("environment"), "both")
                return data.get("id"), env
            for tn in ("META-INF/neoforge.mods.toml", "META-INF/mods.toml"):
                if tn in names and tomllib:
                    try:
                        t = tomllib.loads(z.read(tn).decode("utf-8", "replace"))
                        mods = t.get("mods", [])
                        if mods:
                            return mods[0].get("modId"), None  # forge: no reliable side
                    except Exception:  # noqa
                        pass
    except Exception:  # noqa
        pass
    return None, None


def classify_mods(paths):
    """[{path, filename, sha1, key, side}] with side in {keep, client, unknown}."""
    infos = [{"path": str(p), "filename": Path(p).name, "sha1": sha1_of(p)} for p in paths]
    sides = modrinth_sides([i["sha1"] for i in infos])
    for i in infos:
        hit = sides.get(i["sha1"])
        if hit:
            pid, server_side = hit
            i["key"] = "modrinth:" + pid
            i["side"] = "client" if server_side == "unsupported" else "keep"
            continue
        modid, env = read_jar_meta(i["path"])
        i["key"] = ("mod:" + modid) if modid else ("file:" + i["filename"])
        i["side"] = {"client": "client", "server": "keep", "both": "keep"}.get(env, "unknown")
    return infos


def manifest_path(server):
    return STATE_DIR / f"{server}.json"


def load_manifest(server):
    p = manifest_path(server)
    return json.loads(p.read_text()) if p.exists() else {"mods": {}}


def save_manifest(server, man):
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    manifest_path(server).write_text(json.dumps(man, indent=2))


def plan_mod_sync(infos, manifest, remote_files):
    """Diff local mods vs what we manage on the server. Returns (plan, new_managed)."""
    managed = manifest.get("mods", {})
    plan = {"add": [], "update": [], "remove": [], "keep": [], "client": [], "unknown": []}
    new_managed, seen = {}, set()
    for i in infos:
        key = i["key"]
        seen.add(key)
        if i["side"] == "client":
            for fn in {managed.get(key, {}).get("filename"), i["filename"]} - {None}:
                if fn in remote_files:
                    plan["remove"].append(fn)
            plan["client"].append(i["filename"])
            continue
        if i["side"] == "unknown":
            plan["unknown"].append(i["filename"])
        prev = managed.get(key)
        if prev:
            if prev["sha1"] == i["sha1"]:
                (plan["keep"] if i["filename"] in remote_files else plan["add"]).append(
                    i["filename"] if i["filename"] in remote_files else i)
            else:
                if prev["filename"] in remote_files and prev["filename"] != i["filename"]:
                    plan["remove"].append(prev["filename"])
                plan["update"].append(i)
        else:
            (plan["keep"] if i["filename"] in remote_files else plan["add"]).append(
                i["filename"] if i["filename"] in remote_files else i)
        new_managed[key] = {"filename": i["filename"], "sha1": i["sha1"], "side": i["side"]}
    # mods that left the pack: drop the ones we previously managed
    for key, m in managed.items():
        if key not in seen and m["filename"] in remote_files:
            plan["remove"].append(m["filename"])
    return plan, new_managed


def do_mod_sync(sftp, server_name, src, dry_run):
    """Patch the server's /mods to match the local instance, over an existing sftp."""
    mods_dir = Path(src) / "mods"
    if not mods_dir.is_dir():
        print(f"  ! no mods/ folder in {src}, skipping mods")
        return
    paths = sorted(mods_dir.glob("*.jar"))
    if not paths:
        print("  ! no .jar files in mods/, skipping mods")
        return

    print(f"Scanning {len(paths)} local mods (resolving client/server via Modrinth)...")
    infos = classify_mods(paths)
    n_client = sum(1 for i in infos if i["side"] == "client")
    n_unknown = sum(1 for i in infos if i["side"] == "unknown")
    print(f"  {len(infos) - n_client} server-side · {n_client} client-only (skipped)"
          + (f" · {n_unknown} undetermined (kept)" if n_unknown else ""))

    manifest = load_manifest(server_name)
    if True:
        remote_files = set(sftp.listdir("/mods")) if remote_exists(sftp, "/mods") else set()
        plan, new_managed = plan_mod_sync(infos, manifest, remote_files)

        print("\nPlan:")
        print(f"  add {len(plan['add'])} · update {len(plan['update'])} · "
              f"remove {len(plan['remove'])} · unchanged {len(plan['keep'])} · "
              f"client skipped {len(plan['client'])}")
        for i in plan["add"]:
            print(f"  + add     {i['filename']}")
        for i in plan["update"]:
            print(f"  ~ update  {i['filename']}")
        for fn in dict.fromkeys(plan["remove"]):
            print(f"  - remove  {fn}")
        if plan["unknown"]:
            print(f"  ? kept (couldn't determine side): {', '.join(plan['unknown'][:8])}"
                  + (" ..." if len(plan['unknown']) > 8 else ""))

        if not (plan["add"] or plan["update"] or plan["remove"]):
            print("\nServer mods already up to date. Nothing to do.")
            return
        if dry_run:
            print("\n(dry run — no changes made)")
            return
        if not confirm("\nApply this patch?", default=True):
            print("aborted")
            return

        sftp_mkdirs(sftp, "/mods")
        for fn in dict.fromkeys(plan["remove"]):
            try:
                sftp.remove("/mods/" + fn)
            except IOError:
                pass
            print(f"  - {fn}")
        for i in plan["add"] + plan["update"]:
            sftp.put(i["path"], "/mods/" + i["filename"])
            print(f"  ↑ {i['filename']}")

        manifest["mods"] = new_managed
        save_manifest(server_name, manifest)
    print("\n✓ Mods patched. Restart the server to load changes.")


def sync_mods(profile, server_name, src, dry_run):
    """Standalone mod patch: open a connection and run do_mod_sync."""
    transport, sftp = connect(profile)
    try:
        do_mod_sync(sftp, server_name, src, dry_run)
    finally:
        sftp.close()
        transport.close()


def move_world(sftp, world_src, level_name, clear):
    if clear:
        print(f"  ✗ clearing remote /{level_name} ...")
        rm_rf(sftp, "/" + level_name)
    print(f"  moving world '{Path(world_src).name}' -> /{level_name} ...")
    upload_dir(sftp, world_src, "/" + level_name)
    set_level_name(sftp, level_name)


# ----------------------------------------------------------------------------- wizard
def run_wizard(args):
    cfg = load_config()
    if not cfg["servers"]:
        print("No servers yet. Let's add one.\n")
        cmd_add_server(args)
        cfg = load_config()

    server_name = pick_one(
        "Target server:",
        [(f"{n}  ({s['username']}@{s['host']})", n) for n, s in cfg["servers"].items()],
    )
    if not server_name:
        die("no server selected")
    profile = cfg["servers"][server_name]

    remembered = profile.get("last_src", "")
    src = clean_path(args.src or ask("Path to your local Modrinth/Minecraft instance", remembered))
    if not os.path.isdir(src):
        die(f"not a folder: {src}")

    actions = pick_many(
        "What do you want to move?",
        [("Mods (patch — add/update/remove, skips client-only)", "mods"),
         ("World (from saves/)", "world"),
         ("Config (config/)", "config")],
    )
    if not actions:
        die("nothing selected")

    world_src = level_name = None
    if "world" in actions:
        saves = Path(src) / "saves"
        worlds = sorted([p for p in saves.iterdir() if p.is_dir()]) if saves.is_dir() else []
        if not worlds:
            die(f"no worlds found in {saves}")
        world_src = pick_one("Which world?", [(p.name, str(p)) for p in worlds])
        if not world_src:
            die("no world selected")
        default_name = "world"
        level_name = ask("Target level-name on the server", default_name)

    clear_world = "world" in actions and confirm(
        f"Clear existing remote /{level_name} first?", default=False)
    # World/config overwrite, so offer a backup. Mods are patched (non-destructive
    # to unmanaged files), so they're excluded from the backup.
    backup_actions = [a for a in actions if a in ("world", "config")]
    do_backup = bool(backup_actions) and confirm(
        "Back up the server's current world/config before overwriting?", default=True)

    print("\nPlan:")
    print(f"  server : {server_name}  ({profile['username']}@{profile['host']}:{profile['port']})")
    print(f"  source : {src}")
    for a in actions:
        if a == "world":
            print(f"  world  : {Path(world_src).name}  ->  /{level_name}"
                  + ("  (clearing target)" if clear_world else ""))
        elif a == "mods":
            print("  mods   : patch /mods (add/update/remove · skip client-only)")
        elif a == "config":
            print("  config : config/  ->  /config")
    print(f"  backup : {'yes' if do_backup else 'no'}")
    if not confirm("\nProceed?", default=True):
        print("aborted")
        return

    transport, sftp = connect(profile)
    try:
        if do_backup:
            targets = []
            if "world" in actions:
                targets.append("/" + level_name)
            if "config" in actions:
                targets.append("/config")
            print("\nBackup:")
            backup_remote(sftp, server_name, targets)

        if "mods" in actions:
            print("\nMods (patch):")
            do_mod_sync(sftp, server_name, src, dry_run=False)
        if "config" in actions:
            print("\nConfig:")
            move_config(sftp, src)
        if "world" in actions:
            print("\nWorld:")
            move_world(sftp, world_src, level_name, clear_world)
    finally:
        sftp.close()
        transport.close()

    # Remember the source path for next time.
    profile["last_src"] = src
    cfg["servers"][server_name] = profile
    save_config(cfg)

    print("\n✓ Done. Restart the server in the panel to load the changes.")
    if "world" in actions:
        print("  Note: if this was a single-player world on vanilla, dimensions are nested")
        print("  inside the world folder — that's fine for modded/Forge/NeoForge servers.")


# ----------------------------------------------------------------------------- main
def main():
    p = argparse.ArgumentParser(description="Move worlds/mods/configs into a Pelican server over SFTP.")
    sub = p.add_subparsers(dest="cmd")

    sub.add_parser("list", help="list configured servers")
    add = sub.add_parser("add-server", help="add a server profile (from panel SFTP Details)")
    add.add_argument("--url", help="paste the panel's sftp://user@host:port string")
    rm = sub.add_parser("remove-server", help="remove a server profile")
    rm.add_argument("name")

    wiz = sub.add_parser("move", help="run the move wizard (default)")
    wiz.add_argument("--src", help="path to local instance (skips the prompt)")

    syn = sub.add_parser("sync", help="patch a server's mods to match a local instance")
    syn.add_argument("--server", help="saved server name (otherwise you'll be asked)")
    syn.add_argument("--src", help="path to local instance (otherwise remembered/asked)")
    syn.add_argument("--dry-run", action="store_true", help="show the plan, change nothing")

    args = p.parse_args()
    if args.cmd == "list":
        cmd_list(args)
    elif args.cmd == "add-server":
        cmd_add_server(args)
    elif args.cmd == "remove-server":
        cmd_remove_server(args)
    elif args.cmd == "sync":
        cmd_sync(args)
    else:  # move / default
        if not hasattr(args, "src"):
            args.src = None
        run_wizard(args)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\naborted")
        sys.exit(130)
