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
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

try:
    import tomllib  # py3.11+
except ImportError:
    tomllib = None

try:
    import nbtlib  # for the playerdata command
except ImportError:
    nbtlib = None

CONFIG_DIR = Path(os.path.expanduser("~/.config/mcmove"))
CONFIG_FILE = CONFIG_DIR / "servers.json"
BACKUP_DIR = CONFIG_DIR / "backups"
STATE_DIR = CONFIG_DIR / "state"
MODRINTH_API = "https://api.modrinth.com/v2"
__version__ = "0.5.0"
USER_AGENT = f"mcmove/{__version__} (github.com/zeriaxdev/mcmove)"

# ----------------------------------------------------------------------------- color
_COLOR = (
    sys.stdout.isatty()
    and os.environ.get("NO_COLOR") is None
    and os.environ.get("TERM") != "dumb"
)


def _c(code, s):
    return f"\033[{code}m{s}\033[0m" if _COLOR else s


def green(s):
    return _c("32", s)


def red(s):
    return _c("31", s)


def yellow(s):
    return _c("33", s)


def cyan(s):
    return _c("36", s)


def dim(s):
    return _c("2", s)


def bold(s):
    return _c("1", s)


try:
    import paramiko
except ImportError:
    paramiko = None


# ----------------------------------------------------------------------------- ui helpers
def die(msg, code=1):
    print(red(f"error: {msg}"), file=sys.stderr)
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
    return (
        u.hostname,
        (u.port or 2022),
        (urllib.parse.unquote(u.username) if u.username else ""),
    )


def cmd_add_server(args):
    cfg = load_config()
    print(
        "Add a server. Grab these from the panel: your server → Settings → SFTP Details.\n"
    )

    # Fast path: paste the whole sftp://user@host:port string the panel gives you.
    url = getattr(args, "url", None) or ask(
        "Paste SFTP URL (sftp://user@host:port), or leave blank to type fields manually",
        "",
    )
    host = port = username = ""
    if url:
        parsed = parse_sftp_url(url)
        if not parsed:
            die(f"could not parse SFTP URL: {url}")
        host, port, username = parsed
        port = str(port)
        print(f"  parsed → host={host} port={port} username={username}")

    name = ask(
        "Profile name (e.g. survival)", username.split(".")[-1] if username else ""
    )
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
    print(
        f"\nSaved '{name}'. Password is never stored — you'll be prompted at connect time"
        if not key_path
        else f"\nSaved '{name}'."
    )


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
        print(
            f"  {name:16} {s['username']}@{s['host']}:{s['port']}  ({auth})"
            + (f"  src={src}" if src else "")
        )


def _select_server(cfg, name):
    if not cfg["servers"]:
        die("no servers configured; add one with:  python3 mcmove.py add-server")
    if not name:
        name = pick_one(
            "Target server:",
            [
                (f"{n}  ({s['username']}@{s['host']})", n)
                for n, s in cfg["servers"].items()
            ],
        )
    if not name or name not in cfg["servers"]:
        die("no server selected")
    return name, cfg["servers"][name]


def cmd_sync(args):
    """Patch only the mods on a server to match a local instance."""
    cfg = load_config()
    name, profile = _select_server(cfg, args.server)
    src = clean_path(
        args.src
        or ask(
            "Path to your local Modrinth/Minecraft instance",
            profile.get("last_src", ""),
        )
    )
    if not os.path.isdir(src):
        die(f"not a folder: {src}")
    sync_mods(profile, name, src, args.dry_run)
    if not args.dry_run:
        profile["last_src"] = src
        cfg["servers"][name] = profile
        save_config(cfg)


def cmd_pull(args):
    """Download the server's mods and patch the LOCAL instance (reverse of sync)."""
    cfg = load_config()
    name, profile = _select_server(cfg, args.server)
    src = clean_path(
        args.src
        or ask(
            "Path to your local Modrinth/Minecraft instance",
            profile.get("last_src", ""),
        )
    )
    if not os.path.isdir(src):
        die(f"not a folder: {src}")
    pull_mods(profile, name, src, args.dry_run, args.mirror)
    if not args.dry_run:
        profile["last_src"] = src
        cfg["servers"][name] = profile
        save_config(cfg)


# ----------------------------------------------------------------------------- playerdata
_UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?"
    r"[0-9a-fA-F]{4}-?[0-9a-fA-F]{12}$"
)


