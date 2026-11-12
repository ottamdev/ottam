"""Back up and reset Ottam's SQLite files. Run by scripts/reset-app-data.sh --db-only."""
from __future__ import annotations

import fcntl
import os
import shutil
import stat
import sys
import tempfile
import time

DATABASE_FILES = (
    "ottam.sqlite", "ottam.sqlite-wal", "ottam.sqlite-shm", "ottam.sqlite-journal",
    "payloads.sqlite", "payloads.sqlite-wal", "payloads.sqlite-shm", "payloads.sqlite-journal",
)


class Refusal(Exception):
    pass


def reset(home: str) -> None:
    if os.path.islink(home):
        raise Refusal(f"Refusing symlinked parent: {home}")
    if not os.path.exists(home):
        print("No Ottam database to reset.")
        return
    # Use the same flock as Core, keeping it held through backup and rollback.
    lock_path = os.path.join(home, "core.lock")
    lock = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(lock).st_mode):
            raise Refusal(f"Refusing non-regular lock: {lock_path}")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refusal("Quit Ottam: its core still holds the database lock.") from None
        move_to_backup(home)
    finally:
        os.close(lock)


def move_to_backup(home: str) -> None:
    files = [os.path.join(home, name) for name in DATABASE_FILES]
    files = [path for path in files if os.path.lexists(path)]
    for path in files:
        if not stat.S_ISREG(os.lstat(path).st_mode):
            raise Refusal(f"Refusing non-regular database file: {path}")
    if not files:
        print("No Ottam database to reset.")
        return
    backups = os.path.join(home, "backups")
    if os.path.islink(backups):
        raise Refusal(f"Refusing symlinked backup directory: {backups}")
    if not os.path.exists(backups):
        os.mkdir(backups, 0o700)
    backup = tempfile.mkdtemp(prefix=f"db-reset-{time.strftime('%Y%m%d-%H%M%S')}-", dir=backups)
    moved = []
    try:
        for source in files:
            destination = os.path.join(backup, os.path.basename(source))
            os.rename(source, destination)
            moved.append((source, destination))
    except BaseException:  # Restore moved files even if the reset is interrupted.
        print(f"Reset incomplete; restoring files from {backup}", file=sys.stderr)
        for source, destination in reversed(moved):
            os.rename(destination, source)
        shutil.rmtree(backup)
        raise
    print(f"Database backup: {backup}")
    print("Ottam database reset. Launch the app to create an empty database; configured agents are preserved.")


def main() -> int:
    try:
        reset(sys.argv[1])
    except (Refusal, OSError) as error:
        print(f"Database reset failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
