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
        self, run_id: str, phase_attempt_id: str | None, name: str, data: bytes
    ) -> StoredArtifact:
        relative = Path(run_id)
        if phase_attempt_id:
            relative = relative / phase_attempt_id
        relative = relative / name
        return self.put_relative_bytes(relative, data)

    def put_relative_bytes(self, relative_path: str | Path, data: bytes) -> StoredArtifact:
        digest = hashlib.sha256(data).hexdigest()
        file_path = self.base_dir / relative_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_bytes(data)
        return StoredArtifact(
            storage_path=str(file_path.relative_to(self.base_dir)),
            sha256=digest,
            size_bytes=len(data),
        )

    def read_bytes(self, storage_path: str) -> bytes:
        return self.resolve(storage_path).read_bytes()

    def resolve(self, storage_path: str) -> Path:
        return self.base_dir / storage_path
