"""Exercise the launcher against raw Git objects and locally built real catalogs."""
import argparse
import base64
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import hashlib
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
from urllib.parse import parse_qs, unquote, urlsplit
from zipfile import ZipFile


class PackMirror(ThreadingHTTPServer):
    def __init__(self, repo, catalogs):
        super().__init__(("127.0.0.1", 0), Handler)
        self.repo = repo
        self.catalogs = catalogs
        self.tag = "139"
        self.legacy = False
        self.asset_override = None
        self.transfers = defaultdict(lambda: [0, 0])
        self.lock = threading.Lock()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, stderr=subprocess.DEVNULL)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        if self.path != "/git/git-upload-pack":
            self.send_error(404)
            return
        import os
        with subprocess.Popen(
            ["git", "-c", "uploadpack.allowFilter=true", "-c",
             "uploadpack.allowAnySHA1InWant=true", "upload-pack", "--stateless-rpc", "."],
            cwd=self.server.repo, env={**os.environ, "GIT_PROTOCOL": "version=2"},
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        ) as git:
            git.stdin.write(self.rfile.read(int(self.headers["Content-Length"])))
            git.stdin.close()
            self.send_response(200)
            self.send_header("Content-Type", "application/x-git-upload-pack-result")
            self.end_headers()
            self.stream("git", git.stdout)

    def stream(self, category, contents):
        count = 0
        try:
            for chunk in iter(lambda: contents.read(1024 * 1024), b""):
                self.wfile.write(chunk)
                count += len(chunk)
        finally:
            self.record(category, count)

    def record(self, category, count):
        with self.server.lock:
            entry = self.server.transfers[category]
            entry[0] += 1
            entry[1] += count

    def body(self, category, body, status=200):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.record(category, len(body))

    def do_GET(self):
        url = urlsplit(self.path)
        parts = unquote(url.path).strip("/").split("/")
        category = parts[0]
        try:
            if category == "releases":
                return self.asset(parts)
            if category == "raw":
                revision, *path = parts[1:]
                if revision == "main":
                    revision = self.server.tag
                spec = f"{revision}:{'/'.join(path)}"
                return self.body("raw", self.server.git("show", spec))
            if category == "api":
                return self.api(parts[1:], parse_qs(url.query))
        except (FileNotFoundError, subprocess.CalledProcessError):
            pass
        self.body(category, b"", 404)

    def asset(self, parts):
        if parts[1:3] == ["latest", "download"]:
            tag = self.server.tag
        elif parts[1] == "download" and len(parts) == 4:
            tag = parts[2]
        else:
            return self.body("releaseMetadata", b"", 404)
        name = parts[-1]
        category = "bundle" if name.endswith(".zip") else "releaseMetadata"
        if not tag.isdecimal() or not (name == "auram-pack.json"
                                       or name.startswith("pack-") and name.endswith(".zip")):
            return self.body(category, b"", 404)
        root = self.server.asset_override if tag == "161" else None
        path = (root or self.server.catalogs / tag) / name
        if self.server.legacy or not path.is_file():
            return self.body(category, b"", 404)
        with path.open("rb") as contents:
            self.send_response(200)
            self.send_header("Content-Length", str(path.stat().st_size))
            self.end_headers()
            self.stream(category, contents)

    def api(self, parts, query):
        git = self.server.git
        if parts[:3] == ["git", "ref", "tags"]:
            sha = git("rev-parse", f"refs/tags/{parts[3]}^{{commit}}").decode().strip()
            document = {"object": {"type": "commit", "sha": sha}}
        elif parts[:2] == ["git", "trees"]:
            args = ["-r", "-t"] if query.get("recursive") == ["1"] else []
            tree = []
            for line in git("ls-tree", "-l", "-z", *args, parts[2]).split(b"\0"):
                if line:
                    fields, path = line.split(b"\t", 1)
                    mode, kind, sha, size = fields.decode().split()
                    tree.append({"mode": mode, "type": kind, "sha": sha,
                                 "size": None if size == "-" else int(size), "path": path.decode()})
            document = {"tree": tree, "truncated": False}
        else:
            raise FileNotFoundError
        self.body("api", json.dumps(document).encode())


