from __future__ import annotations

import hashlib
from dataclasses import dataclass
from pathlib import Path


@dataclass
class StoredArtifact:
    storage_path: str
    sha256: str
    size_bytes: int


class ArtifactStore:
    def __init__(self, base_dir: Path) -> None:
        self.base_dir = base_dir
        self.base_dir.mkdir(parents=True, exist_ok=True)

    def put_bytes(
        self, run_id: str, work_item_id: str | None, name: str, data: bytes
    ) -> StoredArtifact:
        digest = hashlib.sha256(data).hexdigest()
        directory = self.base_dir / run_id
        if work_item_id:
            directory = directory / work_item_id
        directory.mkdir(parents=True, exist_ok=True)
        file_path = directory / name
        file_path.write_bytes(data)
        return StoredArtifact(
            storage_path=str(file_path.relative_to(self.base_dir)),
            sha256=digest,
            size_bytes=len(data),
        )

    def resolve(self, storage_path: str) -> Path:
        return self.base_dir / storage_path