def looks_like_uuid(s):
    return bool(_UUID_RE.match(s.strip()))


def hyphenate_uuid(u):
    u = u.replace("-", "").lower()
    return f"{u[0:8]}-{u[8:12]}-{u[12:16]}-{u[16:20]}-{u[20:32]}"


def resolve_uuids(names):
    """Bulk-resolve Minecraft usernames -> hyphenated UUIDs via Mojang (one request)."""
    out = {}
    names = list({n for n in names})
    for chunk in (
        names[i : i + 10] for i in range(0, len(names), 10)
    ):  # API caps at 10
        try:
            for e in _http_json(
                "https://api.mojang.com/profiles/minecraft", payload=chunk
            ):
                out[e["name"].lower()] = hyphenate_uuid(e["id"])
        except Exception:  # noqa
            pass
    return out


def cmd_update(args):
    """Check Modrinth for newer mod versions and update the local instance, per mod."""
    cfg = load_config()
    src = clean_path(args.src) if args.src else ""
    if not src:
        last = next((s["last_src"] for s in cfg.get("servers", {}).values()
                     if s.get("last_src")), "")
        src = clean_path(ask("Path to your local Modrinth/Minecraft instance", last))
    if not os.path.isdir(src):
        die(f"not a folder: {src}")
    channel = args.channel or "release"
    mods_dir = Path(src) / "mods"
    jars = sorted(mods_dir.glob("*.jar"))
    if not jars:
        die("no .jar files in mods/")

    print(f"Resolving {len(jars)} mods on Modrinth (channel: {channel})...")
    entries = [(p, sha1_of(str(p))) for p in jars]
    cur = modrinth_version_files([s for _, s in entries])

    plan, unknown, uptodate = [], [], 0
    allowed = CHANNELS[channel]
    for p, sha in entries:
        v = cur.get(sha)
        if not v:
            unknown.append(p.name)
            continue
        vers = modrinth_versions(v["project_id"], v.get("game_versions", []), v.get("loaders", []))
        newer = [x for x in vers
                 if x.get("version_type") in allowed
                 and x.get("date_published", "") > v.get("date_published", "")]
        if newer:
            plan.append({"path": p, "cur": v, "newer": newer})
        else:
            uptodate += 1

    print("  " + green(f"{uptodate} up to date") + " · "
          + yellow(f"{len(plan)} with updates") + " · "
          + dim(f"{len(unknown)} not on Modrinth"))
    if not plan:
        print(green("\nEverything's current. Nothing to do."))
        return

    # Per-mod version selector (unless --all takes the latest in-channel for each).
    chosen = []
    if args.all:
        for m in plan:
            m["pick"] = m["newer"][0]
            chosen.append(m)
    else:
        print(dim("\nFor each mod:  Enter = latest · number = pick a version · s = skip\n"))
        for m in plan:
            opts = m["newer"][:12]
            print(yellow(m["path"].name)
                  + dim(f"   (current {m['cur']['version_number']} [{m['cur']['version_type']}])"))
            for i, x in enumerate(opts, 1):
                tag = {"release": green, "beta": yellow, "alpha": red}.get(x["version_type"], dim)
                mark = dim("  (latest)") if i == 1 else ""
                print(f"   {i}) {x['version_number']:24} {tag('[' + x['version_type'] + ']'):20}"
                      f" {x.get('date_published', '')[:10]}{mark}")
            raw = input("   choose [1] / s: ").strip().lower()
            if raw == "s":
                continue
            if raw.isdigit() and 1 <= int(raw) <= len(opts):
                m["pick"] = opts[int(raw) - 1]
            else:
                m["pick"] = opts[0]
            chosen.append(m)

    if not chosen:
        print("nothing selected")
        return

    print(bold("\nPlan:"))
    for m in chosen:
        print("  " + yellow(f"~ {m['path'].name}")
              + dim(f"  {m['cur']['version_number']}") + "  →  "
              + green(f"{m['pick']['version_number']} [{m['pick']['version_type']}]"))
    if args.dry_run:
        print(cyan("\n(dry run — no changes made)"))
        return
    if not confirm("\nDownload and apply these to the local instance?", default=True):
        print("aborted")
        return

    tmp = Path(tempfile.mkdtemp(prefix="mcmove-update-"))
    try:
        for m in chosen:
            f = _primary_file(m["pick"])
            if not f:
                print(red(f"  ! {m['path'].name}: no downloadable file, skipping"))
                continue
            newp = tmp / f["filename"]
            download_file(f["url"], newp)
            shutil.move(str(newp), str(mods_dir / f["filename"]))
            if m["path"].name != f["filename"]:      # version-bumped filename -> drop the old jar
                try:
                    m["path"].unlink()
                except OSError:
                    pass
            print(green(f"  ↑ {m['path'].name} → {f['filename']}"))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    print(green(bold("\n✓ Local instance updated."))
          + dim("  Run `mcmove sync` to push the changes to the server."))


