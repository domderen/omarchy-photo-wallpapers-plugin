"""An installation must stop before building with an unsupported native codec."""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def test_source_install_rejects_libheif_before_security_floor(tmp_path):
    tools = tmp_path / 'tools'
    tools.mkdir()
    cargo = tools / 'cargo'
    marker = tmp_path / 'build-started'
    cargo.write_text('#!/bin/sh\ntouch "$BUILD_MARKER"\nexit 73\n')
    cargo.chmod(0o755)
    pkg = tools / 'pkg-config'
    pkg.write_text('#!/bin/sh\n# Simulate an installed libheif 1.23.6.\ncase "$1" in\n--atleast-version=1.21|--atleast-version=1.23.6) exit 0;;\n--atleast-version=1.23.7) exit 1;;\nesac\nexit 74\n')
    pkg.chmod(0o755)
    result = subprocess.run(['/bin/sh', str(ROOT / 'scripts/install-omarchy'), '--backend-only'],
        env=os.environ | {'PATH': str(tools) + ':/usr/bin:/bin', 'BUILD_MARKER': str(marker)},
        capture_output=True, text=True)
    assert result.returncode == 1, result.stderr
    assert '1.23.7' in result.stderr
    assert not marker.exists(), 'must refuse before invoking cargo'
