"""Observe or apply the single approved live Docker GC trial."""

import hashlib
import http.client
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
import time


VM = "Ubuntu 24.04 P5-001"
TRIAL = "dsg-gc-cf83a703d2fd"
ENGINE = "3f0311f3-c456-43e5-a123-014f500108fd"
GENERATION = "91258d7a9be96b3a41320dc67c09329b0e81fa23d865cd059fffe37ad9296e50"
IMAGE = "sha256:a854697496f4c843431566255d0cb2ae780ebf51f9ca70773445c1f52549f94c"
ROOT = Path("/run") / TRIAL
STORE = ROOT / "store"
SOCKET = STORE / "docker.sock"
GROUP = f"/system.slice/{TRIAL}.service"
CGROUP = Path("/sys/fs/cgroup" + GROUP)
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C"}
HEX = re.compile(r"[0-9a-f]{64}")
API_VERSION = re.compile(r"1\.[0-9]{1,3}")
HOST_FLAGS = ("host_namespace_socket_hidden", "guest_trial_active_before_suspend", "guest_restored_to_suspended")
HOST_FIELDS = (*HOST_FLAGS, "script_sha256")
MAX_JSON = 8192
MAX_CHILD_OUTPUT = 16384
GUEST_SECONDS = 8
ERRORS = frozenset({
    "E_INPUT", "E_BINDING", "E_VM", "E_UNIT", "E_PROOF", "E_ISOLATION", "E_PEER",
    "E_INVENTORY", "E_SNAPSHOT", "E_TIME", "E_DELETE_STATUS", "E_DELETE_RESULT",
    "E_AFTER", "E_TRANSPORT", "E_RUNTIME",
})


class Failed(RuntimeError):
    def __init__(self, code, status=None):
        self.code = code
        self.status = status


def require(condition, code):
    if not condition:
        raise Failed(code)


