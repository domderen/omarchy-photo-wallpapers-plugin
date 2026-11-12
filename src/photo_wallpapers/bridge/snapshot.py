import sqlite3
import tempfile
from pathlib import Path

from ..io import owned_regular, private_dir


def capture_database(source: Path, destination: Path) -> Path:
    original = owned_regular(source)
    for suffix in ("-wal", "-shm"):
        owned_regular(Path(str(source) + suffix), missing_ok=True)
    private_dir(destination.parent)
    owned_regular(destination, missing_ok=True)
    with tempfile.NamedTemporaryFile(
        dir=destination.parent, suffix=".sqlite", delete=False
    ) as temp:
        temporary = Path(temp.name)
    try:
        with (
            sqlite3.connect(
                source.absolute().as_uri() + "?mode=ro", uri=True, timeout=30
            ) as db,
            sqlite3.connect(temporary) as copy,
        ):
            db.backup(copy, pages=1024, sleep=0.05)
            if copy.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise ValueError("Photos snapshot integrity failed")
        current = owned_regular(source)
        if (current.st_dev, current.st_ino) != (original.st_dev, original.st_ino):
            raise ValueError("photos_database_replaced_during_snapshot")
        owned_regular(destination, missing_ok=True)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)
    return destination
