from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from ..models import PhaseAttempt, Project, Run


class GitError(RuntimeError):
    pass


def _run_git(
    *args: str,
    cwd: Path | None = None,
    git_dir: Path | None = None,
    strip_output: bool = True,
) -> str:
    command = ["git"]
    if git_dir is not None:
        command.extend(["--git-dir", str(git_dir)])
    command.extend(args)
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise GitError(completed.stderr.strip() or "git command failed")
    return completed.stdout.strip() if strip_output else completed.stdout


class RepoWorkspaceManager:
    def __init__(self, cache_dir: Path, worktree_dir: Path) -> None:
        self.cache_dir = cache_dir
        self.worktree_dir = worktree_dir
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self.worktree_dir.mkdir(parents=True, exist_ok=True)

    def mirror_path_for(self, project: Project) -> Path:
        return self.cache_dir / f"{project.slug}.git"

    def ensure_mirror(self, project: Project) -> Path:
        mirror = self.mirror_path_for(project)
        if not mirror.exists():
            _run_git("clone", "--mirror", project.repo_url, str(mirror))
        else:
            _run_git("fetch", "--all", "--prune", git_dir=mirror)
        return mirror

    def prepare_worktree(
        self,
        project: Project,
        run: Run,
        phase_attempt: PhaseAttempt,
        patch_paths: list[Path],
    ) -> Path:
        mirror = self.ensure_mirror(project)
        worktree = self.worktree_dir / run.id / phase_attempt.id
        if worktree.exists():
            shutil.rmtree(worktree)
        worktree.parent.mkdir(parents=True, exist_ok=True)
        base_ref = run.base_ref or project.default_branch
        resolved_ref = self._resolve_ref(mirror, base_ref)
        _run_git("worktree", "add", "--detach", str(worktree), resolved_ref, git_dir=mirror)
        for patch_path in patch_paths:
            if patch_path.exists() and patch_path.stat().st_size:
                _run_git("apply", "--whitespace=nowarn", str(patch_path), cwd=worktree)
        return worktree

    def materialize_artifact(self, worktree: Path, relative_path: str, content: bytes) -> None:
        target = worktree / relative_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)

    def create_patch(self, worktree: Path) -> bytes | None:
        _run_git("add", "-A", cwd=worktree)
        diff = _run_git(
            "diff",
            "--cached",
            "--binary",
            "--no-color",
            cwd=worktree,
            strip_output=False,
        )
        _run_git("reset", "HEAD", "--", ".", cwd=worktree)
        return diff.encode() if diff else None

    def push_branch(self, worktree: Path, remote: str, branch_name: str) -> None:
        status = _run_git("status", "--porcelain", cwd=worktree)
        if status:
            _run_git("add", "-A", cwd=worktree)
            _run_git(
                "-c",
                "user.name=Bokkie",
                "-c",
                "user.email=bokkie@example.local",
                "commit",
                "-m",
                f"Bokkie publish for {branch_name}",
                cwd=worktree,
            )
        _run_git("push", remote, f"HEAD:refs/heads/{branch_name}", cwd=worktree)

    def cleanup(self, worktree: Path) -> None:
        if worktree.exists():
            shutil.rmtree(worktree)

    def _resolve_ref(self, mirror: Path, base_ref: str) -> str:
        candidates = [
            f"origin/{base_ref}",
            f"refs/remotes/origin/{base_ref}",
            f"refs/heads/{base_ref}",
            base_ref,
        ]
        for candidate in candidates:
            completed = subprocess.run(
                ["git", "--git-dir", str(mirror), "rev-parse", "--verify", candidate],
                check=False,
                capture_output=True,
                text=True,
            )
            if completed.returncode == 0:
                return candidate
        raise GitError(f"Unable to resolve base ref {base_ref!r}")