def uuid_to_name(uuid):
    """Reverse lookup: hyphenated/plain UUID -> current Minecraft username (or None)."""
    u = uuid.replace("-", "")
    try:
        d = _http_json(
            f"https://sessionserver.mojang.com/session/minecraft/profile/{u}"
        )
        return d.get("name")
    except Exception:  # noqa
        return None


def cmd_whois(args):
    """Resolve UUIDs to usernames — from args, a local folder, or a server's playerdata."""
    uuids = list(args.uuid or [])
    if args.dir:
        for p in Path(clean_path(args.dir)).glob("*.dat"):
            uuids.append(p.stem)
    if args.server or args.world:
        cfg = load_config()
        name, profile = _select_server(cfg, args.server)
        world = args.world or ask("Server world folder name (the level-name)", "world")
        transport, sftp = connect(profile)
        try:
            rd = f"/{world}/playerdata"
            if remote_exists(sftp, rd):
                for fn in sftp.listdir(rd):
                    if fn.endswith(".dat"):
                        uuids.append(fn[:-4])
        finally:
            sftp.close()
            transport.close()

    seen = []
    for u in uuids:
        if looks_like_uuid(u) and u not in seen:
            seen.append(u)
    if not seen:
        die("no UUIDs to look up — pass UUIDs, --dir, or --server/--world")

    print(bold(f"  {'USERNAME':<18} UUID"))
    for u in seen:
        nm = uuid_to_name(u)
        print(f"  {nm:<18} {u}" if nm else f"  {dim('(unknown)'):<18} {u}")


def extract_player(level_path):
    """Return the Player compound from a singleplayer level.dat."""
    data = nbtlib.load(level_path)
    if "Data" not in data or "Player" not in data["Data"]:
        die(f"{level_path}: no Data/Player tag — is this a single-player level.dat?")
    return data["Data"]["Player"]


def write_playerdata(player, uuid, out_dir):
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{uuid}.dat"
    f = nbtlib.File(nbtlib.Compound(player))
    f.gzipped = True
    f.root_name = ""
    f.save(str(path))
    return path


def cmd_playerdata(args):
    """Build server playerdata/<uuid>.dat files from single-player level.dat files."""
    if nbtlib is None:
        die(
            "nbtlib is required for this command. Install it with:\n    pip install nbtlib"
        )

    # Collect (level.dat, who) entries: one-shot via flags, or interactive batch.
    entries = []
    if args.level:
        entries.append(
            (
                clean_path(args.level),
                args.player or ask("Username or UUID for this level.dat"),
            )
        )
    else:
        print("Add each player: their single-player level.dat + who it belongs to.")
        print("(level.dat lives in an instance under saves/<world>/level.dat)\n")
        while True:
            lp = ask("level.dat path (blank to finish)", "")
            if not lp:
                break
            who = ask("  Minecraft username (or paste a UUID)")
            if who:
                entries.append((clean_path(lp), who))
    if not entries:
        die("nothing to do")

    name_map = resolve_uuids([w for _, w in entries if not looks_like_uuid(w)])
    out_dir = (
        Path(clean_path(args.out)) if args.out else (CONFIG_DIR / "playerdata-out")
    )

    results = []
    for lp, who in entries:
        if not os.path.isfile(lp):
            die(f"not a file: {lp}")
        uuid = (
            hyphenate_uuid(who) if looks_like_uuid(who) else name_map.get(who.lower())
        )
        if not uuid:
            die(
                f"couldn't resolve a UUID for '{who}' "
                f"(Mojang lookup failed — paste the UUID directly, or check spelling)"
            )
        player = extract_player(lp)
        path = write_playerdata(player, uuid, out_dir)
        print(green(f"  ✓ {who:20} → {path.name}"))
        results.append((uuid, path))

    print(green(bold(f"\nWrote {len(results)} playerdata file(s) to {out_dir}")))
    print(dim("  Online-mode (Mojang-auth) servers only — offline-mode UUIDs differ."))

    do_upload = args.upload or confirm(
        "\nUpload these into a server's <world>/playerdata now?", default=False
    )
    if not do_upload:
        print("  (copy them into <world>/playerdata/ yourself when ready)")
        return

    cfg = load_config()
    name, profile = _select_server(cfg, args.server)
    world = args.world or ask("Server world folder name (the level-name)", "world")
    transport, sftp = connect(profile)
    try:
        remote_dir = f"/{world}/playerdata"
        sftp_mkdirs(sftp, remote_dir)
        ts = time.strftime("%Y%m%d-%H%M%S")
        for uuid, path in results:
            rp = f"{remote_dir}/{uuid}.dat"
            if remote_exists(sftp, rp):  # keep a backup of any existing file
                try:
                    sftp.rename(rp, f"{rp}.{ts}.bak")
                except IOError:
                    pass
            sftp.put(str(path), rp)
            print(green(f"  ↑ {uuid}.dat"))
    finally:
        sftp.close()
        transport.close()
    print(
        green(
            bold(
                "\n✓ Uploaded. Restart the server — those players keep their inventory and attributes."
            )
        )
    )


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
        die(
            "authentication failed — check username/password (it's your PANEL password)."
        )
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
    req = urllib.request.Request(
        url, data=data, headers=headers, method="POST" if payload is not None else "GET"
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())