def encode(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


def digest(value):
    return hashlib.sha256(value).hexdigest()


def unique_fields(pairs):
    value = dict(pairs)
    if len(value) != len(pairs):
        raise ValueError
    return value


def invalid_constant(_):
    raise ValueError


def decode(data, code):
    require(len(data) <= MAX_JSON, code)
    try:
        return json.loads(data.decode("ascii"), object_pairs_hook=unique_fields, parse_constant=invalid_constant)
    except (ValueError, RecursionError):
        raise Failed(code) from None


def fields(value, keys):
    return isinstance(value, dict) and set(value) == set(keys)


def hex_field(value):
    return isinstance(value, str) and HEX.fullmatch(value) is not None


def positive_int(value):
    return type(value) is int and 0 < value < 2**63


def zeros(value, keys):
    return all(type(value.get(key)) is int and value[key] == 0 for key in keys)


def binding(mode, envelope):
    require(mode in ("observe", "apply") and fields(envelope, ("proof", "intent")), "E_INPUT")
    proof, intent = envelope["proof"], envelope["intent"]
    require(isinstance(proof, dict) and (
        proof.get("trial"), proof.get("engine_id"), proof.get("generation"), proof.get("image_id")
    ) == (TRIAL, ENGINE, GENERATION, IMAGE), "E_BINDING")
    require(all(proof.get(flag) is True for flag in HOST_FLAGS) and hex_field(proof.get("script_sha256")), "E_PROOF")
    require(positive_int(proof.get("engine_pid")) and all(
        hex_field(proof.get(key)) for key in ("author_fingerprint", "endpoint", "snapshot", "archive_sha256")
    ) and zeros(proof, ("before_images", "containers", "volumes", "cache", "tcp_listeners", "delete_requests"))
        and type(proof.get("after_images")) is int and proof["after_images"] == 1
        and proof.get("restricted_access_denied") is True, "E_PROOF")
    if mode == "observe":
        require(intent is None, "E_BINDING")
        return proof

    require(fields(intent, ("operation", "policy", "binding"))
            and hex_field(intent["operation"]) and hex_field(intent["policy"]), "E_BINDING")
    target = intent["binding"]
    # The registry verifies its host owner. The private engine's owner is checked in snapshot().
    require(fields(target, ("project", "owner", "kind", "id", "endpoint", "generation", "snapshot"))
            and positive_int(target["project"]) and hex_field(target["owner"])
            and (target["kind"], target["id"], target["endpoint"], target["generation"], target["snapshot"])
            == ("image", IMAGE, proof["endpoint"], GENERATION, proof["snapshot"]), "E_BINDING")
    return proof


def monotonic_us():
    return time.monotonic_ns() // 1000


def read_proof(expected):
    directory = ROOT.lstat()
    require(stat.S_ISDIR(directory.st_mode) and directory.st_uid == 0
            and stat.S_IMODE(directory.st_mode) == 0o700, "E_PROOF")
    fd = os.open(ROOT / "proof.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as proof:
        meta = os.fstat(proof.fileno())
        require(stat.S_ISREG(meta.st_mode) and meta.st_uid == 0 and not meta.st_mode & 0o022, "E_PROOF")
        current = decode(proof.read(MAX_JSON + 1), "E_PROOF")
    guest_fields = {key: value for key, value in expected.items() if key not in HOST_FIELDS}
    require(encode(current) == encode(guest_fields), "E_PROOF")


def run(argv, seconds, code, **kwargs):
    try:
        result = subprocess.run(argv, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=seconds, **kwargs)
    except subprocess.TimeoutExpired:
        raise Failed("E_TIME") from None
    except (OSError, subprocess.SubprocessError):
        raise Failed(code) from None
    require(len(result.stdout) <= MAX_CHILD_OUTPUT, code)
    return result


def command(argv, code, seconds=2):
    result = run(argv, seconds, code, stdin=subprocess.DEVNULL)
    require(result.returncode == 0, code)
    return result.stdout.decode("ascii").strip()


def transport(source, entry, *args):
    return source + ("\nwire_entry(" + entry + ", " + ascii(args) + ")\n").encode("ascii")


def verified_result(mode, proof):
    result = {"verified": True, "engine_id": ENGINE, "generation": GENERATION, "image_id": IMAGE,
              "snapshot": proof["snapshot"], "author_fingerprint": proof["author_fingerprint"], "delete_requests": 0}
    if mode == "apply":
        result.update(delete_requests=1, after_images=0, effect="applied")
    return result


def result_from(result, expected, code):
    value = decode(result.stdout, code)
    if result.returncode == 0:
        require(encode(value) == encode(expected), code)
        return expected
    if isinstance(value, dict) and isinstance(value.get("error"), str) and value["error"] in ERRORS:
        status = value.get("status")
        if value["error"] == "E_DELETE_STATUS" and type(status) is int and 100 <= status <= 599:
            raise Failed(value["error"], status)
        raise Failed(value["error"])
    raise Failed(code)


def unit_state():
    names = ("MainPID", "ActiveState", "SubState", "RuntimeMaxUSec", "ControlGroup",
             "ExecMainStartTimestampMonotonic", "ActiveEnterTimestampMonotonic")
    rows = command(("/usr/bin/systemctl", "show", f"{TRIAL}.service",
                    "--property=" + ",".join(names)), "E_UNIT")
    values = dict(row.split("=", 1) for row in rows.splitlines())
    require(set(values) == set(names) and values["ActiveState"] == "active" and values["SubState"] == "running"
            and values["RuntimeMaxUSec"] == "2min" and values["ControlGroup"] == GROUP, "E_UNIT")
    pid, started, activated = (int(values[key]) for key in
                               ("MainPID", "ExecMainStartTimestampMonotonic", "ActiveEnterTimestampMonotonic"))
    now = monotonic_us()
    require(pid > 1 and 0 < started <= now and 0 < activated <= now, "E_UNIT")
    deadline = min(started, activated) + 120_000_000
    require(deadline - now >= 10_000_000, "E_TIME")
    return pid, deadline


def proc_start(pid):
    return Path(f"/proc/{pid}/stat").read_text(encoding="ascii").rsplit(")", 1)[1].split()[19]


def proc_group(pid):
    return Path(f"/proc/{pid}/cgroup").read_text(encoding="ascii").strip()


def namespaces(pid):
    return tuple(os.readlink(f"/proc/{pid}/ns/{kind}") for kind in ("mnt", "net"))


def join_group():
    with (CGROUP / "cgroup.procs").open("w", encoding="ascii") as membership:
        membership.write(str(os.getpid()))


def guest_outer(mode, envelope, source):
    proof = binding(mode, envelope)
    require(sys.platform == "linux" and os.uname().machine == "aarch64" and os.geteuid() == 0, "E_UNIT")
    pid, deadline = unit_state()
    read_proof(proof)
    start = proc_start(pid)
    target = namespaces(pid)
    require(proc_group(pid) == "0::" + GROUP
            and all(left != right for left, right in zip(target, namespaces(1))), "E_ISOLATION")
    argv = ("/usr/bin/nsenter", "--target", str(pid), "--mount", "--net", "--", "/usr/bin/python3", "-IB", "-")
    payload = transport(source, "guest_inner", mode, envelope, pid, start, target, deadline)
    result = run(argv, GUEST_SECONDS, "E_ISOLATION", input=payload, preexec_fn=join_group)
    return result_from(result, verified_result(mode, proof), "E_ISOLATION")


def empty_network(pid):
    path = Path(f"/proc/{pid}/net")
    ifaces = {row.split(":", 1)[0].strip() for row in
              (path / "dev").read_text(encoding="ascii").splitlines() if ":" in row}
    return ifaces == {"lo"} and all(row.split()[3] != "0A" for protocol in ("tcp", "tcp6")
                                    for row in (path / protocol).read_text(encoding="ascii").splitlines()[1:])


def isolation(proof, pid, start, target):
    require(proc_start(pid) == start and proc_group(pid) == proc_group("self") == "0::" + GROUP
            and namespaces(pid) == namespaces("self") == target
            and all(left != right for left, right in zip(target, namespaces(1))), "E_ISOLATION")
    store = STORE.lstat()
    require(stat.S_ISDIR(store.st_mode) and store.st_uid == 0 and stat.S_IMODE(store.st_mode) == 0o700
            and STORE.is_mount(), "E_ISOLATION")
    storage = os.statvfs(STORE)
    cpu, period = (CGROUP / "cpu.max").read_text(encoding="ascii").split()
    memory, pids = (int((CGROUP / key).read_text(encoding="ascii")) for key in ("memory.max", "pids.max"))
    size = storage.f_blocks * storage.f_frsize
    require(0 < int(cpu) <= int(period) and 0 < memory <= 512 * 1024**2 and 0 < pids <= 128
            and 0 < size <= 128 * 1024**2 and 0 < storage.f_files <= 4096
            and proof.get("limits") == {"cpu_cores": 1, "cgroup_memory_bytes": memory, "pids": pids,
                                        "storage_bytes": size, "storage_inodes": storage.f_files, "runtime_seconds": 120}
            and not Path("/run/docker.sock").is_socket() and not Path("/run/containerd/containerd.sock").is_socket(),
            "E_ISOLATION")


def peer_generation(conn, proof, target):
    pid = proof["engine_pid"]
    reply = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
    actual_pid, uid, _ = struct.unpack("3i", reply)
    require((actual_pid, uid) == (pid, 0) and proc_group(pid) == "0::" + GROUP
            and namespaces(pid) == target and empty_network(pid), "E_PEER")
    sock = SOCKET.lstat()
    require(stat.S_ISSOCK(sock.st_mode) and sock.st_uid == sock.st_gid == 0
            and not sock.st_mode & stat.S_IRWXO, "E_PEER")
    boot = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    endpoint = digest(encode([TRIAL, ENGINE, str(STORE / "docker-data")]))
    generation = digest(encode([endpoint, boot, pid, proc_start(pid), sock.st_dev, sock.st_ino]))
    require(endpoint == proof["endpoint"] and generation == GENERATION, "E_PEER")


def docker_command(*args):
    executable = shutil.which("docker", path=ENV["PATH"])
    require(executable is not None, "E_INVENTORY")
    return command((executable, "--config", str(STORE / "config"), "--host", f"unix://{SOCKET}", *args),
                   "E_INVENTORY")


def inventory(expected, code):
    info = docker_command("info", "--format", "{{.ID}} {{.DockerRootDir}} {{.Containers}} {{.Images}} {{.Driver}}")
    require(info.split() == [ENGINE, str(STORE / "docker-data"), "0", str(len(expected)), "vfs"], code)
    ids = docker_command("image", "ls", "-aq", "--no-trunc").splitlines()
    cache = docker_command("system", "df", "--format", '{{if eq .Type "Build Cache"}}{{.TotalCount}} {{.Active}}{{end}}')
    volumes = docker_command("volume", "ls", "-q")
    require(ids == expected and cache == "0 0" and not volumes, code)


def snapshot(proof):
    project = STORE / "project"
    meta = project.lstat()
    require(stat.S_ISDIR(meta.st_mode) and meta.st_uid == 0 and stat.S_IMODE(meta.st_mode) == 0o700, "E_SNAPSHOT")
    owner = digest(os.fsencode(project.resolve(strict=True)))
    require(owner == proof["author_fingerprint"], "E_SNAPSHOT")
    fmt = ('{{.ID}} {{.Os}} {{.Architecture}} {{len .RepoTags}} {{len .RepoDigests}} '
           '{{len .RootFS.Layers}} {{.Size}} {{eq (index .Config.Labels "io.dsg.gc.owner") "' + owner + '"}}')
    report = docker_command("image", "inspect", "--format", fmt, "--", IMAGE)
    require(report.split() == [IMAGE, "linux", "arm64", "0", "0", "0", "0", "true"]
            and digest(encode([IMAGE, report])) == proof["snapshot"], "E_SNAPSHOT")
    release = docker_command("version", "--format", "{{.Server.Version}} {{.Server.APIVersion}}").split()
    require(len(release) == 2 and release[0] == proof.get("server_version")
            and API_VERSION.fullmatch(release[1]) is not None, "E_SNAPSHOT")
    return release[1]


def delete_once(conn, api_version):
    request = (f"DELETE /v{api_version}/images/{IMAGE}?force=false&noprune=true HTTP/1.1\r\n"
               "Host: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").encode("ascii")
    conn.sendall(request)
    reply = http.client.HTTPResponse(conn, method="DELETE")
    with reply:
        reply.begin()
        if reply.status != 200:
            raise Failed("E_DELETE_STATUS", reply.status)
        payload = decode(reply.read(MAX_JSON + 1), "E_DELETE_RESULT")
    require(payload == [{"Deleted": IMAGE}], "E_DELETE_RESULT")


def guest_inner(mode, envelope, pid, start, target, deadline):
    proof = binding(mode, envelope)
    read_proof(proof)
    isolation(proof, pid, start, target)
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(1.5)
        conn.connect(str(SOCKET))
        peer_generation(conn, proof, target)
        inventory([IMAGE], "E_INVENTORY")
        api_version = snapshot(proof)
        peer_generation(conn, proof, target)
        if mode == "apply":
            require(deadline - monotonic_us() >= 4_000_000 and proc_start(pid) == start, "E_TIME")
            delete_once(conn, api_version)
            inventory([], "E_AFTER")
            peer_generation(conn, proof, target)

    return verified_result(mode, proof)


def timed_out(*_):
    raise Failed("E_TIME")


def error_payload(exc):
    error = {"error": exc.code if isinstance(exc, Failed) else "E_RUNTIME"}
    if isinstance(exc, Failed) and exc.code == "E_DELETE_STATUS" and type(exc.status) is int:
        error["status"] = exc.status
    return error


def wire_entry(entry, args):
    signal.signal(signal.SIGALRM, timed_out)
    signal.alarm(GUEST_SECONDS + 1)
    try:
        print(encode(entry(*args)).decode("ascii"))
    except Exception as exc:
        print(encode(error_payload(exc)).decode("ascii"))
        raise SystemExit(1) from None


def host_main():
    try:
        require(len(sys.argv) == 2 and sys.argv[1] in ("observe", "apply"), "E_INPUT")
        mode = sys.argv[1]
        envelope = decode(sys.stdin.buffer.read(MAX_JSON + 1), "E_INPUT")
        proof = binding(mode, envelope)
        prlctl = next((path for path in ("/usr/local/bin/prlctl",
                     "/Applications/Parallels Desktop.app/Contents/MacOS/prlctl") if os.access(path, os.X_OK)), None)
        require(prlctl is not None, "E_VM")
        running = command((prlctl, "status", VM), "E_VM")
        require(running in (f"VM {VM} exist running", f"VM '{VM}' is running"), "E_VM")
        source = Path(__file__).read_bytes().rsplit(b'\nif __name__ == "__main__":\n', 1)[0]
        payload = transport(source, "guest_outer", mode, envelope, source)
        reply = run((prlctl, "exec", VM, "/usr/bin/python3", "-IB", "-"), 12, "E_TRANSPORT", input=payload)
        result = result_from(reply, verified_result(mode, proof), "E_TRANSPORT")
        print(encode(result).decode("ascii"))
    except Exception as exc:
        print(encode(error_payload(exc)).decode("ascii"), file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    host_main()
