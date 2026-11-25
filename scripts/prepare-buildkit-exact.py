#!/usr/bin/env python3
"""Prepare the DSG cache overlay in a new copy of the pinned official source."""
import argparse
import hashlib
import io
from pathlib import Path, PurePosixPath
import shutil
import tarfile

COMMIT = "412cc86ad3c4252b82e929f9b1b1ad167376ded8"
ARCHIVE_SHA256 = "dce31a3f405045af3406a626b6888dd347103222764c2bf11bbf4286aafed9b8"
ARCHIVE_LIMIT = 16 << 20


def prepare(archive, destination):
    with archive.open("rb") as handle:
        data = handle.read(ARCHIVE_LIMIT + 1)
    if len(data) > ARCHIVE_LIMIT or hashlib.sha256(data).hexdigest() != ARCHIVE_SHA256:
        raise SystemExit("official v0.35.1 source archive digest mismatch")
    # The validated archive bytes are immutable throughout extraction. Create
    # only ordinary source files, and never overlay an existing checkout.
    destination.mkdir(mode=0o700)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for item in tar:
            path = PurePosixPath(item.name)
            if path.is_absolute() or ".." in path.parts or path.parts[0] != f"buildkit-{COMMIT}":
                raise SystemExit("invalid upstream source path")
            if not item.isfile() or path.name == ".env" or ".env." in path.name:
                continue
            if path.suffix.lower() in {".env", ".crt", ".key", ".pem", ".pfx", ".p12", ".der"}:
                continue
            target = destination.joinpath(*path.parts[1:])
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with tar.extractfile(item) as source, target.open("xb") as output:
                shutil.copyfileobj(source, output)
    overlay = Path(__file__).resolve().parents[1] / "integrations/buildkit-exact"
    for package in ("cache", "cmd/buildkitd"):
        for source in sorted((overlay / package).glob("dsg_*.go")):
            with source.open("rb") as handle, (destination / package / source.name).open("xb") as output:
                shutil.copyfileobj(handle, output)
    main = destination / "cmd/buildkitd/main.go"
    source = main.read_text()
    insertions = {
        '\t\t&cli.StringFlag{\n\t\t\tName:  "root",': (
            '\t\t&cli.BoolFlag{Name: "dsg-dedicated-worker", Usage: "own a private native OCI worker root for the daemon lifetime"},\n'
            '\t\t&cli.BoolFlag{Name: "dsg-inspect-dedicated-worker", Usage: "inspect an offline dedicated native worker"},\n'
            '\t\t&cli.BoolFlag{Name: "dsg-require-quiescence", Usage: "require the dedicated Linux process and read-only storage boundary"},\n'
            '\t\t&cli.StringFlag{Name: "dsg-record-id", Usage: "inspect one full native cache record ID offline"},\n'
        ),
        '\t\tlogFormat := cfg.Log.Format': (
            '\t\tif c.Bool("dsg-inspect-dedicated-worker") {\n'
            '\t\t\treturn dsgInspectWorker(c, cfg.Root)\n\t\t}\n'
            '\t\tif c.Bool("dsg-require-quiescence") && !c.Bool("dsg-dedicated-worker") {\n'
            '\t\t\treturn errors.New("dsg-require-quiescence requires a dedicated worker or offline inspection")\n\t\t}\n'
            '\t\tif c.IsSet("dsg-record-id") {\n\t\t\treturn errors.New("dsg-record-id requires offline inspection")\n\t\t}\n'
            '\t\tif err := dsgStartWorker(c, cfg.Root, &cfg); err != nil {\n\t\t\treturn err\n\t\t}\n\n'
        ),
        '\t\t// listeners have to be initialized before the controller': (
            '\t\tif err := dsgRecheckWorker(c, root); err != nil {\n\t\t\treturn err\n\t\t}\n\n'
        ),
    }
    for anchor, insertion in insertions.items():
        if source.count(anchor) != 1:
            raise SystemExit("pinned buildkitd startup anchor mismatch")
        source = source.replace(anchor, insertion + anchor)
    main.write_text(source)
    print(f"prepared official BuildKit {COMMIT} with the DSG worker and cache overlay")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, help="official GitHub codeload tar.gz for commit " + COMMIT)
    parser.add_argument("destination", type=Path, help="new source directory in caller-owned scratch")
    args = parser.parse_args()
    prepare(args.archive, args.destination)