def modrinth_sides(hashes):
    """sha1 -> (project_id, server_side) via Modrinth. Best-effort; {} on failure."""
    if not hashes:
        return {}
    try:
        vf = _http_json(
            f"{MODRINTH_API}/version_files", {"hashes": hashes, "algorithm": "sha1"}
        )
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


# Channel = which release types are acceptable, widest-to-narrowest.
CHANNELS = {
    "release": {"release"},
    "beta": {"release", "beta"},
    "alpha": {"release", "beta", "alpha"},
}


def modrinth_version_files(hashes):
    """sha1 -> the Modrinth version object that file belongs to (or {} on failure)."""
    if not hashes:
        return {}
    try:
        return _http_json(f"{MODRINTH_API}/version_files",
                          {"hashes": hashes, "algorithm": "sha1"})
    except Exception:  # noqa
        return {}


def modrinth_versions(project_id, game_versions, loaders):
    """All versions of a project, filtered to a game version + loader. Newest first."""
    params = []
    if game_versions:
        params.append("game_versions=" + urllib.parse.quote(json.dumps(sorted(set(game_versions)))))
    if loaders:
        params.append("loaders=" + urllib.parse.quote(json.dumps(sorted(set(loaders)))))
    url = f"{MODRINTH_API}/project/{project_id}/version"
    if params:
        url += "?" + "&".join(params)
    try:
        vers = _http_json(url)
        vers.sort(key=lambda x: x.get("date_published", ""), reverse=True)
        return vers
    except Exception:  # noqa
        return []


def _primary_file(version):
    files = version.get("files", [])
    if not files:
        return None
    return next((f for f in files if f.get("primary")), files[0])


def download_file(url, dest):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as r, open(dest, "wb") as out:
        shutil.copyfileobj(r, out)


