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
from pathlib import Path

CONFIG_DIR = Path(os.path.expanduser("~/.config/mcmove"))
CONFIG_FILE = CONFIG_DIR / "servers.json"
BACKUP_DIR = CONFIG_DIR / "backups"

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
        print(f"  {name:16} {s['username']}@{s['host']}:{s['port']}  ({auth})")


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

    src = clean_path(args.src or ask("Path to your local Modrinth/Minecraft instance"))
    if not os.path.isdir(src):
        die(f"not a folder: {src}")

    actions = pick_many(
        "What do you want to move?",
        [("Mods (mods/*.jar)", "mods"),
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

    clear_mods = "mods" in actions and confirm("Clear existing remote /mods first?", default=False)
    clear_world = "world" in actions and confirm(
        f"Clear existing remote /{level_name} first?", default=False)
    do_backup = confirm("Back up the server's current files before overwriting?", default=True)

    print("\nPlan:")
    print(f"  server : {server_name}  ({profile['username']}@{profile['host']}:{profile['port']})")
    print(f"  source : {src}")
    for a in actions:
        if a == "world":
            print(f"  world  : {Path(world_src).name}  ->  /{level_name}"
                  + ("  (clearing target)" if clear_world else ""))
        elif a == "mods":
            print("  mods   : mods/*.jar  ->  /mods" + ("  (clearing target)" if clear_mods else ""))
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
            if "mods" in actions:
                targets.append("/mods")
            if "world" in actions:
                targets.append("/" + level_name)
            if "config" in actions:
                targets.append("/config")
            print("\nBackup:")
            backup_remote(sftp, server_name, targets)

        print("\nMoving:")
        if "mods" in actions:
            move_mods(sftp, src, clear_mods)
        if "config" in actions:
            move_config(sftp, src)
        if "world" in actions:
            move_world(sftp, world_src, level_name, clear_world)
    finally:
        sftp.close()
        transport.close()

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

    args = p.parse_args()
    if args.cmd == "list":
        cmd_list(args)
    elif args.cmd == "add-server":
        cmd_add_server(args)
    elif args.cmd == "remove-server":
        cmd_remove_server(args)
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
