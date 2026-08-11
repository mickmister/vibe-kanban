#!/usr/bin/env python3
"""Dry-run Vibe Kanban Codex session cleanup candidates.

This script is intentionally read-only. It mirrors the current Vibe Kanban
candidate-selection policy for Codex sessions, then scans the Codex sessions
folder for rollout files that the existing manual cleanup logic would have
matched.

Example:
    python3 scripts/codex-session-cleanup-dry-run.py --db /path/to/vibe-kanban.sqlite

Notes:
    - The SQLite database is opened read-only.
    - No files are deleted.
    - This estimates rollout-file space only. Native `codex delete --force
      <session>` may reclaim additional Codex metadata.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import uuid
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import quote


DEFAULT_KEEP_COUNT = 5
CODEX_EXECUTOR = "CODEX"


@dataclass(frozen=True)
class CodexTurn:
    app_session_id: str
    execution_process_id: str
    agent_session_id: str
    created_at: str
    rowid: int


@dataclass
class SessionPlan:
    retained: list[CodexTurn] = field(default_factory=list)
    obsolete: list[CodexTurn] = field(default_factory=list)


@dataclass(frozen=True)
class MatchedFile:
    path: Path
    apparent_bytes: int
    disk_bytes: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Dry-run Vibe Kanban Codex session cleanup and report reclaimable "
            "rollout-file disk usage."
        )
    )
    parser.add_argument(
        "--db",
        required=True,
        type=Path,
        help="Path to the Vibe Kanban SQLite database to inspect.",
    )
    parser.add_argument(
        "--codex-home",
        type=Path,
        default=None,
        help=(
            "Path to Codex home. Defaults to $CODEX_HOME, then ~/.codex. "
            "The sessions folder is expected at <codex-home>/sessions."
        ),
    )
    parser.add_argument(
        "--keep",
        type=int,
        default=DEFAULT_KEEP_COUNT,
        help=f"Number of newest unique Codex session IDs to keep per VK session (default: {DEFAULT_KEEP_COUNT}).",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print retained sessions and obsolete session IDs without matching rollout files.",
    )
    return parser.parse_args()


def human_bytes(size: int) -> str:
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    value = float(size)
    for unit in units:
        if abs(value) < 1024.0 or unit == units[-1]:
            if unit == "B":
                return f"{int(value)} {unit}"
            return f"{value:.2f} {unit}"
        value /= 1024.0
    return f"{size} B"


def sqlite_value_to_text(value: Any) -> str:
    if isinstance(value, bytes) and len(value) == 16:
        return str(uuid.UUID(bytes=value))
    return str(value)


def resolve_codex_home(arg: Path | None) -> tuple[Path, bool]:
    if arg is not None:
        return arg.expanduser(), True

    env_home = os.environ.get("CODEX_HOME")
    if env_home and env_home.strip():
        return Path(env_home).expanduser(), False

    return Path.home() / ".codex", False


def open_db_readonly(db_path: Path) -> sqlite3.Connection:
    db_path = db_path.expanduser()
    if not db_path.exists():
        raise FileNotFoundError(f"database does not exist: {db_path}")
    if not db_path.is_file():
        raise ValueError(f"database path is not a file: {db_path}")

    absolute = db_path.resolve()
    uri = f"file:{quote(str(absolute), safe='/')}?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def normalize_executor(raw: Any) -> str | None:
    if not isinstance(raw, str):
        return None
    return raw.replace("-", "_").upper()


def executor_from_action(action_json: str) -> str | None:
    try:
        action = json.loads(action_json)
    except json.JSONDecodeError:
        return None

    if not isinstance(action, dict):
        return None

    # Current shape:
    # {"typ":{"type":"CodingAgentFollowUpRequest","executor_config":{"executor":"CODEX"}}}
    typ = action.get("typ")
    if isinstance(typ, dict):
        executor_config = typ.get("executor_config")
        if isinstance(executor_config, dict):
            executor = normalize_executor(executor_config.get("executor"))
            if executor is not None:
                return executor

        # Legacy compatibility: older serialized actions may have stored the
        # executor/profile directly on the request payload.
        for key in ("executor", "profile", "executor_profile_id", "profile_variant_label"):
            value = typ.get(key)
            if isinstance(value, dict):
                executor = normalize_executor(value.get("executor") or value.get("profile"))
            else:
                executor = normalize_executor(value)
            if executor is not None:
                return executor

    return None


def load_codex_turns(conn: sqlite3.Connection) -> list[CodexTurn]:
    query = """
        SELECT
            ep.rowid AS rowid,
            ep.id AS execution_process_id,
            ep.session_id AS app_session_id,
            ep.created_at AS created_at,
            ep.executor_action AS executor_action,
            cat.agent_session_id AS agent_session_id
        FROM execution_processes ep
        JOIN coding_agent_turns cat ON cat.execution_process_id = ep.id
        JOIN sessions s ON s.id = ep.session_id
        LEFT JOIN execution_processes reset_ep
          ON reset_ep.id = s.context_reset_execution_process_id
        WHERE ep.run_reason = 'codingagent'
          AND ep.dropped = FALSE
          AND cat.agent_session_id IS NOT NULL
          AND (
              s.context_reset_execution_process_id IS NULL
              OR reset_ep.id IS NULL
              OR ep.rowid > reset_ep.rowid
          )
        ORDER BY ep.session_id ASC, ep.created_at DESC, ep.rowid DESC
    """
    rows = conn.execute(query).fetchall()
    turns: list[CodexTurn] = []

    for row in rows:
        if executor_from_action(row["executor_action"]) != CODEX_EXECUTOR:
            continue

        turns.append(
            CodexTurn(
                app_session_id=sqlite_value_to_text(row["app_session_id"]),
                execution_process_id=sqlite_value_to_text(row["execution_process_id"]),
                agent_session_id=str(row["agent_session_id"]),
                created_at=str(row["created_at"]),
                rowid=int(row["rowid"]),
            )
        )

    return turns


def select_cleanup_plan(turns: list[CodexTurn], keep_count: int) -> dict[str, SessionPlan]:
    if keep_count < 0:
        raise ValueError("--keep must be zero or greater")

    grouped: dict[str, list[CodexTurn]] = defaultdict(list)
    for turn in turns:
        grouped[turn.app_session_id].append(turn)

    plan: dict[str, SessionPlan] = {}
    for app_session_id, session_turns in grouped.items():
        seen: set[str] = set()
        session_plan = SessionPlan()

        for turn in session_turns:
            if turn.agent_session_id in seen:
                continue
            seen.add(turn.agent_session_id)

            if len(session_plan.retained) < keep_count:
                session_plan.retained.append(turn)
            else:
                session_plan.obsolete.append(turn)

        plan[app_session_id] = session_plan

    return plan


def file_disk_bytes(path: Path) -> tuple[int, int]:
    stat = path.stat()
    apparent = stat.st_size
    blocks = getattr(stat, "st_blocks", None)
    disk = int(blocks) * 512 if blocks is not None else apparent
    return apparent, disk


def scan_codex_rollout_files(
    sessions_dir: Path,
    obsolete_session_ids: set[str],
) -> dict[str, list[MatchedFile]]:
    matches: dict[str, list[MatchedFile]] = {session_id: [] for session_id in obsolete_session_ids}
    if not obsolete_session_ids:
        return matches

    for root, dirnames, filenames in os.walk(sessions_dir, followlinks=False):
        dirnames.sort()
        filenames.sort()
        for filename in filenames:
            if not (
                filename.startswith("rollout-")
                and filename.endswith(".jsonl")
                and any(session_id in filename for session_id in obsolete_session_ids)
            ):
                continue

            path = Path(root) / filename
            matched_ids = [session_id for session_id in obsolete_session_ids if session_id in filename]
            if not matched_ids:
                continue

            apparent, disk = file_disk_bytes(path)
            matched = MatchedFile(path=path, apparent_bytes=apparent, disk_bytes=disk)
            for session_id in matched_ids:
                matches[session_id].append(matched)

    return matches


def print_report(
    *,
    db_path: Path,
    codex_home: Path,
    sessions_dir: Path,
    plan: dict[str, SessionPlan],
    file_matches: dict[str, list[MatchedFile]],
    verbose: bool,
) -> None:
    obsolete_turns = [turn for session_plan in plan.values() for turn in session_plan.obsolete]
    retained_turns = [turn for session_plan in plan.values() for turn in session_plan.retained]
    obsolete_ids = {turn.agent_session_id for turn in obsolete_turns}

    all_matched_files_by_path: dict[Path, MatchedFile] = {}
    for files in file_matches.values():
        for matched in files:
            all_matched_files_by_path[matched.path] = matched

    total_apparent = sum(file.apparent_bytes for file in all_matched_files_by_path.values())
    total_disk = sum(file.disk_bytes for file in all_matched_files_by_path.values())

    print("Codex session cleanup dry run")
    print("==============================")
    print(f"Database:              {db_path.expanduser()}")
    print(f"Codex home:            {codex_home}")
    print(f"Codex sessions folder: {sessions_dir}")
    print(f"VK sessions scanned:   {len(plan)}")
    print(f"Retained Codex IDs:    {len(retained_turns)}")
    print(f"Obsolete Codex IDs:    {len(obsolete_ids)}")
    print(f"Matched rollout files: {len(all_matched_files_by_path)}")
    print(f"Apparent bytes:        {total_apparent} ({human_bytes(total_apparent)})")
    print(f"Disk bytes:            {total_disk} ({human_bytes(total_disk)})")
    print()
    print("No files were deleted.")
    print(
        "Estimate caveat: this matches rollout files only; native "
        "`codex delete --force <session>` may reclaim additional Codex metadata."
    )

    if not obsolete_turns:
        return

    print()
    print("Hypothetical cleanup candidates")
    print("-------------------------------")
    for turn in sorted(obsolete_turns, key=lambda t: (t.app_session_id, t.created_at), reverse=True):
        files = file_matches.get(turn.agent_session_id, [])
        apparent = sum(file.apparent_bytes for file in files)
        disk = sum(file.disk_bytes for file in files)
        print(
            f"- {turn.agent_session_id} "
            f"(VK session {turn.app_session_id}, execution {turn.execution_process_id}, "
            f"created {turn.created_at}): {len(files)} file(s), "
            f"{human_bytes(disk)} disk / {human_bytes(apparent)} apparent"
        )
        for matched in files:
            print(
                f"    {matched.path} "
                f"({human_bytes(matched.disk_bytes)} disk / "
                f"{human_bytes(matched.apparent_bytes)} apparent)"
            )

    unmatched = sorted(session_id for session_id, files in file_matches.items() if not files)
    if unmatched and verbose:
        print()
        print("Obsolete Codex IDs without matching rollout files")
        print("------------------------------------------------")
        for session_id in unmatched:
            print(f"- {session_id}")

    if verbose and retained_turns:
        print()
        print("Retained Codex IDs")
        print("------------------")
        for turn in sorted(retained_turns, key=lambda t: (t.app_session_id, t.created_at), reverse=True):
            print(
                f"- {turn.agent_session_id} "
                f"(VK session {turn.app_session_id}, execution {turn.execution_process_id}, "
                f"created {turn.created_at})"
            )


def main() -> int:
    args = parse_args()
    codex_home, explicit_codex_home = resolve_codex_home(args.codex_home)
    sessions_dir = codex_home / "sessions"

    try:
        with open_db_readonly(args.db) as conn:
            turns = load_codex_turns(conn)
            plan = select_cleanup_plan(turns, args.keep)
    except (FileNotFoundError, ValueError, sqlite3.Error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    if not sessions_dir.exists():
        message = f"Codex sessions folder does not exist: {sessions_dir}"
        if explicit_codex_home:
            print(f"error: {message}", file=sys.stderr)
            return 2
        print(f"warning: {message}", file=sys.stderr)
        file_matches = {}
    elif not sessions_dir.is_dir():
        print(f"error: Codex sessions path is not a directory: {sessions_dir}", file=sys.stderr)
        return 2
    else:
        obsolete_ids = {
            turn.agent_session_id
            for session_plan in plan.values()
            for turn in session_plan.obsolete
        }
        file_matches = scan_codex_rollout_files(sessions_dir, obsolete_ids)

    print_report(
        db_path=args.db,
        codex_home=codex_home,
        sessions_dir=sessions_dir,
        plan=plan,
        file_matches=file_matches,
        verbose=args.verbose,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
