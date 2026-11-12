import argparse
import json
import sys
from dataclasses import asdict
from pathlib import Path

from ..io import atomic_json, read_json
from .auth import ensure_token
from .config import BridgeConfig, default_state, load_config


def main(argv=None):
    if not (3, 13, 18) <= sys.version_info < (3, 14):
        print(
            json.dumps(
                {
                    "error": "unsupported_python",
                    "action": "Use current uv to install managed Python >=3.13.18,<3.14",
                }
            )
        )
        return 1
    parser = argparse.ArgumentParser(
        description="Private read-only Apple Photos bridge"
    )
    parser.add_argument(
        "command",
        choices=[
            "init",
            "refresh",
            "serve",
            "status",
            "pair",
            "install",
            "stop",
            "remove",
        ],
    )
    parser.add_argument("--config", type=Path, default=default_state() / "config.json")
    parser.add_argument("--library", type=Path)
    parser.add_argument(
        "--output", type=Path, help="Private pairing output file; never prints token"
    )
    parser.add_argument("--helper-url")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--purge", action="store_true")
    parser.add_argument(
        "--force", action="store_true", help="Force metadata extraction (refresh only)"
    )
    args = parser.parse_args(argv)
    if args.force and args.command != "refresh":
        parser.error("--force requires refresh")
    try:
        if args.command == "init":
            if args.config.exists():
                raise ValueError("configuration already exists")
            if args.config.parent.exists() and any(args.config.parent.iterdir()):
                raise ValueError("choose an empty private state directory")
            library = (
                args.library or Path.home() / "Pictures/Photos Library.photoslibrary"
            )
            if not (library / "database/Photos.sqlite").exists():
                raise ValueError("Photos library not found; use --library")
            config = BridgeConfig(library.resolve(), args.config.parent.resolve())
            atomic_json(
                args.config,
                {
                    k: str(v) if isinstance(v, Path) else v
                    for k, v in asdict(config).items()
                },
            )
            ensure_token(config.state_path)
            result = {"initialized": True}
        elif args.command == "serve":
            import uvicorn

            from .api import create_app

            config = load_config(args.config)
            uvicorn.run(
                create_app(args.config),
                host="127.0.0.1",
                port=config.port,
                access_log=False,
                log_level="warning",
                limit_concurrency=32,
                backlog=64,
                h11_max_incomplete_event_size=16384,
            )
            return 0
        elif args.command == "status":
            config = load_config(args.config)
            result = read_json(config.state_path / "refresh.json", {}) | {
                "mode": "legacy_media" if config.legacy_media else "metadata_only"
            }
            if config.legacy_media:
                result["manifest"] = read_json(
                    config.state_path / "catalogs/manifest.json"
                )
        elif args.command == "pair":
            config = load_config(args.config)
            if not args.output or not args.helper_url:
                raise ValueError(
                    "pair requires --output FILE and --helper-url HTTPS_ORIGIN"
                )
            from ..io import validate_url

            atomic_json(
                args.output,
                {
                    "helper_url": validate_url(args.helper_url),
                    "token": ensure_token(config.state_path),
                },
            )
            result = {"pairing_file_written": True}
        elif args.command in ("install", "stop", "remove"):
            from .setup import install_helper, remove_helper

            result = (
                install_helper(args.config)
                if args.command == "install"
                else remove_helper(args.config, args.purge)
            )
        else:
            from .service import create_service

            service = create_service(load_config(args.config))
            try:
                success = service.refresh(force=args.force)
                result = service.status()
                if not success:
                    raise RuntimeError(result.get("refresh_error", "refresh_failed"))
            finally:
                service.close()
        print(json.dumps(result, ensure_ascii=False))
        return 0
    except Exception as error:  # noqa: BLE001 - sanitize provider/private diagnostics at CLI boundary
        # Provider and HTTP exceptions may carry secrets or private file names.
        print(
            json.dumps(
                {
                    "error": type(error).__name__,
                    "action": "Check configuration, Photos permissions, and private service diagnostics",
                }
            )
        )
        return 1


if __name__ == "__main__":
    sys.exit(main())
