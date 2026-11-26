"""Qualify the archive-only Moby image route in a bounded, private ARM64 guest unit."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, closing, contextmanager
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import secrets
import select
import shutil
import signal
import socket
import sqlite3
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import threading
import time


def load_fixture_helpers():
    path = Path(__file__).with_name("check-dedicated-docker-engine.py")
    spec = importlib.util.spec_from_file_location("dsg_private_engine_fixture", path)
    helpers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helpers)
    return helpers


fx = load_fixture_helpers()
check = fx.require
encode = fx.encode
run = fx.invoke
ENV = fx.ENV


def guard(scratch):
    check(sys.platform == "linux" and os.getuid() == os.geteuid() == 0
          and os.uname().machine == "aarch64", "Linux ARM64 root required")
    check(scratch.is_absolute() and scratch.parent == Path("/run")
          and scratch.resolve(strict=True) == scratch and scratch.is_dir()
          and scratch.stat().st_uid == 0 and stat.S_IMODE(scratch.stat().st_mode) == 0o700
          and not any(scratch.iterdir()), "empty private scratch required")
    for ns in ("mnt", "net"):
        check(os.readlink("/proc/self/ns/" + ns) != os.readlink("/proc/1/ns/" + ns),
              "private guest namespaces required")
    check(fx.private_net(os.getpid()), "isolated network required")
    path = Path("/proc/self/cgroup").read_text().strip().removeprefix("0::/")
    check(re.fullmatch(r"(?:[\w.@:-]+/)*[\w.@:-]+\.service", path) is not None,
          "dedicated service cgroup required")
    cg = Path("/sys/fs/cgroup") / path
    quota, period = (cg / "cpu.max").read_text().split()
    memory, pids = [(cg / file).read_text().strip() for file in ("memory.max", "pids.max")]
    check(quota.isdigit() and 0 < int(quota) <= int(period) and memory.isdigit()
          and 0 < int(memory) <= 1024**3 and pids.isdigit() and 0 < int(pids) <= 192,
          "resource limits required")
    for path in ("/run/docker.sock", "/run/containerd/containerd.sock"):
        check(not Path(path).exists(), "standard service sockets must be absent")
    return cg


@contextmanager
def fixture_mount(scratch):
    image = scratch / "store.ext4"
    with image.open("xb") as f:
        f.truncate(64 * 1024**2)
    run("private propagation", fx.tool("mount"), "--make-rprivate", "/")
    run("format fresh image", fx.tool("mkfs.ext4"), "-q", "-F", "-b", "4096", "-N", "4096", image)
    mount = scratch / "store"
    mount.mkdir(mode=0o700)
    run("mount fresh image", fx.tool("mount"), "-o", "loop,nodev,nosuid,noexec", image, mount)
    try:
        mount.chmod(0o700)
        check(mount.is_mount() and os.major(mount.stat().st_dev) == 7, "private loop required")
        yield mount
    finally:
        run("unmount owned fixture", fx.tool("umount"), mount)
        check(not mount.is_mount(), "fixture mount remained")
        deadline = time.monotonic() + 3
        while run("loop detach check", fx.tool("losetup"), "-j", image):
            check(time.monotonic() < deadline, "fixture loop remained")
            time.sleep(0.1)


def start(stack, args, **options):
    defaults = dict(env=ENV, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL, start_new_session=True)
    defaults.update(options)
    p = subprocess.Popen(args, **defaults)

    def stop():
        if p.poll() is None:
            p.terminate()
            try:
                p.wait(8)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(3)
        for stream in (p.stdin, p.stdout):
            if stream is not None:
                stream.close()

    stack.callback(stop)
    return p


def archive_with_layers(name, owner):
    def tar(entries):
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w", format=tarfile.USTAR_FORMAT) as handle:
            for entry, data in entries:
                info = tarfile.TarInfo(entry)
                info.mode = 0o600
                info.size = len(data)
                handle.addfile(info, io.BytesIO(data))
        return out.getvalue()

    layers = [(filename + ".tar", tar([(filename, hashlib.sha256(filename.encode()).digest() * 256)]))
              for filename in ("dsg-shared", "dsg-" + name)]
    config = encode({
        "architecture": "arm64", "os": "linux",
        "config": {"Labels": {fx.OWNER_LABEL: owner, "io.dsg.fixture": name}},
        "rootfs": {"type": "layers", "diff_ids": ["sha256:" + fx.digest(layer) for _, layer in layers]},
    })
    manifest = encode([{"Config": "config.json", "RepoTags": [], "Layers": [path for path, _ in layers]}])
    return "sha256:" + fx.digest(config), tar([("config.json", config), ("manifest.json", manifest), *layers])


def layer_projection(root):
    layers = sorted(root.iterdir())
    check(len(layers) <= 3, "unexpected layered fixture count")
    result = {}
    for layer in layers:
        check(layer.is_dir() and not layer.is_symlink(), "invalid layered fixture directory")
        files = sorted(layer.iterdir())
        check(1 <= len(files) <= 2, "unexpected fixture payload count")
        content = {}
        for path in files:
            check(path.name in {"dsg-shared", "dsg-selected", "dsg-protected-peer"}
                  and path.is_file() and not path.is_symlink() and path.stat().st_size == 8192,
                  "unexpected layered fixture payload")
            info = path.stat()
            content[path.name] = (info.st_ino, info.st_mtime_ns, file_digest(path))
        result[layer.name] = content
    return result


def seed(scratch, mount, project, cgroup, patched, layered):
    store = mount / "docker-data"
    conf = scratch / "engine.json"
    conf.write_text('{"features":{"containerd-snapshotter":false}}')
    ctr_conf = scratch / "containerd.toml"
    ctr_conf.write_text("version = 2\n")
    config = scratch / "docker-cli"
    config.mkdir(0o700)
    engine_sock, ctr_sock = scratch / "stock.sock", scratch / "ctr.sock"
    stock_args = [patched, "--config-file", conf, "--containerd", ctr_sock, "--data-root", store,
                  "--exec-root", scratch / "exec", "--pidfile", scratch / "stock.pid",
                  "--host", f"unix://{engine_sock}", "--group", "root", "--log-level", "error",
                  "--storage-driver", "vfs", "--bridge", "none", "--iptables=false",
                  "--ip6tables=false", "--ip-forward=false", "--ip-masq=false",
                  "--userland-proxy=false", "--shutdown-timeout", "4"]
    members = lambda: set((cgroup / "cgroup.procs").read_text().split())
    before = members()
    cli = [fx.tool("docker"), "--config", config, "--host", f"unix://{engine_sock}"]
    owner = fx.digest(os.fsencode(project))
    with ExitStack() as children:
        runtime = start(children, [fx.tool("containerd"), "--config", ctr_conf,
                                    "--root", scratch / "ctr", "--state", scratch / "ctr-state",
                                    "--address", ctr_sock, "--log-level", "error"])
        fx.ready(runtime, ctr_sock, 12)
        engine = start(children, [fx.tool("dockerd"), *stock_args[1:]])
        fx.ready(engine, engine_sock, 18)
        version = run("stock version", *cli, "version", "--format", "{{.Server.Version}}")
        check(version == "29.1.3", "stock engine version mismatch")
        info = run("stock scope", *cli, "info", "--format",
                   "{{.DockerRootDir}} {{.Driver}} {{.Containers}} {{.Images}}")
        check(info == f"{store} vfs 0 0", "stock root or inventory mismatch")
        ids = []
        generate = archive_with_layers if layered else fx.archive_for
        for name in ("selected", "protected-peer"):
            ident, archive = generate(name, owner)
            reply = run("load native classic fixture", *cli, "image", "load", payload=archive)
            check(reply == "Loaded image ID: " + ident, "seed did not match native image ID")
            ids.append(ident)
        actual = run("stock inventory", *cli, "image", "ls", "-aq", "--no-trunc")
        check(sorted(actual.splitlines()) == sorted(ids), "stock inventory differed")
        # A clean stock shutdown is required before handoff to the sealed store.
        for p in (engine, runtime):
            p.terminate()
            check(p.wait(12) == 0, "stock engine or runtime shutdown failed")
    check(members() == before, "stock child remained in unit")
    return store, owner, ids, config, stock_args


def marker(path, payload):
    with path.open("xb") as f:
        f.write(payload)
        os.fchmod(f.fileno(), 0o444)
        os.fsync(f.fileno())
    d = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(d)
    finally:
        os.close(d)


def file_digest(path):
    with path.open("rb") as f:
        h = hashlib.sha256()
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
        return h.hexdigest()


def peer(process, path, uid, executable, fingerprint):
    check(process.poll() is None, "peer process exited")
    pid = process.pid
    stat_fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return dict(socket=str(path), pid=pid, uid=uid, start_ticks=int(stat_fields[19]),
                executable=str(executable), executable_sha256=fingerprint)


def role(stack, dsg, control, path, uid):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(str(path))
        path.chmod(0o600)
        fd = listener.fileno()
        p = start(stack, [dsg, "docker-image-session", "--control", control,
                          "--probe-fd", str(fd)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                  pass_fds=(fd,), user=uid, group=uid, extra_groups=[])
    deadline = time.monotonic() + 8
    while p.poll() is None and time.monotonic() < deadline:
        with socket.socket(socket.AF_UNIX) as probe:
            try:
                probe.connect(str(path))
            except ConnectionRefusedError:
                time.sleep(0.05)
                continue
            creds = probe.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
            check(struct.unpack("3i", creds)[:2] == (p.pid, uid), "role peer identity differed")
            return p
    raise fx.CheckFailed("role probe unavailable")


def line(process, step, timeout=25):
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    check(readable, step + " timed out")
    data = process.stdout.readline(513)
    check(0 < len(data) <= 512 and data.endswith(b"\n"), step + " bounded response missing")
    return json.loads(data)


def request(session, project, operation, **selector):
    msg = {"protocol_version": 1, "request": {"operation": operation,
            "project_id": project, **selector}}
    session.stdin.write(encode(msg) + b"\n")
    session.stdin.flush()
    return line(session, operation)


def reject_engine(socket_path, packet):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(12)
        connection.connect(str(socket_path))
        try:
            connection.sendall(packet)
            connection.shutdown(socket.SHUT_WR)
            return connection.recv(1) == b""
        except (BrokenPipeError, ConnectionResetError):
            return True


def rejection(command, timeout=12):
    result = subprocess.run(command, env=ENV, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout, check=False)
    return result.returncode != 0


def assert_sealed_start(command):
    # The stock runtime has stopped. Only the explicit marker error proves that
    # the guard, rather than a missing dependency, refused the standard path.
    with subprocess.Popen(command, env=ENV, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.PIPE) as trial:
        try:
            counted = subprocess.run([fx.tool("grep"), "-Fc",
                                      "dedicated DSG image storage cannot start the standard daemon"],
                                     stdin=trial.stderr, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, timeout=6)
            code = trial.wait(3)
        finally:
            if trial.poll() is None:
                trial.kill()
                trial.wait(3)
    check(code != 0 and counted.returncode == 0 and counted.stdout.strip().isdigit()
          and int(counted.stdout) >= 1, "standard daemon seal was not verified")


def sealed_trial(args, scratch, mount, project, cgroup, public, stack):
    state = scratch / "state"
    state.mkdir(0o700)
    dsg = public / "dsg"
    shutil.copyfile(args.dsg, dsg)
    dsg.chmod(0o755)
    project_id = json.loads(run("register synthetic project", dsg, "--json", "--root", project,
                                "--state-dir", state, "init"))["result"]["project"]["id"]
    store, owner, (target, other), config, stock_args = seed(
        scratch, mount, project, cgroup, args.engine, args.layered)
    store.chmod(0o700)
    store_id = secrets.token_hex(32)
    marker(mount / "dsg-dedicated-image-store", b"classic-vfs-archive-v1\n")
    marker(store / "docker-backend-store-id", (store_id + "\n").encode())
    control, probe, native = public / "c", scratch / "p", scratch / "native"
    policy = scratch / "docker-controller-admission.json"
    controller = start(stack, [dsg, "--state-dir", state, "docker-image-serve", "--policy", policy,
                               "--control", control, "--probe", probe, "--store-directory", store],
                       stdout=subprocess.PIPE)
    fd = line(controller, "controller startup")["store_fd"]
    check(isinstance(fd, int) and fd >= 3, "controller store descriptor missing")
    dsg_sum, engine_sum = file_digest(dsg), file_digest(args.engine)
    engine = start(stack, [args.engine, "--dsg-image-store", store, native, str(controller.pid), dsg_sum])
    fx.ready(engine, native, 15)
    assert_sealed_start(stock_args)
    policies = {"version": 1, "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
                "controller": peer(controller, probe, 0, dsg, dsg_sum),
                "engine": peer(engine, native, 0, args.engine, engine_sum)}
    sessions = {}
    for name, uid in (("approver", 61131), ("executor", 61132)):
        path = scratch / name
        sessions[name] = role(stack, dsg, control, path, uid)
        policies[name] = peer(sessions[name], path, uid, dsg, dsg_sum)
    packed = encode(policies)
    marker(policy, packed)
    marker(scratch / "docker-backend-ownership.json", encode({
        "version": 1, "admission_policy_sha256": fx.digest(packed),
        "controller_executable_sha256": dsg_sum, "engine_executable_sha256": engine_sum,
        "store_directory": str(store), "store_id": store_id, "controller_store_fd": fd,
        "resources": [{"project_id": project_id, "project_owner_sha256": owner, "kind": "image", "id": target}],
    }))
    approver, executor = sessions["approver"], sessions["executor"]
    content = store / "image/vfs/imagedb/content/sha256"
    peer_file = content / other[7:]
    peer_stat = peer_file.stat()
    initial_layers = layer_projection(store / "vfs/dir") if args.layered else {}
    target_layers = [layer for layer, files in initial_layers.items() if "dsg-selected" in files]
    if args.layered:
        check(len(initial_layers) == 3 and len(target_layers) == 1 and
              sum("dsg-shared" in files for files in initial_layers.values()) == 3,
              "shared and target layer baseline differed")
    check(sorted(p.name for p in content.iterdir()) == sorted([target[7:], other[7:]])
          and file_digest(peer_file) == other[7:], "pre-apply content differed")
    check(request(executor, project_id, "plan", image_id=target)["status"] == "error",
          "executor admitted as approver")
    check(request(approver, project_id, "plan", image_id=other)["status"] == "preserved",
          "unassigned image admitted")
    plan = request(approver, project_id, "plan", image_id=target)
    check(plan.get("status") == "planned", "owned native plan failed")
    pid = plan["plan_id"]
    approved = request(approver, project_id, "approve", plan_id=pid)
    check(approved.get("status") == "approved", "native approval failed")
    intent = approved["intent_id"]
    check(request(approver, project_id, "apply", plan_id=pid)["status"] == "error",
          "approver admitted as executor")
    body = encode({"Image": target, "Cmd": ["true"]})
    packets = [f"POST /v1.52/images/{target}/tag?repo=dsg-race HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\n\r\n".encode(),
               f"POST /v1.52/containers/create HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body]
    cli = [fx.tool("docker"), "--config", config, "--host", "unix://" + str(native)]
    # Gate all three workers on one event and overlap the two client routes with application.
    gate = threading.Barrier(3, timeout=5)

    def competitor(argv, packet):
        gate.wait()
        return reject_engine(native, packet) and rejection([*cli, *argv])

    with ThreadPoolExecutor(max_workers=2) as all_clients:
        tag = all_clients.submit(competitor, ["tag", target, "dsg-race:local"], packets[0])
        create = all_clients.submit(competitor, ["create", "--pull", "never", target, "true"], packets[1])
        gate.wait()
        applied = request(executor, project_id, "apply", plan_id=pid)
        check(tag.result(20) and create.result(20), "competing writer accepted")
    if args.expect_recovery:
        check(applied.get("status") == "recovery_required", "commit response loss not observed")
        applied = request(executor, project_id, "recover", intent_id=intent)
    check(applied.get("status") == "applied" and applied.get("reclaimed_bytes") == 0,
          "single native apply did not settle")
    receipts = mount / "dsg-image-receipts"
    expected = {target[7:] + ".attempt", intent + ".receipt"}
    check({p.name for p in receipts.iterdir()} == expected, "single durable receipt missing")
    record = json.loads((receipts / (intent + ".receipt")).read_bytes())
    check(record["intent"]["binding"]["id"] == target and record["intent"]["operation"] == intent
          and record["reclaimed_bytes"] == 0, "receipt binding differed")
    before = {p.name: (p.stat().st_ino, p.stat().st_mtime_ns, file_digest(p)) for p in receipts.iterdir()}
    check(request(executor, project_id, "apply", plan_id=pid)["status"] == "preserved",
          "duplicate approval allowed")
    check({p.name: (p.stat().st_ino, p.stat().st_mtime_ns, file_digest(p))
           for p in receipts.iterdir()} == before, "duplicate altered the native ledger")
    check([p.name for p in content.iterdir()] == [other[7:]] and file_digest(peer_file) == other[7:]
          and os.path.samestat(peer_stat, peer_file.stat())
          and peer_stat.st_mtime_ns == peer_file.stat().st_mtime_ns, "peer image changed")
    check(not any((store / "containers").iterdir()), "competing container created")
    check(json.loads((store / "image/vfs/repositories.json").read_bytes()) == {"Repositories": {}},
          "competing tag created")
    if args.layered:
        check(layer_projection(store / "vfs/dir") ==
              {layer: files for layer, files in initial_layers.items() if layer not in target_layers},
              "shared or non-target layer changed")
    uri = (state / "registry.sqlite3").as_uri() + "?mode=ro"
    with closing(sqlite3.connect(uri, uri=True)) as db:
        row = db.execute("SELECT p.status,a.claimed_at_ms IS NOT NULL,a.bytes_applied "
                         "FROM gc_plan p JOIN gc_application a ON a.plan_id=p.id WHERE p.id=?", (pid,)).fetchone()
        check(row == ("applied", 1, 0), "approval consumption or settled bytes differed")
    return {"single_delete": 1, "native_attempts": 1, "native_receipts": 1, "images_before": 2,
            "images_after": 1, "peer_preserved": True, "reclaimed_bytes": 0, "wrong_role_denied": 2,
            "duplicate_mutations": 0, "tag_denied": True, "container_create_denied": True,
            "receipt_recovery": args.expect_recovery, "registered_project": project_id,
            "exclusive_layers_removed": len(target_layers), "shared_layers_preserved": args.layered}


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--scratch", required=True, type=Path)
    cli.add_argument("--engine", required=True, type=Path)
    cli.add_argument("--dsg", required=True, type=Path)
    cli.add_argument("--expect-recovery", action="store_true")
    cli.add_argument("--layered", action="store_true")
    args = cli.parse_args()
    os.umask(0o077)
    scratch = args.scratch
    cgroup = guard(scratch)
    initial_members = set((cgroup / "cgroup.procs").read_text().split())
    for binary in (args.dsg, args.engine):
        check(binary.is_absolute() and binary.resolve(strict=True) == binary and binary.is_file()
              and binary.stat().st_uid == 0 and not binary.stat().st_mode & 0o022
              and 0 < binary.stat().st_size <= 128 << 20, "protected pinned binaries required")
    signal.signal(signal.SIGALRM, lambda *_: (_ for _ in ()).throw(fx.CheckFailed("trial deadline")))
    signal.alarm(160)
    public = Path(tempfile.mkdtemp(prefix="dsg-role-", dir="/run"))
    public_mounted = False
    try:
        run("mount private role executables", fx.tool("mount"), "-t", "tmpfs", "-o",
            "mode=0755,exec,nosuid,nodev,size=16M", "tmpfs", public)
        public_mounted = True
        public.chmod(0o755)
        project = scratch / "project"
        project.mkdir(0o700)
        with fixture_mount(scratch) as mount, ExitStack() as children:
            result = sealed_trial(args, scratch, mount, project, cgroup, public, children)
        check(set((cgroup / "cgroup.procs").read_text().split()) == initial_members,
              "fixture child remains after cleanup")
        (scratch / "store.ext4").unlink()
        result.update({"loop_detached": True, "child_processes_remaining": 0})
        print(json.dumps(result, sort_keys=True), flush=True)
    finally:
        # Only the root-owned, unique control socket and copied executable live here.
        for name in ("c", "dsg"):
            (public / name).unlink(missing_ok=True)
        if public_mounted:
            run("unmount private role executables", fx.tool("umount"), public)
        public.rmdir()
        signal.alarm(0)


if __name__ == "__main__":
    try:
        main()
    except fx.CheckFailed as error:
        print(json.dumps({"status": "failed", "stage": str(error)}))
        sys.exit(1)
    except Exception as error:
        # OS/CLI errors may carry raw paths or messages. Emit only a bounded stage.
        import traceback
        frames = traceback.extract_tb(error.__traceback__)
        relevant = [frame for frame in frames if Path(frame.filename).name in
                    {"check-moby-image-apply.py", "check-dedicated-docker-engine.py"}]
        print(json.dumps({"status": "failed", "type": type(error).__name__,
                          "errno": getattr(error, "errno", None),
                          "function": [frame.name + ":" + str(frame.lineno) for frame in relevant[-4:]],
                          "line": frames[-1].lineno}))
        sys.exit(1)
