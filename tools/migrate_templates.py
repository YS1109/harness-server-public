"""Explicit, create-only template migration via the existing Harness CLI.

No credential handling, service-directory writes, configuration changes or automatic
replacement. The default is a read-only plan; --apply creates missing resources.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import uuid

NAMES = ("technical-plan-template.md", "api-doc-template.md", "release-doc-template.md")
MAX_BYTES = 256 * 1024


class MigrationError(Exception):
    pass


def invoke(client, args):
    try:
        return subprocess.run(client + args, capture_output=True, encoding="utf-8", errors="replace", timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        raise MigrationError("客户端无法执行或请求超时；未确认的写入须沿用报告中的幂等键核验/重试。") from None


def digest(content):
    return hashlib.sha256(content).hexdigest()


def migrate(source, client, namespace, apply=False):
    report = {"namespace": namespace, "apply": apply, "entries": [], "saved": [], "pending_write": None}
    contents = {}
    try:
        # Validate all source files before contacting the service. Preserve bytes/CRLF.
        if source.is_symlink() or not source.is_dir():
            raise MigrationError("来源必须是模板目录，不能是符号链接。")
        for name in NAMES:
            file = source / name
            if file.is_symlink() or not file.is_file():
                raise MigrationError(f"缺少普通模板文件：{name}")
            with file.open("rb") as handle:
                content = handle.read(MAX_BYTES + 1)
            try:
                text = content.decode("utf-8")
            except UnicodeDecodeError:
                raise MigrationError(f"模板必须是UTF-8：{name}") from None
            if len(content) > MAX_BYTES or not text.strip() or "\0" in text:
                raise MigrationError(f"模板为空、含NUL或超过256KiB：{name}")
            contents[name] = content
        # Probe every target first. A 400/403/connection failure is never 'missing'.
        for name, content in contents.items():
            path = f"memory/templates/{name}"
            result = invoke(client, ["get", "--path", path])
            status = "missing"
            if result.returncode:
                code = re.search(r"HTTP (\d{3})", result.stderr)
                if not code or code[1] != "404":
                    raise MigrationError(f"读取{path}失败（{code[1] if code else '未确认HTTP状态'}）；请先升级/核验服务和授权，不会继续写入。")
            else:
                try:
                    doc = json.loads(result.stdout)
                    remote = doc["content"].encode("utf-8")
                    if doc["sha256"] != digest(remote):
                        raise ValueError("invalid hash")
                except (ValueError, KeyError, AttributeError, TypeError):
                    raise MigrationError(f"读取{path}返回格式或哈希无效。") from None
                if doc.get("namespace", namespace) != namespace:
                    raise MigrationError("目标读取继承了另一个空间；请显式选择来源空间，本工具不创建覆盖版本。")
                status = "unchanged" if remote == content else "conflict"
            report["entries"].append({"path": path, "bytes": len(content), "sha256": digest(content), "status": status})
        if any(e["status"] == "conflict" for e in report["entries"]):
            raise MigrationError("目标已有不同正文；不会覆盖。请先通过get/put和最新SHA人工合并。")
        if apply:
            with tempfile.TemporaryDirectory(prefix="harness-templates-") as tmp:
                for entry in report["entries"]:
                    if entry["status"] != "missing":
                        continue
                    name = entry["path"].rsplit("/", 1)[-1]
                    staged = Path(tmp) / name
                    staged.write_bytes(contents[name])
                    key = uuid.uuid4().hex
                    report["pending_write"] = {"path": entry["path"], "idempotency_key": key, "sha256": entry["sha256"]}
                    result = invoke(client, ["--idempotency-key", key, "put", "--path", entry["path"], "--file", str(staged)])
                    if result.returncode:
                        raise MigrationError("模板保存未确认；已保存项不会回滚，不会自动重试或覆盖。请保留pending_write并核验目标；重试必须使用相同正文和首次幂等键。")
                    try:
                        saved = json.loads(result.stdout)
                        if saved["path"] != entry["path"] or saved["sha256"] != entry["sha256"]:
                            raise ValueError("unexpected result")
                    except (ValueError, KeyError, TypeError):
                        raise MigrationError("保存响应未能核验；请保留pending_write并核验目标，不将它视为成功。") from None
                    report["saved"].append(entry["path"])
                    report["pending_write"] = None
        report["status"] = "completed" if apply else "planned"
    except (MigrationError, OSError) as error:
        report["status"] = "failed"
        # OSError can contain absolute paths; don't echo it or raw service errors.
        report["error"] = str(error) if isinstance(error, MigrationError) else "本地模板文件读取/暂存失败。"
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path, help="directory containing exactly the three selected template sources")
    parser.add_argument("--client", default="harness", help="existing client or credential-injecting launcher")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--namespace", required=True, help="explicit, already authorized destination")
    parser.add_argument("--apply", action="store_true", help="create missing resources; never replace differing content")
    args = parser.parse_args()
    client = [args.client, "--namespace", args.namespace]
    if args.config:
        client += ["--config", str(args.config)]
    report = migrate(args.source, client, args.namespace, args.apply)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if report["status"] == "failed" else 0


if __name__ == "__main__":
    sys.exit(main())
