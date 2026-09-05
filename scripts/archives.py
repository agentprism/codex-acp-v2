"""Deterministic streaming ZIP payloads and corresponding-source archives."""

from pathlib import Path
import shutil
import stat
import tempfile
import zipfile


def write_archive(destination, directory, files):
    """Write name -> (bytes or Path, mode) without buffering backend executables."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
        staging = Path(temporary.name)
    try:
        with zipfile.ZipFile(staging, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for name, (content, mode) in sorted(files.items()):
                entry = zipfile.ZipInfo(f"{directory}/{name}" if directory else name)
                entry.create_system = 3
                entry.external_attr = (stat.S_IFREG | mode) << 16
                entry.compress_type = zipfile.ZIP_DEFLATED
                with output.open(entry, "w", force_zip64=True) as sink:
                    if isinstance(content, Path):
                        with content.open("rb") as source:
                            shutil.copyfileobj(source, sink, length=1024 * 1024)
                    else:
                        sink.write(content)
        staging.replace(destination)
    finally:
        staging.unlink(missing_ok=True)