def read_jar_meta(path):
    """Offline fallback: (modid, env) from a jar. env in {client, server, both, None}."""
    try:
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())
            if "fabric.mod.json" in names:
                data = json.loads(z.read("fabric.mod.json").decode("utf-8", "replace"))
                env = {"client": "client", "server": "server"}.get(
                    data.get("environment"), "both"
                )
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
    infos = [
        {"path": str(p), "filename": Path(p).name, "sha1": sha1_of(p)} for p in paths
    ]
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
        i["side"] = {"client": "client", "server": "keep", "both": "keep"}.get(
            env, "unknown"
        )
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
    plan = {
        "add": [],
        "update": [],
        "remove": [],
        "keep": [],
        "client": [],
        "unknown": [],
    }
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
                    i["filename"] if i["filename"] in remote_files else i
                )
            else:
                if (
                    prev["filename"] in remote_files
                    and prev["filename"] != i["filename"]
                ):
                    plan["remove"].append(prev["filename"])
                plan["update"].append(i)
        else:
            (plan["keep"] if i["filename"] in remote_files else plan["add"]).append(
                i["filename"] if i["filename"] in remote_files else i
            )
        new_managed[key] = {
            "filename": i["filename"],
            "sha1": i["sha1"],
            "side": i["side"],
        }
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
    print(
        f"  {len(infos) - n_client} server-side · "
        + dim(f"{n_client} client-only (skipped)")
        + (dim(f" · {n_unknown} undetermined (kept)") if n_unknown else "")
    )

    manifest = load_manifest(server_name)
    if True:
        remote_files = (
            set(sftp.listdir("/mods")) if remote_exists(sftp, "/mods") else set()
        )
        plan, new_managed = plan_mod_sync(infos, manifest, remote_files)

        print(bold("\nPlan:"))
        print(
            "  "
            + green(f"add {len(plan['add'])}")
            + " · "
            + yellow(f"update {len(plan['update'])}")
            + " · "
            + red(f"remove {len(plan['remove'])}")
            + " · "
            + dim(
                f"unchanged {len(plan['keep'])} · client skipped {len(plan['client'])}"
            )
        )
        for i in plan["add"]:
            print(green(f"  + add     {i['filename']}"))
        for i in plan["update"]:
            print(yellow(f"  ~ update  {i['filename']}"))
        for fn in dict.fromkeys(plan["remove"]):
            print(red(f"  - remove  {fn}"))
        if plan["unknown"]:
            print(
                dim(
                    f"  ? kept (couldn't determine side): {', '.join(plan['unknown'][:8])}"
                    + (" ..." if len(plan["unknown"]) > 8 else "")
                )
            )

        if not (plan["add"] or plan["update"] or plan["remove"]):
            print(green("\nServer mods already up to date. Nothing to do."))
            return
        if dry_run:
            print(cyan("\n(dry run — no changes made)"))
            return

        # Safety guard: a sync that removes a large share of managed mods almost
        # always means the wrong/incomplete local instance was selected.
        n_remove = len(dict.fromkeys(plan["remove"]))
        managed_total = max(len(manifest.get("mods", {})), 1)
        if n_remove >= 15 and n_remove > 0.5 * managed_total:
            print(
                red(
                    bold(
                        f"\n⚠  This would REMOVE {n_remove} mods from the server "
                        f"— more than half of what mcmove manages here."
                    )
                )
            )
            print(red("   That usually means this isn't the right/complete instance."))
            if not confirm("   Type y only if you're SURE. Proceed?", default=False):
                print("aborted")
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
            print(red(f"  - {fn}"))
        for i in plan["add"] + plan["update"]:
            sftp.put(i["path"], "/mods/" + i["filename"])
            print(green(f"  ↑ {i['filename']}"))

        manifest["mods"] = new_managed
        save_manifest(server_name, manifest)
    print(green(bold("\n✓ Mods patched. Restart the server to load changes.")))


def sync_mods(profile, server_name, src, dry_run):
    """Standalone mod patch: open a connection and run do_mod_sync."""
    transport, sftp = connect(profile)
    try:
        do_mod_sync(sftp, server_name, src, dry_run)
    finally:
        sftp.close()
        transport.close()


