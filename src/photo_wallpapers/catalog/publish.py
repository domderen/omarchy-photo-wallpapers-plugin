"""Canonical content identities and validated, immutable SQLite generations."""

import hashlib
import json
import sqlite3
import tempfile
from dataclasses import asdict
from datetime import UTC, datetime
from pathlib import Path

from ..io import atomic_json, locked, private_dir
from .models import Asset, CatalogSnapshot, Collection, Face, Person

SCHEMA_VERSION = 1


def canonical(snapshot):
    data = asdict(snapshot)
    data.pop("enrichment", None)  # Separate v2 metadata must not change the v1 catalog.
    for key in (
        "assets",
        "people",
        "collections",
        "photo_people",
        "photo_albums",
        "faces",
    ):
        data[key] = sorted(
            data[key], key=lambda v: json.dumps(v, sort_keys=True, ensure_ascii=False)
        )
    return json.dumps(
        data, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode()


def generation(snapshot):
    return hashlib.sha256(canonical(snapshot)).hexdigest()


def write_catalog(snapshot, path, content_id):
    if not snapshot.namespace:
        raise ValueError("missing namespace")
    for asset in snapshot.assets:
        if (
            asset.width <= 0
            or asset.height <= 0
            or asset.orientation not in range(1, 9)
        ):
            raise ValueError("invalid asset geometry")
    with sqlite3.connect(path) as db:
        db.executescript(Path(__file__).with_name("schema.sql").read_text())
        metadata = {
            "schema_version": SCHEMA_VERSION,
            "generation": content_id,
            "namespace": snapshot.namespace,
            "capabilities": snapshot.capabilities,
            "exported_at": datetime.now(UTC).isoformat(),
        }
        db.executemany(
            "INSERT INTO catalog_metadata VALUES (?,?)",
            [(k, json.dumps(v)) for k, v in metadata.items()],
        )
        for table, records in [
            ("photos", snapshot.assets),
            ("people", snapshot.people),
            ("albums", snapshot.collections),
        ]:
            db.executemany(
                f"INSERT INTO {table} VALUES (?,?)",
                [
                    (r.id, json.dumps(asdict(r), ensure_ascii=False, sort_keys=True))
                    for r in sorted(records, key=lambda r: r.id)
                ],
            )
        db.executemany(
            "INSERT INTO photo_people VALUES (?,?)", sorted(set(snapshot.photo_people))
        )
        db.executemany(
            "INSERT INTO photo_albums VALUES (?,?)", sorted(set(snapshot.photo_albums))
        )
        db.executemany(
            "INSERT INTO faces VALUES (?,?,?)",
            [
                (f.asset_id, f.person_id, json.dumps(asdict(f), sort_keys=True))
                for f in snapshot.faces
            ],
        )


def read_catalog(path: Path) -> CatalogSnapshot:
    # immutable avoids sidecar files and ensures this reader cannot write.
    with sqlite3.connect(
        path.resolve().as_uri() + "?mode=ro&immutable=1", uri=True
    ) as db:
        if (
            db.execute("PRAGMA integrity_check").fetchone()[0] != "ok"
            or db.execute("PRAGMA foreign_key_check").fetchone()
        ):
            raise ValueError("catalog integrity failure")
        metadata = {
            k: json.loads(v)
            for k, v in db.execute("SELECT key,value FROM catalog_metadata")
        }
        if metadata["schema_version"] != SCHEMA_VERSION:
            raise ValueError("unsupported schema")

        def records(table, cls):
            values = []
            for (raw,) in db.execute(f"SELECT data FROM {table} ORDER BY id"):
                item = json.loads(raw)
                if cls is Asset:
                    item["keywords"] = tuple(item["keywords"])
                values.append(cls(**item))
            return values

        result = CatalogSnapshot(
            metadata["namespace"],
            records("photos", Asset),
            records("people", Person),
            records("albums", Collection),
            list(db.execute("SELECT * FROM photo_people ORDER BY 1,2")),
            list(db.execute("SELECT * FROM photo_albums ORDER BY 1,2")),
            [
                Face(**json.loads(raw))
                for (raw,) in db.execute("SELECT data FROM faces")
            ],
            metadata["capabilities"],
        )
        if generation(result) != metadata["generation"]:
            raise ValueError("content generation mismatch")
        return result


def publish_catalog(snapshot: CatalogSnapshot, directory: Path) -> dict:
    private_dir(directory)
    content_id = generation(snapshot)
    with locked(directory / ".publish.lock"):
        path = directory / (content_id + ".sqlite")
        reusable = False
        if path.exists():
            try:
                reusable = generation(read_catalog(path)) == content_id
            except (ValueError, sqlite3.DatabaseError):
                pass
        if not reusable:
            with tempfile.NamedTemporaryFile(
                dir=directory, suffix=".sqlite", delete=False
            ) as temp:
                temporary = Path(temp.name)
            try:
                write_catalog(snapshot, temporary, content_id)
                read_catalog(temporary)
                with temporary.open("rb") as handle:
                    import os

                    os.fsync(handle.fileno())
                temporary.replace(path)
            finally:
                temporary.unlink(missing_ok=True)
        data = path.read_bytes()
        manifest = {
            "generation": content_id,
            "schema_version": SCHEMA_VERSION,
            "catalog_url": f"/v1/catalogs/{content_id}.sqlite",
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
        atomic_json(directory / "manifest.json", manifest)
        # Open descriptors remain valid after unlink on both supported platforms.
        previous = sorted(
            directory.glob("*.sqlite"), key=lambda p: p.stat().st_mtime, reverse=True
        )
        keep = {path, *[p for p in previous if p != path][:1]}
        for old in previous:
            if old not in keep:
                old.unlink(missing_ok=True)
        return manifest