def split_catalog(mirror, count, output, fixture_key):
    """Repack real changed Git blobs with a different number of local archives."""
    base = json.loads((mirror.catalogs / "139/manifest.json").read_bytes())
    target = json.loads((mirror.catalogs / "161/manifest.json").read_bytes())
    baseline = {file["sha"] for file in base["files"].values()}
    missing = {file["sha"]: file for file in target["files"].values()
               if file["sha"] not in baseline}
    assert len(missing) >= count
    target["bundles"] = {}
    target["objects"] = {}
    output.mkdir()
    keys = sorted(missing)
    for index in range(count):
        members = {}
        archive = output / "asset.zip"
        with ZipFile(archive, "w") as destination:
            for oid in keys[index::count]:
                body = mirror.git("cat-file", "blob", oid)
                assert hashlib.sha256(body).hexdigest() == missing[oid]["sha256"]
                destination.writestr(f"objects/{oid}", body)
                members[oid] = {field: missing[oid][field] for field in ("sha256", "size")}
        identity = hashlib.sha256(archive.read_bytes()).hexdigest()
        name = f"pack-{identity}.zip"
        archive.rename(output / name)
        target["bundles"][identity] = {
            "sha256": identity, "size": (output / name).stat().st_size, "members": members,
            "url": f"https://github.com/AuramMods/Auram/releases/download/161/{name}",
        }
        target["objects"].update(dict.fromkeys(members, identity))
    asset = output / "manifest.json"
    payload = json.dumps(target, sort_keys=True, separators=(",", ":")).encode() + b"\n"
    asset.write_bytes(payload)
    signature = output / "manifest.minisig"
    subprocess.run(["minisign", "-S", "-s", fixture_key, "-m", asset, "-x", signature],
                   check=True, capture_output=True)
    (output / "auram-pack.json").write_text(json.dumps({
        "payload": base64.b64encode(payload).decode(), "signature": signature.read_text(),
    }))


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("repo", type=Path)
    cli.add_argument("catalogs", type=Path)
    cli.add_argument("launcher_example", type=Path)
    cli.add_argument("--legacy", action="store_true")
    cli.add_argument("--public", action="store_true", help="update a local build 159 to the published build 161")
    cli.add_argument("--boundary", action="store_true", help="test real pack files with eight and nine local archives")
    cli.add_argument("--runtime-cache", type=Path, help="optional previous isolated QA data root")
    args = cli.parse_args()
    with tempfile.TemporaryDirectory(prefix="Auram pack QA ") as directory:
        root = Path(directory)
        with PackMirror(args.repo.resolve(), args.catalogs.resolve()) as mirror:
            thread = threading.Thread(target=mirror.serve_forever, daemon=True)
            thread.start()
            origin = f"http://127.0.0.1:{mirror.server_port}"

            def run(case, name, tag, strategy, mode="update", public=False, key=None):
                mirror.tag = str(tag)
                mirror.transfers.clear()
                output = subprocess.check_output(
                    [args.launcher_example.resolve(), root / name, str(tag), mode,
                     *([] if public else [origin]), *([key] if key else [])],
                    text=True,
                )
                result = json.loads(output)
                transfers = dict(mirror.transfers)
                print(json.dumps({"case": case, **result, "requestsAndBodyBytes": transfers}), flush=True)
                if strategy:
                    assert result["strategy"]["strategy"] == strategy
                if strategy == "bundles":
                    assert not transfers.get("api") and not transfers.get("git")
                return result, transfers

            try:
                if args.public:
                    run("prepare pack 159", "client", 159, "sparse")
                    result, _ = run("published 159 to 161 update", "client", 161, "bundles", public=True)
                    assert result["strategy"]["requiredBundles"] == 1
                    repeated, _ = run("published repeat", "client", 161, "bundles", public=True)
                    assert repeated["downloadBodyBytes"] == 0
                    return
                run("fresh sparse", "client", 139, "sparse")
                if args.boundary:
                    private = root / "fixture.key"
                    public = root / "fixture.pub"
                    subprocess.run(["minisign", "-G", "-W", "-s", private, "-p", public],
                                   check=True, capture_output=True)
                    for count, strategy in ((8, "bundles"), (9, "sparse")):
                        archive_dir = root / f"archives-{count}"
                        split_catalog(mirror, count, archive_dir, private)
                        mirror.asset_override = archive_dir
                        instance = f"boundary-{count}"
                        shutil.copytree(root / "client", root / instance)
                        custom = root / instance / "client/minecraft/config/qa-only.toml"
                        custom.write_text("preserve me")
                        result, transfers = run(f"{count} real payload archives", instance, 161, strategy, key=public)
                        assert result["strategy"]["requiredBundles"] == count
                        assert transfers.get("bundle", [0])[0] == (count if count == 8 else 0)
                        assert custom.exists() == (count == 8)
                        if count == 9:
                            assert list((root / instance / "client/.auram-backups").glob(
                                "*/original/minecraft/config/qa-only.toml"))
                    return
                if args.runtime_cache:
                    for name in ("minecraft", "java", "metadata", "forge"):
                        shutil.copytree(args.runtime_cache / "cache" / name,
                                        root / "client" / "cache" / name, dirs_exist_ok=True)
                    for name in ("minecraft-1.20.1.json", *[p.name for p in
                                  (args.runtime_cache / "state").glob("forge-runtime-*.json")]):
                        shutil.copy2(args.runtime_cache / "state" / name,
                                     root / "client" / "state" / name)
                    before, _ = run("prime shared runtime", "client", 139, None, "runtime")
                world = root / "client/client/minecraft/saves/QA/level.dat"
                world.parent.mkdir(parents=True)
                world.write_bytes(b"preserve test world")
                run("adjacent update", "client", 141, "bundles")
                run("skipped release update", "client", 161, "bundles")
                if args.runtime_cache:
                    after, _ = run("verify shared runtime after update", "client", 161, None, "runtime")
                    assert (before["minecraft"], before["forge"]) == (after["minecraft"], after["forge"])
                    assert after["runtimeDownloadBodyBytes"] == 0
                result, _ = run("repeat update", "client", 161, "bundles")
                assert result["downloadBodyBytes"] == 0
                index = json.loads((root / "client/client/.auram-manifest.json").read_bytes())
                path, file = next((p, f) for p, f in index["files"].items() if p.startswith("minecraft/mods/"))
                (root / "client/client" / path).write_bytes(b"corrupt")
                (root / "client/cache/blobs" / file["sha"]).unlink()
                run("repair damaged jar", "client", 161, None, "repair")
                assert (root / "client/client" / path).stat().st_size == file["size"]
                assert world.read_bytes() == b"preserve test world"
                if args.legacy:
                    mirror.legacy = True
                    _, transfers = run("legacy without release assets", "legacy", 145, None)
                    assert transfers["releaseMetadata"][1] == 0 and transfers["api"][0] > 0
            finally:
                mirror.shutdown()
                thread.join()


if __name__ == "__main__":
    main()