def pull_mods(profile, server_name, src, dry_run, mirror):
    """Reverse sync: download the server's /mods and patch the LOCAL instance.

    Default is additive + update only — it NEVER deletes a local mod, so your
    client-only mods (shaders, minimaps, etc.) are always safe. `--mirror` also
    removes local *server-side* mods that are gone from the server, but still
    leaves client-only mods untouched.
    """
    mods_dir = Path(src) / "mods"
    if not mods_dir.is_dir():
        die(f"no mods/ folder in {src}")
    local_paths = sorted(mods_dir.glob("*.jar"))
    local_files = {p.name for p in local_paths}
    local_by_key = {}
    for p in local_paths:
        modid, _ = read_jar_meta(str(p))
        local_by_key[("mod:" + modid) if modid else ("file:" + p.name)] = p

    transport, sftp = connect(profile)
    tmp = Path(tempfile.mkdtemp(prefix="mcmove-pull-"))
    try:
        # Only real .jar files — skip directories and stray non-jar entries.
        server_files = set()
        if remote_exists(sftp, "/mods"):
            for e in sftp.listdir_attr("/mods"):
                if e.filename.endswith(".jar") and not stat.S_ISDIR(e.st_mode):
                    server_files.add(e.filename)
        if not server_files:
            die("server has no .jar mods in /mods")

        add, update, skip, failed = [], [], [], []
        for sf in sorted(server_files):
            if sf in local_files:  # same filename = same version
                skip.append(sf)
                continue
            lp = tmp / sf  # new to us: fetch + inspect
            try:
                sftp.get("/mods/" + sf, str(lp))
            except (IOError, OSError) as e:  # unreadable file -> skip, don't abort
                failed.append(sf)
                print(red(f"  ! couldn't download {sf}: {e} — skipping"))
                continue
            modid, _ = read_jar_meta(str(lp))
            key = ("mod:" + modid) if modid else ("file:" + sf)
            old = local_by_key.get(key)
            if old is not None and old.name != sf:
                update.append((sf, old, lp))  # same mod, new version
            else:
                add.append((sf, lp))

        remove = []
        if mirror:
            print(
                "Resolving which local mods are server-side (client-only are protected)..."
            )
            for i in classify_mods([str(p) for p in local_paths]):
                if i["side"] == "keep" and i["filename"] not in server_files:
                    remove.append(Path(i["path"]))

        print(bold("\nPlan (server → local instance):"))
        line = (
            "  "
            + green(f"add {len(add)}")
            + " · "
            + yellow(f"update {len(update)}")
            + " · "
            + dim(f"unchanged {len(skip)}")
        )
        if mirror:
            line += " · " + red(f"remove {len(remove)}")
        print(line)
        for sf, _ in add:
            print(green(f"  + add     {sf}"))
        for sf, old, _ in update:
            print(yellow(f"  ~ update  {old.name}  →  {sf}"))
        for p in remove:
            print(red(f"  - remove  {p.name}"))

        if not (add or update or remove):
            print(green("\nYour instance already matches the server. Nothing to do."))
            return
        if dry_run:
            print(cyan("\n(dry run — no changes made)"))
            return
        if not confirm("\nApply to your LOCAL instance?", default=True):
            print("aborted")
            return

        for sf, lp in add:
            shutil.move(str(lp), str(mods_dir / sf))
            print(green(f"  + {sf}"))
        for sf, old, lp in update:
            try:
                old.unlink()
            except OSError:
                pass
            shutil.move(str(lp), str(mods_dir / sf))
            print(yellow(f"  ~ {old.name} → {sf}"))
        for p in remove:
            try:
                p.unlink()
            except OSError:
                pass
            print(red(f"  - {p.name}"))
        print(
            green(
                bold("\n✓ Local instance patched from the server. Restart your game.")
            )
        )
    finally:
        sftp.close()
        transport.close()
        shutil.rmtree(tmp, ignore_errors=True)


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
    src = clean_path(
        args.src or ask("Path to your local Modrinth/Minecraft instance", remembered)
    )
    if not os.path.isdir(src):
        die(f"not a folder: {src}")

    actions = pick_many(
        "What do you want to move?",
        [
            ("Mods (patch — add/update/remove, skips client-only)", "mods"),
            ("World (from saves/)", "world"),
            ("Config (config/)", "config"),
        ],
    )
    if not actions:
        die("nothing selected")

    world_src = level_name = None
    if "world" in actions:
        saves = Path(src) / "saves"
        worlds = (
            sorted([p for p in saves.iterdir() if p.is_dir()]) if saves.is_dir() else []
        )
        if not worlds:
            die(f"no worlds found in {saves}")
        world_src = pick_one("Which world?", [(p.name, str(p)) for p in worlds])
        if not world_src:
            die("no world selected")
        default_name = "world"
        level_name = ask("Target level-name on the server", default_name)

    clear_world = "world" in actions and confirm(
        f"Clear existing remote /{level_name} first?", default=False
    )
    # World/config overwrite, so offer a backup. Mods are patched (non-destructive
    # to unmanaged files), so they're excluded from the backup.
    backup_actions = [a for a in actions if a in ("world", "config")]
    do_backup = bool(backup_actions) and confirm(
        "Back up the server's current world/config before overwriting?", default=True
    )

    print("\nPlan:")
    print(
        f"  server : {server_name}  ({profile['username']}@{profile['host']}:{profile['port']})"
    )
    print(f"  source : {src}")
    for a in actions:
        if a == "world":
            print(
                f"  world  : {Path(world_src).name}  ->  /{level_name}"
                + ("  (clearing target)" if clear_world else "")
            )
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

    print(green(bold("\n✓ Done. Restart the server in the panel to load the changes.")))
    if "world" in actions:
        print(
            "  Note: if this was a single-player world on vanilla, dimensions are nested"
        )
        print(
            "  inside the world folder — that's fine for modded/Forge/NeoForge servers."
        )


