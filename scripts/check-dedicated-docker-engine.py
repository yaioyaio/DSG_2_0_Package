"""Stage one disposable image for approval in a bounded, private Linux service."""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tarfile
import time
from contextlib import ExitStack


ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C"}
NAME = re.compile(r"dsg-gc-[0-9a-f]{12}")
ENGINE = re.compile(r"[A-Za-z0-9:-]{8,128}")
OWNER_LABEL = "io.dsg.gc.owner"
CACHE = (
    '{{if eq .Type "Build Cache"}}'
    '{{.TotalCount}} {{.Active}}{{end}}'
)


class CheckFailed(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise CheckFailed(reason)


def digest(value):
    return hashlib.sha256(value).hexdigest()


def encode(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


def archive_for(name, owner):
    """Build a Classic image archive. The image ID is the original JSON hash."""
    config = encode({
        "architecture": "arm64",
        "os": "linux",
        "config": {"Labels": {OWNER_LABEL: owner, "io.dsg.fixture": name}},
        "rootfs": {"type": "layers", "diff_ids": []},
    })
    manifest = encode([{"Config": "config.json", "RepoTags": [], "Layers": []}])
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for filename, contents in (("config.json", config), ("manifest.json", manifest)):
            entry = tarfile.TarInfo(filename)
            entry.mode = 0o600
            entry.size = len(contents)
            archive.addfile(entry, io.BytesIO(contents))
    return "sha256:" + digest(config), buffer.getvalue()


def tool(name):
    path = shutil.which(name, path=ENV["PATH"])
    require(path is not None, name + " unavailable")
    return path


def invoke(step, *argv, payload=None):
    with subprocess.Popen(argv, env=ENV, stdin=subprocess.PIPE if payload is not None else subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as child:
        try:
            output, _ = child.communicate(payload, timeout=6)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate(timeout=2)
            raise CheckFailed(step + " timed out") from None
    require(child.returncode == 0 and len(output) <= 16384, step + " failed")
    return output.decode("ascii", errors="strict").strip()


def start_child(stack, *argv):
    child = subprocess.Popen(argv, env=ENV, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop():
        if child.poll() is not None:
            return
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=2)

    stack.callback(stop)
    return child


def private_net(pid):
    interfaces = {
        row.split(":", 1)[0].strip()
        for row in Path(f"/proc/{pid}/net/dev").read_text(encoding="ascii").splitlines()
        if ":" in row
    }
    listeners = sum(
        row.split()[3] == "0A"
        for protocol in ("tcp", "tcp6")
        for row in Path(f"/proc/{pid}/net/{protocol}").read_text(encoding="ascii").splitlines()[1:]
    )
    return interfaces == {"lo"} and listeners == 0


def ready(child, path, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        require(child.poll() is None, "fixture daemon exited")
        if path.is_socket():
            with socket.socket(socket.AF_UNIX) as conn:
                conn.settimeout(0.5)
                try:
                    conn.connect(str(path))
                except (ConnectionRefusedError, FileNotFoundError):
                    continue
                pid, uid, _ = struct.unpack("3i", conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                require((pid, uid) == (child.pid, 0), "private socket peer mismatch")
            return
        time.sleep(0.12)
    raise CheckFailed("fixture socket readiness timeout")


def limited_unit(name, store):
    require(sys.platform == "linux" and os.geteuid() == 0 and os.uname().machine == "aarch64",
            "requires a Linux ARM64 root fixture unit")
    for key in ("mnt", "net"):
        require(os.readlink(f"/proc/self/ns/{key}") != os.readlink(f"/proc/1/ns/{key}"),
                key + " namespace not isolated")
    require(store.is_mount() and store.stat().st_mode & 0o777 == 0o700,
            "private fixture tmpfs unavailable")
    rootfs = os.statvfs(store)
    require(0 < rootfs.f_blocks * rootfs.f_frsize <= 128 * 1024**2 and 0 < rootfs.f_files <= 4096,
            "tmpfs limits not enforced")
    scope = Path("/sys/fs/cgroup/system.slice") / f"{name}.service"
    path = Path("/proc/self/cgroup").read_text(encoding="ascii").strip()
    require(path == "0::/" + str(scope.relative_to("/sys/fs/cgroup")),
            "cgroup membership mismatch")
    cpu = (scope / "cpu.max").read_text().split()
    memory = (scope / "memory.max").read_text().strip()
    pids = (scope / "pids.max").read_text().strip()
    require(cpu[0].isdigit() and int(cpu[0]) <= int(cpu[1]) and memory.isdigit()
            and 0 < int(memory) <= 512 * 1024**2 and pids.isdigit() and 0 < int(pids) <= 128,
            "fixture cgroup limits not enforced")
    return scope, {"cpu_cores": 1, "cgroup_memory_bytes": int(memory),
                   "pids": int(pids), "storage_bytes": rootfs.f_blocks * rootfs.f_frsize,
                   "storage_inodes": rootfs.f_files, "runtime_seconds": 120}


def restrict_probe(path):
    probe = """import os, socket, sys
from pathlib import Path
import ctypes
os.setgroups([])
os.setgid(65534)
os.setuid(65534)
libc=ctypes.CDLL(None,use_errno=True)
if libc.prctl(38,1,0,0,0)!=0 or os.getgroups() or os.geteuid()==0:
    raise SystemExit(3)
status={line.split(':',1)[0]:line.split(':',1)[1].strip()
        for line in Path('/proc/self/status').read_text().splitlines() if ':' in line}
if int(status['CapEff'],16)!=0 or status['NoNewPrivs']!='1':
    raise SystemExit(4)
with socket.socket(socket.AF_UNIX) as client:
    client.settimeout(1)
    try:
        client.connect(sys.argv[1])
    except PermissionError:
        raise SystemExit(0)
    raise SystemExit(5)
"""
    invoke("unprivileged socket test", sys.executable, "-IB", "-c", probe, str(path))


def stage(name, root):
    store = root / "store"
    scope, limits = limited_unit(name, store)
    require(not Path("/run/docker.sock").is_socket() and not Path("/run/containerd/containerd.sock").is_socket(),
            "an existing service socket is visible")
    require(not any(store.iterdir()), "fixture storage must start empty")
    project = store / "project"
    project.mkdir(mode=0o700)
    owner = digest(os.fsencode(project.resolve(strict=True)))
    image_id, image_tar = archive_for(name, owner)

    config = store / "config"
    config.mkdir(mode=0o700)
    conf = store / "engine.json"
    conf.write_text('{"features":{"containerd-snapshotter":false}}', encoding="ascii")
    ctr_conf = store / "containerd.toml"
    ctr_conf.write_text('version = 2\n', encoding="ascii")
    runtime_sock, engine_sock = store / "containerd.sock", store / "docker.sock"
    engine_root = store / "docker-data"
    cli = (tool("docker"), "--config", str(config), "--host", f"unix://{engine_sock}")

    def docker(step, *arguments, payload=None):
        return invoke(step, *cli, *arguments, payload=payload)

    def inventory():
        rows = docker("engine identity", "info", "--format",
                      "{{.ID}} {{.DockerRootDir}} {{.Containers}} {{.Images}} {{.Driver}}").split()
        require(len(rows) == 5 and ENGINE.fullmatch(rows[0]) and rows[1] == str(engine_root)
                and rows[4] == "vfs" and rows[2].isdigit() and rows[3].isdigit(),
                "private engine observation invalid")
        ids = docker("image inventory", "image", "ls", "-aq", "--no-trunc").splitlines()
        require(len(ids) == int(rows[3]), "image inventory incomplete")
        cache = docker("builder cache inventory", "system", "df", "--format", CACHE)
        volumes = docker("volume inventory", "volume", "ls", "-q")
        require(cache == "0 0" and not volumes and rows[2] == "0", "fixture contains unexpected resources")
        return rows[0], ids

    with ExitStack() as stack:
        runtime = start_child(stack, tool("containerd"), "--config", ctr_conf, "--root", store / "ctr",
                              "--state", store / "ctr-state", "--address", runtime_sock, "--log-level", "error")
        ready(runtime, runtime_sock, 8)
        engine = start_child(stack, tool("dockerd"), "--config-file", conf, "--containerd", runtime_sock,
                             "--data-root", engine_root, "--exec-root", store / "exec", "--pidfile",
                             store / "dockerd.pid", "--host", f"unix://{engine_sock}", "--group", "root",
                             "--log-level", "error", "--storage-driver", "vfs", "--bridge", "none",
                             "--iptables=false", "--ip6tables=false", "--ip-forward=false",
                             "--ip-masq=false", "--userland-proxy=false", "--shutdown-timeout", "4")
        ready(engine, engine_sock, 12)
        require(private_net(engine.pid) and engine_sock.stat().st_uid == 0
                and not engine_sock.stat().st_mode & stat.S_IRWXO, "engine socket or network gate failed")
        restrict_probe(engine_sock)
        engine_id, earlier = inventory()
        require(not earlier, "initial image inventory is not empty")
        reply = docker("load synthetic image", "image", "load", payload=image_tar)
        require(reply == "Loaded image ID: " + image_id, "loaded ID differs from the source configuration")
        after_engine_id, after = inventory()
        require(after_engine_id == engine_id and after == [image_id], "exactly one image expected after load")
        fmt = ('{{.ID}} {{.Os}} {{.Architecture}} {{len .RepoTags}} {{len .RepoDigests}} '
               '{{len .RootFS.Layers}} {{.Size}} {{eq (index .Config.Labels "' + OWNER_LABEL + '") "' + owner + '"}}')
        report = docker("project-owned image projection", "image", "inspect", "--format", fmt, "--", image_id)
        require(report.split() == [image_id, "linux", "arm64", "0", "0", "0", "0", "true"],
                "image fingerprint or references mismatch")
        version = docker("server release identity", "version", "--format", "{{.Server.Version}}")
        require(re.fullmatch(r"[0-9A-Za-z.+-]{1,64}", version) is not None, "unexpected server release identity")
        sock = engine_sock.stat()
        process_start = Path(f"/proc/{engine.pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
        boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        endpoint = digest(encode([name, engine_id, str(engine_root)]))
        generation = digest(encode([endpoint, boot, engine.pid, process_start, sock.st_dev, sock.st_ino]))
        proof = {
            "trial": name, "engine_id": engine_id, "server_version": version, "endpoint": endpoint,
            "generation": generation, "engine_pid": engine.pid, "image_id": image_id,
            "author_fingerprint": owner, "archive_sha256": digest(image_tar), "archive_bytes": len(image_tar),
            "snapshot": digest(encode([image_id, report])), "before_images": 0, "after_images": 1,
            "containers": 0, "volumes": 0, "cache": 0, "tcp_listeners": 0, "restricted_access_denied": True,
            "delete_requests": 0, "limits": limits,
            "observed_memory_peak_bytes": int((scope / "memory.peak").read_text().strip()),
            "expiry": "supervised trial lifetime or changed engine generation",
        }
        (root / "proof.tmp").write_bytes(encode(proof))
        (root / "proof.tmp").replace(root / "proof.json")
        deadline = time.monotonic() + limits["runtime_seconds"]
        while time.monotonic() < deadline and engine.poll() is None and runtime.poll() is None:
            time.sleep(0.25)


def main():
    if len(sys.argv) != 2 or NAME.fullmatch(sys.argv[1]) is None:
        raise SystemExit("a private trial service identity is required")
    name = sys.argv[1]
    root = Path("/run") / name
    require(root.is_dir() and not root.is_symlink() and root.stat().st_uid == 0
            and root.stat().st_mode & 0o777 == 0o700, "private service directory unavailable")

    def stop(*_):
        raise SystemExit(0)

    signal.signal(signal.SIGTERM, stop)
    try:
        stage(name, root)
    except (CheckFailed, OSError, ValueError) as exc:
        reason = str(exc) if isinstance(exc, CheckFailed) else type(exc).__name__
        (root / "failure.txt").write_text(reason, encoding="ascii")
        time.sleep(3)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
