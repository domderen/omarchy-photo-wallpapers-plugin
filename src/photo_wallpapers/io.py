"""Atomic private storage. Lock files coordinate CLI and daemon processes."""

import fcntl
import json
import os
import stat
import tempfile
from contextlib import contextmanager
from pathlib import Path


def open_directory(path: Path, *, create=False):
    """Walk without following directory links; keep operations on descriptors."""
    parts = path.absolute().parts
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    fd = os.open(parts[0], flags)
    try:
        for part in parts[1:]:
            if part == "..":
                raise ValueError("unsafe_parent_directory")
            if create:
                try:
                    os.mkdir(part, mode=0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            child = os.open(part, flags, dir_fd=fd)
            info = os.fstat(child)
            trusted_temp = info.st_uid == 0 and info.st_mode & stat.S_ISVTX
            if info.st_uid not in (0, os.getuid()) or (
                info.st_mode & 0o022 and not trusted_temp
            ):
                os.close(child)
                raise ValueError("unsafe_private_ancestor")
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def private_dir(path: Path) -> Path:
    # Even chmod to the existing mode emits IN_ATTRIB. Omarchy watches the
    # XDG data directory and rescans applications when its children change.
    fd = open_directory(path, create=True)
    try:
        info = os.fstat(fd)
        if info.st_uid != os.getuid():
            raise ValueError("unowned_private_directory")
        if stat.S_IMODE(info.st_mode) != 0o700:
            os.fchmod(fd, 0o700)
    finally:
        os.close(fd)
    return path


def owned_regular(path: Path, *, missing_ok=False):
    try:
        directory = open_directory(path.parent)
        try:
            info = os.stat(path.name, dir_fd=directory, follow_symlinks=False)
        finally:
            os.close(directory)
    except FileNotFoundError:
        if missing_ok:
            return None
        raise
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError("unsafe_private_file")
    return info


def open_private(path: Path, *, create=False):
    flags = (
        (os.O_RDWR | os.O_CREAT if create else os.O_RDONLY)
        | os.O_NOFOLLOW
        | os.O_NONBLOCK
    )
    directory = open_directory(path.parent)
    try:
        fd = os.open(path.name, flags, 0o600, dir_fd=directory)
    finally:
        os.close(directory)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
            raise ValueError("unsafe_private_file")
        if create and stat.S_IMODE(info.st_mode) != 0o600:
            os.fchmod(fd, 0o600)
        return os.fdopen(fd, "r+b" if create else "rb")
    except BaseException:
        os.close(fd)
        raise


@contextmanager
def locked(path: Path, *, blocking=True):
    private_dir(path.parent)
    with open_private(path, create=True) as handle:
        flags = fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB)
        fcntl.flock(handle, flags)
        yield


def atomic_bytes(path: Path, data: bytes):
    private_dir(path.parent)
    owned_regular(path, missing_ok=True)
    fd, temporary = tempfile.mkstemp(prefix="." + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        owned_regular(path, missing_ok=True)
        os.replace(temporary, path)
        directory = open_directory(path.parent)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        Path(temporary).unlink(missing_ok=True)


def atomic_json(path: Path, data):
    atomic_bytes(path, json.dumps(data, ensure_ascii=False, sort_keys=True).encode())


def read_json(path: Path, default=None, *, limit=64 * 1024**2):
    try:
        handle = open_private(path)
    except FileNotFoundError:
        return default
    with handle:
        if os.fstat(handle.fileno()).st_size > limit:
            raise ValueError("private_json_too_large")
        data = handle.read(limit + 1)
        if len(data) > limit:
            raise ValueError("private_json_too_large")
        return json.loads(data)


def validate_url(url):
    from urllib.parse import urlsplit

    p = urlsplit(url)
    if (
        p.username
        or p.password
        or p.query
        or p.fragment
        or p.path not in ("", "/")
        or not p.hostname
    ):
        raise ValueError(
            "helper URL must be an origin without credentials, path, or query"
        )
    if p.scheme != "https" and not (
        p.scheme == "http" and p.hostname in ("127.0.0.1", "localhost", "::1")
    ):
        raise ValueError("private HTTPS required; HTTP is allowed only on loopback")
    return url.rstrip("/")