# ----------------------------------------------------------------------------- main
def main():
    p = argparse.ArgumentParser(
        description="Move worlds/mods/configs into a Pelican server over SFTP."
    )
    p.add_argument("--version", action="version", version=f"mcmove {__version__}")
    sub = p.add_subparsers(dest="cmd")

    sub.add_parser("list", help="list configured servers")
    add = sub.add_parser(
        "add-server", help="add a server profile (from panel SFTP Details)"
    )
    add.add_argument("--url", help="paste the panel's sftp://user@host:port string")
    rm = sub.add_parser("remove-server", help="remove a server profile")
    rm.add_argument("name")

    wiz = sub.add_parser("move", help="run the move wizard (default)")
    wiz.add_argument("--src", help="path to local instance (skips the prompt)")

    syn = sub.add_parser(
        "sync", help="patch a server's mods to match a local instance (local → server)"
    )
    syn.add_argument("--server", help="saved server name (otherwise you'll be asked)")
    syn.add_argument(
        "--src", help="path to local instance (otherwise remembered/asked)"
    )
    syn.add_argument(
        "--dry-run", action="store_true", help="show the plan, change nothing"
    )

    pul = sub.add_parser(
        "pull",
        help="download the server's mods into your local instance (server → local)",
    )
    pul.add_argument("--server", help="saved server name (otherwise you'll be asked)")
    pul.add_argument(
        "--src", help="path to local instance (otherwise remembered/asked)"
    )
    pul.add_argument(
        "--dry-run", action="store_true", help="show the plan, change nothing"
    )
    pul.add_argument(
        "--mirror",
        action="store_true",
        help="also remove local SERVER-SIDE mods missing from the server "
        "(client-only mods are always kept)",
    )

    pd = sub.add_parser(
        "playerdata",
        help="build server playerdata/<uuid>.dat from single-player level.dat files",
    )
    pd.add_argument(
        "--level", help="path to a single-player level.dat (omit for interactive batch)"
    )
    pd.add_argument("--player", help="username or UUID this level.dat belongs to")
    pd.add_argument(
        "--out", help="output dir (default ~/.config/mcmove/playerdata-out)"
    )
    pd.add_argument(
        "--upload",
        action="store_true",
        help="upload results to a server's <world>/playerdata",
    )
    pd.add_argument("--server", help="server name for --upload")
    pd.add_argument("--world", help="server world folder (level-name) for --upload")

    upd = sub.add_parser(
        "update",
        help="check Modrinth for newer mod versions and update the local instance",
    )
    upd.add_argument("--src", help="path to local instance (otherwise remembered/asked)")
    upd.add_argument(
        "--channel",
        choices=["release", "beta", "alpha"],
        help="newest release channel to allow (default: release)",
    )
    upd.add_argument(
        "--all",
        action="store_true",
        help="take the latest in-channel for every mod (no per-mod prompts)",
    )
    upd.add_argument("--dry-run", action="store_true", help="show the plan, change nothing")

    who = sub.add_parser(
        "whois",
        help="resolve UUIDs to usernames (args, a folder, or a server's playerdata)",
    )
    who.add_argument("uuid", nargs="*", help="one or more UUIDs to look up")
    who.add_argument("--dir", help="a local folder of <uuid>.dat files")
    who.add_argument("--server", help="read the server's <world>/playerdata listing")
    who.add_argument("--world", help="server world folder (level-name)")

    args = p.parse_args()
    if args.cmd == "list":
        cmd_list(args)
    elif args.cmd == "add-server":
        cmd_add_server(args)
    elif args.cmd == "remove-server":
        cmd_remove_server(args)
    elif args.cmd == "sync":
        cmd_sync(args)
    elif args.cmd == "pull":
        cmd_pull(args)
    elif args.cmd == "playerdata":
        cmd_playerdata(args)
    elif args.cmd == "whois":
        cmd_whois(args)
    elif args.cmd == "update":
        cmd_update(args)
    else:  # move / default
        if not hasattr(args, "src"):
            args.src = None
        run_wizard(args)


def cli():
    """Console entry point (used by the installed `mcmove` command)."""
    try:
        main()
    except KeyboardInterrupt:
        print("\naborted")
        sys.exit(130)


if __name__ == "__main__":
    cli()
