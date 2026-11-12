import sqlite3
from photo_wallpapers.bridge.snapshot import capture_database


def test_backup_includes_committed_wal(tmp_path):
    source = tmp_path / "source.sqlite"
    with sqlite3.connect(source) as db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("CREATE TABLE test(value)")
        db.execute("INSERT INTO test VALUES (1)")
        db.commit()
        captured = capture_database(source, tmp_path / "snapshot.sqlite")
        with sqlite3.connect(captured) as other:
            assert (
                other.execute("SELECT count(*) FROM test").fetchone()[0]
                == db.execute("SELECT count(*) FROM test").fetchone()[0]
            )
