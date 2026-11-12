"""Exercise source export boundaries against an independent synthetic Git tree."""
from pathlib import Path
import json
import subprocess
import sys

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/export-release'


def source_tree(tmp_path):
    source = tmp_path / 'source'
    source.mkdir()
    files = {
        'manifest.json': json.dumps({'id':'domderen.photo-wallpapers','version':'0.4.0','entryPoints':{'service':'plugin/Service.qml'}}),
        'plugin/Service.qml':'import QtQuick\nItem {}',
        'plugin/components/PrivacyOverlay.qml':'NATIVE_PRIVACY_COVER',
        'crates/wallpapers/src/privacy/inference.rs':'NATIVE_PRIVACY_WORKER',
        'third_party/privacy-models.json':'PINNED_MODEL_PROVENANCE',
        'third_party/onnxruntime-MIT.txt':'RUNTIME_NOTICE',
        'tools/privacy-evaluation/private-reference.json':'PRIVATE_EVALUATION_CANARY',
        'Cargo.toml':'[workspace]\nmembers=[]\n',
        'Cargo.lock':'version = 4\n',
        '.python-version':'3.13.18\n',
        'LICENSE':'Synthetic license',
        'docs/public/README.md':'Public installation and removal',
        'src/photo_wallpapers/bridge/cli.py':'def main(): pass\n',
        'src/photo_wallpapers/client/cli.py':'LEGACY_MUST_NOT_EXPORT',
        'docs/IMPLEMENTATION_STATUS.md':'PRIVATE_CANARY',
        '.local/account.json':'PRIVATE_CANARY',
        'pyproject.toml':'[project]\nname="photo-bridge"\n',
    }
    for name, content in files.items():
        path=source/name; path.parent.mkdir(parents=True,exist_ok=True); path.write_text(content)
    subprocess.run(['git','init','-q',str(source)],check=True)
    subprocess.run(['git','-C',str(source),'add','.'],check=True)
    (source/'plugin/untracked.qml').write_text('PRIVATE_UNTRACKED')
    return source


def export(source,destination):
    return subprocess.run([sys.executable,str(SCRIPT),'--source',str(source),'--output',str(destination)],capture_output=True,text=True)


def test_export_uses_allowlist_and_tracked_files_without_development_history(tmp_path):
    source=source_tree(tmp_path); out=tmp_path/'release'
    result=export(source,out)
    assert result.returncode==0, result.stderr
    assert (out/'README.md').read_text()=='Public installation and removal'
    assert (out/'plugin/Service.qml').exists()
    assert (out/'plugin/components/PrivacyOverlay.qml').read_text() == 'NATIVE_PRIVACY_COVER'
    assert (out/'crates/wallpapers/src/privacy/inference.rs').read_text() == 'NATIVE_PRIVACY_WORKER'
    assert (out/'third_party/privacy-models.json').read_text() == 'PINNED_MODEL_PROVENANCE'
    assert (out/'third_party/onnxruntime-MIT.txt').read_text() == 'RUNTIME_NOTICE'
    assert not (out/'tools').exists()
    assert (out/'.python-version').read_text() == '3.13.18\n'
    assert (out/'src/photo_wallpapers/bridge/cli.py').exists()
    assert not (out/'src/photo_wallpapers/client').exists()
    assert not (out/'docs/IMPLEMENTATION_STATUS.md').exists()
    assert not (out/'.local').exists()
    assert not (out/'.git').exists()
    assert not (out/'plugin/untracked.qml').exists()
    assert json.loads((out/'release-source.json').read_text())['plugin_version']=='0.4.0'


def test_export_refuses_existing_destination_without_mutation(tmp_path):
    source=source_tree(tmp_path); out=tmp_path/'valuable'; out.mkdir()
    (out/'keep.txt').write_text('preserve me')
    result=export(source,out)
    assert result.returncode!=0 and "destination_exists" in result.stderr
    assert list(out.iterdir())==[out/'keep.txt']
    assert (out/'keep.txt').read_text()=='preserve me'


def test_export_rejects_a_tracked_symlink_in_runtime_source(tmp_path):
    source=source_tree(tmp_path)
    (source/'plugin/Service.qml').unlink()
    (source/'plugin/Service.qml').symlink_to(source/'.local/account.json')
    subprocess.run(['git','-C',str(source),'add','plugin/Service.qml'],check=True)
    out=tmp_path/'release'
    result=export(source,out)
    assert result.returncode!=0 and "symlink_forbidden" in result.stderr
    assert not out.exists()
