"""Read-only MCP server for the Meetily app's local SQLite database.

Lets Claude list, search and read meetings, summaries, notes and transcripts.
It never writes: the database is opened read-only, and if that open fails
(macOS sandboxing can refuse it) it falls back to an immutable open.
"""

import json
import os
import sqlite3
from pathlib import Path
from urllib.parse import quote

from mcp.server.fastmcp import FastMCP

DB_PATH = Path(
    os.environ.get(
        "MEETILY_DB",
        Path.home() / "Library/Application Support/com.meetily.ai/meeting_minutes.sqlite",
    )
)

mcp = FastMCP("meetily")


def connect() -> sqlite3.Connection:
    if not DB_PATH.exists():
        raise FileNotFoundError(f"Meetily database not found at {DB_PATH}")
    uri = f"file:{quote(str(DB_PATH))}"
    try:
        conn = sqlite3.connect(f"{uri}?mode=ro", uri=True)
        conn.execute("SELECT 1 FROM meetings LIMIT 1")
    except sqlite3.OperationalError:
        conn = sqlite3.connect(f"{uri}?mode=ro&immutable=1", uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def query(sql: str, params: tuple = ()) -> list[dict]:
    with connect() as conn:
        return [dict(row) for row in conn.execute(sql, params)]


def summary_markdown(result: str | None) -> str | None:
    """summary_processes.result is JSON with a `markdown` key; fall back to the raw text."""
    if not result:
        return None
    try:
        parsed = json.loads(result)
    except json.JSONDecodeError:
        return result
    if isinstance(parsed, dict) and parsed.get("markdown"):
        return parsed["markdown"]
    return result


def fmt_time(seconds: float | None) -> str:
    if seconds is None:
        return "--:--"
    total = int(seconds)
    h, rem = divmod(total, 3600)
    m, s = divmod(rem, 60)
    return f"{h}:{m:02d}:{s:02d}" if h else f"{m:02d}:{s:02d}"


@mcp.tool()
def list_meetings(since: str | None = None, title_contains: str | None = None, limit: int = 50) -> list[dict]:
    """List meetings, newest first.

    Args:
        since: Only meetings created at or after this ISO date/time (e.g. "2026-10-29" or "2026-10-29T09:00:00").
        title_contains: Case-insensitive substring filter on the meeting title.
        limit: Maximum number of meetings to return (default 50).
    """
    sql = """
        SELECT m.id, m.title, m.created_at, m.calendar_start_time, m.calendar_end_time,
               s.status AS summary_status,
               (SELECT COUNT(*) FROM transcripts t WHERE t.meeting_id = m.id) AS segment_count,
               (SELECT MAX(t.audio_end_time) FROM transcripts t WHERE t.meeting_id = m.id) AS duration_seconds
        FROM meetings m
        LEFT JOIN summary_processes s ON s.meeting_id = m.id
        WHERE 1 = 1
    """
    params: list = []
    if since:
        sql += " AND m.created_at >= ?"
        params.append(since)
    if title_contains:
        sql += " AND m.title LIKE ?"
        params.append(f"%{title_contains}%")
    sql += " ORDER BY m.created_at DESC LIMIT ?"
    params.append(limit)
    return query(sql, tuple(params))


@mcp.tool()
def get_meeting(meeting_id: str) -> dict:
    """Get a meeting's details: metadata, calendar info, speakers, notes and summary.

    Args:
        meeting_id: The meeting id from list_meetings (e.g. "meeting-81ca0864-...").
    """
    rows = query(
        """
        SELECT m.*, s.status AS summary_status, s.result AS summary, n.notes_markdown AS notes
        FROM meetings m
        LEFT JOIN summary_processes s ON s.meeting_id = m.id
        LEFT JOIN meeting_notes n ON n.meeting_id = m.id
        WHERE m.id = ?
        """,
        (meeting_id,),
    )
    if not rows:
        raise ValueError(f"No meeting with id {meeting_id}")
    meeting = rows[0]
    meeting["summary"] = summary_markdown(meeting["summary"])
    meeting["speakers"] = [
        r["speaker"]
        for r in query(
            "SELECT DISTINCT speaker FROM transcripts WHERE meeting_id = ? AND speaker IS NOT NULL ORDER BY speaker",
            (meeting_id,),
        )
    ]
    return meeting


@mcp.tool()
def get_summary(meeting_id: str) -> str:
    """Get the AI-generated summary of a meeting as markdown.

    Args:
        meeting_id: The meeting id from list_meetings.
    """
    rows = query("SELECT status, result FROM summary_processes WHERE meeting_id = ?", (meeting_id,))
    if not rows or not rows[0]["result"]:
        status = rows[0]["status"] if rows else "not generated"
        return f"No summary for {meeting_id} (status: {status})."
    return summary_markdown(rows[0]["result"])


@mcp.tool()
def get_transcript(meeting_id: str, offset: int = 0, limit: int = 2000) -> str:
    """Get a meeting's transcript as "[mm:ss] Speaker: text" lines, in order.

    Long meetings can be paged with offset/limit (counted in segments).

    Args:
        meeting_id: The meeting id from list_meetings.
        offset: Number of segments to skip.
        limit: Maximum number of segments to return (default 2000).
    """
    rows = query(
        """
        SELECT transcript, speaker, audio_start_time
        FROM transcripts WHERE meeting_id = ?
        ORDER BY audio_start_time, timestamp
        LIMIT ? OFFSET ?
        """,
        (meeting_id, limit, offset),
    )
    if not rows:
        return f"No transcript segments for {meeting_id} at offset {offset}."
    return "\n".join(
        f"[{fmt_time(r['audio_start_time'])}] {r['speaker'] or 'Unknown'}: {r['transcript'].strip()}" for r in rows
    )


@mcp.tool()
def search_transcripts(text: str, meeting_id: str | None = None, limit: int = 50) -> list[dict]:
    """Search transcript segments for text (case-insensitive substring), newest meetings first.

    Args:
        text: Text to look for.
        meeting_id: Restrict the search to one meeting.
        limit: Maximum number of matching segments (default 50).
    """
    sql = """
        SELECT m.id AS meeting_id, m.title, m.created_at, t.speaker, t.audio_start_time, t.transcript
        FROM transcripts t JOIN meetings m ON m.id = t.meeting_id
        WHERE t.transcript LIKE ?
    """
    params: list = [f"%{text}%"]
    if meeting_id:
        sql += " AND t.meeting_id = ?"
        params.append(meeting_id)
    sql += " ORDER BY m.created_at DESC, t.audio_start_time LIMIT ?"
    params.append(limit)
    rows = query(sql, tuple(params))
    for r in rows:
        r["at"] = fmt_time(r.pop("audio_start_time"))
    return rows


if __name__ == "__main__":
    mcp.run()
