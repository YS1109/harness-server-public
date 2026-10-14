"""Release packaging only; Python is not required to run the shipped client."""

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import threading
import tomllib
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "macos-arm64": "aarch64-apple-darwin",
    "macos-x64": "x86_64-apple-darwin",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "windows-x64": "x86_64-pc-windows-msvc",
}


def version():
    with (ROOT / "Cargo.toml").open("rb") as source:
        return tomllib.load(source)["package"]["version"]


def archive_name(ver, name):
    extension = "zip" if name.startswith("windows-") else "tar.gz"
    return f"harness-v{ver}-{name}.{extension}"


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run_client(command, env):
    # The Rust client emits UTF-8 regardless of the Windows runner's code page.
    return subprocess.run(
        command, env=env, check=True, capture_output=True,
        text=True, encoding="utf-8", timeout=30,
    ).stdout


def smoke(binary, ver):
    # Isolate from the runner's configuration and credentials. The temporary
    # HTTP fixture checks the extracted executable, not an unarchived build.
    env = os.environ.copy()
    for key in ("HARNESS_CLIENT_CONFIG", "HARNESS_URL", "HARNESS_TOKEN", "HARNESS_AGENT_ID", "HARNESS_NAMESPACE", "HARNESS_READ_NAMESPACES"):
        env.pop(key, None)
    with tempfile.TemporaryDirectory() as tmp:
        config = Path(tmp) / "client.json"
        config.write_text("{}", encoding="utf-8")
        command = [str(binary.resolve()), "--config", str(config)]

        def run(args):
            return run_client(command + args, env)

        if run(["--version"]).strip() != f"harness-server {ver}":
            raise ValueError("packaged binary version mismatch")
        if "Remote Harness client" not in run(["--help"]):
            raise ValueError("packaged binary help mismatch")
        rules = json.loads(run(["instructions", "--json"]))
        if rules.get("version") != ver or rules.get("source") != "embedded-client":
            raise ValueError("packaged embedded rules mismatch")
        if "--namespace shared finish --mode log" not in rules.get("instructions", ""):
            raise ValueError("packaged shared knowledge rules missing")
        if config.read_text(encoding="utf-8") != "{}":
            raise ValueError("reading packaged rules changed configuration")
        if "harness instructions" not in run(["rules", "--bootstrap"]):
            raise ValueError("packaged bootstrap missing")
        token = uuid.uuid4().hex
        env["HARNESS_TOKEN"] = token

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path != "/v1/health" or self.headers.get("Authorization") != f"Bearer {token}":
                    self.send_error(401)
                    return
                result = {"ok": True, "fixture": "packaged-client"}
                namespace = self.headers.get("X-Harness-Namespace")
                agent_id = self.headers.get("X-Harness-Agent-Id")
                if namespace or agent_id:
                    result["identity"] = {"agent_id": agent_id, "namespace": namespace or "shared"}
                body = json.dumps(result).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                result = json.loads(run(["--url", f"http://127.0.0.1:{server.server_port}", "health"]))
                if result != {"ok": True, "fixture": "packaged-client"}:
                    raise ValueError("packaged binary HTTP response mismatch")
                first = json.loads(run(["init"]))
                repeat = json.loads(run(["init"]))
                if first["agent_id"] != repeat["agent_id"] or first["namespace"] != repeat["namespace"]:
                    raise ValueError("packaged initialization did not preserve identity")
                if first["namespace"] is not None:
                    raise ValueError("initialization must not invent a namespace")
                custom = json.loads(run(["init", "--agent-id", "package-smoke", "--namespace", "package-private"]))
                identity = json.loads(run(["identity"]))
                if custom["agent_id"] != identity["agent_id"] or identity["namespace"] != "package-private":
                    raise ValueError("packaged custom identity mismatch")
                rules = json.loads(run(["instructions", "--json"]))
                if rules.get("task_namespace") != "package-private" or rules.get("agent_id") != "package-smoke":
                    raise ValueError("packaged rules do not reflect configured scope")
                scoped = json.loads(run(["--url", f"http://127.0.0.1:{server.server_port}", "health"]))
                if scoped.get("identity") != {"agent_id": "package-smoke", "namespace": "package-private"}:
                    raise ValueError("packaged namespace header mismatch")
            finally:
                server.shutdown()
                thread.join()


def package(target, name, output):
    if TARGETS.get(name) != target:
        raise ValueError("target/platform mismatch")
    host_os = {"Darwin": "macos", "Linux": "linux", "Windows": "windows"}[platform.system()]
    host_arch = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64", "amd64": "x64"}[platform.machine().lower()]
    if name != f"{host_os}-{host_arch}":
        raise ValueError("packaging must run on the matching native runner")
    ver = version()
    executable = "harness.exe" if name.startswith("windows-") else "harness"
    binary = ROOT / "target" / target / "release" / executable
    output.mkdir(parents=True, exist_ok=True)
    archive = output / archive_name(ver, name)
    with tempfile.TemporaryDirectory() as tmp:
        staging = Path(tmp) / archive.name.split(".zip")[0].removesuffix(".tar.gz")
        staging.mkdir()
        shutil.copy2(binary, staging / executable)
        shutil.copy2(ROOT / "examples/client.example.json", staging / "client.example.json")
        shutil.copy2(ROOT / "docs/CLIENT-INSTALL.md", staging / "INSTALL.md")
        if executable == "harness":
            (staging / executable).chmod(0o755)
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as bundle:
                for path in sorted(staging.iterdir()):
                    bundle.write(path, f"{staging.name}/{path.name}")
        else:
            with tarfile.open(archive, "w:gz") as bundle:
                bundle.add(staging, arcname=staging.name)
        extracted = Path(tmp) / "extracted"
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive) as bundle:
                bundle.extractall(extracted)
        else:
            with tarfile.open(archive) as bundle:
                bundle.extractall(extracted, filter="data")
        smoke(extracted / staging.name / executable, ver)
    checksum = f"{digest(archive)}  {archive.name}\n"
    archive.with_name(archive.name + ".sha256").write_text(checksum, encoding="utf-8")
    print(f"Packaged and verified {archive.name}")


def finalize(output):
    ver = version()
    names = sorted(archive_name(ver, name) for name in TARGETS)
    expected = set(names) | {name + ".sha256" for name in names}
    actual = {p.name for p in output.iterdir()} - {"SHA256SUMS"}
    if actual != expected:
        raise ValueError(f"incomplete or unexpected assets: missing={expected - actual}, extra={actual - expected}")
    checksums = []
    for name in names:
        checksum = f"{digest(output / name)}  {name}\n"
        if (output / (name + ".sha256")).read_text(encoding="utf-8") != checksum:
            raise ValueError(f"checksum mismatch: {name}")
        checksums.append(checksum)
    (output / "SHA256SUMS").write_text("".join(checksums), encoding="utf-8")
    print(f"Verified all {len(names)} archives; wrote SHA256SUMS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pack = commands.add_parser("package")
    pack.add_argument("--target", required=True, choices=TARGETS.values())
    pack.add_argument("--platform", required=True, choices=TARGETS)
    pack.add_argument("--output", type=Path, required=True)
    collect = commands.add_parser("finalize")
    collect.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "package":
        package(args.target, args.platform, args.output)
    else:
        finalize(args.output)
