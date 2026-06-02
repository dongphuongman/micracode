"""Tool execution functions for the LLM tool-calling loop.

LangChain StructuredTool instances are used only to generate the JSON schema
for llm.bind_tools().  Actual execution is handled by the orchestrator loop,
not by LangChain's tool runner.
"""

from __future__ import annotations

import asyncio
import ipaddress
import re
import socket
import subprocess
from pathlib import Path
from typing import Literal
from urllib.parse import urlparse

import httpx
import pathspec
from bs4 import BeautifulSoup
from langchain_core.tools import StructuredTool
from markdownify import markdownify as _html_to_markdown
from pydantic import BaseModel
from pydantic import Field as PField

from .patcher import _ensure_use_client, _normalize_path, _path_is_safe, _truncate
from .storage import Storage, safe_join


# ---------------------------------------------------------------------------
# Execution functions
# ---------------------------------------------------------------------------


def execute_read_file(path: str, project_root: Path) -> str:
    """Read a file relative to project root; return contents or an error string."""
    rel = _normalize_path(path)
    if rel is None:
        return "error: empty path"
    if not _path_is_safe(rel):
        return f"error: path outside project root: {path!r}"
    try:
        return safe_join(project_root, rel).read_text(encoding="utf-8")
    except FileNotFoundError:
        return f"error: file not found: {path!r}"
    except OSError as exc:
        return f"error: {exc}"


def execute_write_patch(
    path: str,
    content: str,
    project_root: Path,
    storage: Storage,
    project_id: str,
) -> tuple[str, "FileWriteEvent | None"]:
    """Create or overwrite a file; return (result_message, FileWriteEvent|None)."""
    from .schemas.stream import FileWriteEvent

    rel = _normalize_path(path)
    if rel is None:
        return "error: empty path", None
    if not _path_is_safe(rel):
        return f"error: path outside project root: {path!r}", None

    final_content = _ensure_use_client(rel, content)
    final_content = _truncate(final_content)

    try:
        storage.write_file(project_id, rel, final_content)
    except (ValueError, OSError) as exc:
        return f"error writing file: {exc}", None

    return f"wrote {rel}", FileWriteEvent(path=rel, content=final_content)


def execute_grep(pattern: str, path: str, project_root: Path) -> str:
    """Search for a regex pattern in files; return matching lines with file:lineno."""
    try:
        regex = re.compile(pattern)
    except re.error as exc:
        return f"error: invalid pattern: {exc}"

    if path and path != ".":
        rel = _normalize_path(path)
        if rel is None:
            return "error: empty path"
        if not _path_is_safe(rel):
            return f"error: path outside project root: {path!r}"
        search_root = safe_join(project_root, rel)
    else:
        search_root = project_root

    if not search_root.exists():
        return f"error: path not found: {path!r}"

    candidates = [search_root] if search_root.is_file() else sorted(search_root.rglob("*"))
    results: list[str] = []
    for file_path in candidates:
        if not file_path.is_file():
            continue
        try:
            text = file_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for lineno, line in enumerate(text.splitlines(), 1):
            if regex.search(line):
                rel_display = file_path.relative_to(project_root)
                results.append(f"{rel_display}:{lineno}: {line}")
            if len(results) >= 200:
                break
        if len(results) >= 200:
            break

    if not results:
        return "no matches found"
    return "\n".join(results)


def _gitignore_style() -> str:
    """Prefer the non-deprecated 'gitignore' factory; fall back for older pathspec."""
    try:
        pathspec.PathSpec.from_lines("gitignore", [])
        return "gitignore"
    except KeyError:
        return "gitwildmatch"


_GITIGNORE_STYLE = _gitignore_style()


def _load_gitignore_spec(project_root: Path) -> "pathspec.PathSpec":
    """Build a gitignore matcher from the project's .gitignore, always ignoring .git/."""
    patterns = [".git/"]
    gitignore = project_root / ".gitignore"
    if gitignore.is_file():
        try:
            patterns += gitignore.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            pass
    return pathspec.PathSpec.from_lines(_GITIGNORE_STYLE, patterns)


def execute_glob(pattern: str, path: str, project_root: Path) -> str:
    """Find files matching a glob pattern; return paths sorted by mtime (newest first).

    Files ignored by the project's .gitignore (and anything under .git/) are skipped.
    """
    if not pattern:
        return "error: empty pattern"

    if path and path != ".":
        rel = _normalize_path(path)
        if rel is None:
            return "error: empty path"
        if not _path_is_safe(rel):
            return f"error: path outside project root: {path!r}"
        search_root = safe_join(project_root, rel)
    else:
        search_root = project_root

    if not search_root.exists():
        return f"error: path not found: {path!r}"
    if not search_root.is_dir():
        return f"error: not a directory: {path!r}"

    spec = _load_gitignore_spec(project_root)
    try:
        matches = [
            p
            for p in search_root.glob(pattern)
            if p.is_file()
            and not spec.match_file(p.relative_to(project_root).as_posix())
        ]
    except (ValueError, OSError) as exc:
        return f"error: invalid pattern: {exc}"

    def _mtime(p: Path) -> float:
        try:
            return p.stat().st_mtime
        except OSError:
            return 0.0

    matches.sort(key=_mtime, reverse=True)

    if not matches:
        return "no files found"

    truncated = len(matches) > 200
    lines = [str(p.relative_to(project_root)) for p in matches[:200]]
    if truncated:
        lines.append("[truncated at 200 matches]")
    return "\n".join(lines)


def execute_list_files(path: str, project_root: Path) -> str:
    """List files and subdirectories at path relative to project root."""
    if path and path != ".":
        rel = _normalize_path(path)
        if rel is None:
            return "error: empty path"
        if not _path_is_safe(rel):
            return f"error: path outside project root: {path!r}"
        target = safe_join(project_root, rel)
    else:
        target = project_root

    if not target.exists():
        return f"error: path not found: {path!r}"
    if not target.is_dir():
        return f"error: not a directory: {path!r}"

    entries = sorted(target.iterdir(), key=lambda p: (p.is_file(), p.name.lower()))
    lines = [
        str(e.relative_to(project_root)) + ("/" if e.is_dir() else "")
        for e in entries
    ]
    return "\n".join(lines) if lines else "(empty directory)"


def execute_search_replace(
    path: str,
    old_str: str,
    new_str: str,
    project_root: Path,
    storage: Storage,
    project_id: str,
) -> tuple[str, "FileWriteEvent | None"]:
    """Replace the unique occurrence of old_str with new_str in a file."""
    from .schemas.stream import FileWriteEvent

    rel = _normalize_path(path)
    if rel is None:
        return "error: empty path", None
    if not _path_is_safe(rel):
        return f"error: path outside project root: {path!r}", None

    file_path = safe_join(project_root, rel)
    try:
        current = file_path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return f"error: file not found: {path!r}", None
    except OSError as exc:
        return f"error: {exc}", None

    count = current.count(old_str)
    if count == 0:
        return "error: search string not found in file", None
    if count > 1:
        return f"error: search string matches {count} times — make old_str more specific", None

    new_content = current.replace(old_str, new_str, 1)
    final_content = _ensure_use_client(rel, new_content)
    final_content = _truncate(final_content)

    try:
        storage.write_file(project_id, rel, final_content)
    except (ValueError, OSError) as exc:
        return f"error writing file: {exc}", None

    return f"replaced in {rel}", FileWriteEvent(path=rel, content=final_content)


def execute_shell_exec(command: str, cwd: Path, output_limit: int) -> str:
    """Run a shell command; return combined stdout+stderr (truncated)."""
    try:
        result = subprocess.run(
            command,
            shell=True,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=60,
        )
        output = result.stdout + result.stderr
        if len(output) > output_limit:
            output = output[:output_limit] + f"\n[truncated at {output_limit} bytes]"
        return output
    except subprocess.TimeoutExpired:
        return "error: command timed out after 60 seconds"
    except OSError as exc:
        return f"error: {exc}"


# ---------------------------------------------------------------------------
# Todo list (session checklist) — todowrite / todoread
# ---------------------------------------------------------------------------

_TODO_STATUSES = ("pending", "in_progress", "completed", "cancelled")
_TODO_STATUS_GLYPH = {
    "pending": "[ ]",
    "in_progress": "[~]",
    "completed": "[x]",
    "cancelled": "[-]",
}


def normalize_todos(raw: object) -> tuple[list["TodoItem"], str | None]:
    """Coerce LLM-supplied todo data into validated ``TodoItem`` objects.

    Returns ``(items, error)``. ``error`` is a non-None string only when the
    input is structurally unusable (e.g. not a list); individual malformed
    entries are skipped rather than failing the whole write so a single bad
    item never derails the loop.
    """
    from .schemas.stream import TodoItem

    if not isinstance(raw, list):
        return [], "error: 'todos' must be a list of todo items"

    items: list[TodoItem] = []
    for index, entry in enumerate(raw):
        if not isinstance(entry, dict):
            continue
        content = entry.get("content")
        if not isinstance(content, str) or not content.strip():
            continue
        status = entry.get("status", "pending")
        if status not in _TODO_STATUSES:
            status = "pending"
        raw_id = entry.get("id")
        item_id = str(raw_id) if raw_id not in (None, "") else f"todo-{index + 1}"
        items.append(TodoItem(id=item_id, content=content.strip(), status=status))
    return items, None


def render_todos(todos: list["TodoItem"]) -> str:
    """Render a todo list as a compact text checklist for the LLM tool result."""
    if not todos:
        return "(todo list is empty)"
    done = sum(1 for t in todos if t.status == "completed")
    lines = [f"Todos ({done}/{len(todos)} completed):"]
    lines += [f"{_TODO_STATUS_GLYPH[t.status]} {t.content}" for t in todos]
    return "\n".join(lines)


def execute_todowrite(raw_todos: object) -> tuple[list["TodoItem"], str]:
    """Replace the session todo list; return (items, result_message)."""
    items, error = normalize_todos(raw_todos)
    if error is not None:
        return [], error
    return items, render_todos(items)


def execute_todoread(todos: list["TodoItem"]) -> str:
    """Return the current session todo list as text."""
    return render_todos(todos)


_WEBFETCH_USER_AGENT = "micracode-webfetch/1.0 (+https://github.com/Jamessdevops/micracode)"


def _html_to_text(html: str) -> str:
    """Strip an HTML document down to readable text, dropping scripts/styles."""
    soup = BeautifulSoup(html, "html.parser")
    for tag in soup(["script", "style", "noscript", "template"]):
        tag.decompose()
    return soup.get_text(separator="\n", strip=True)


_WEBFETCH_REDIRECT_CODES = (301, 302, 303, 307, 308)
_WEBFETCH_MAX_REDIRECTS = 5


def _ip_is_blocked(ip: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    """True if an address is not a routable public host (SSRF target)."""
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped is not None:
        ip = ip.ipv4_mapped
    return (
        ip.is_private
        or ip.is_loopback
        or ip.is_link_local  # includes 169.254.169.254 cloud metadata
        or ip.is_reserved
        or ip.is_multicast
        or ip.is_unspecified
    )


async def _ssrf_check_host(host: str, port: int) -> str | None:
    """Resolve ``host`` and return an error string if any address is non-public.

    Returns ``None`` when every resolved address is a routable public host.
    A literal IP in the URL is checked directly without a DNS lookup.
    """
    try:
        literal = ipaddress.ip_address(host)
    except ValueError:
        literal = None
    if literal is not None:
        if _ip_is_blocked(literal):
            return f"error: refusing to fetch private/internal address {host!r}"
        return None

    try:
        loop = asyncio.get_running_loop()
        infos = await loop.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except (OSError, socket.gaierror) as exc:
        return f"error: could not resolve host {host!r}: {exc}"
    if not infos:
        return f"error: could not resolve host {host!r}"

    for info in infos:
        addr = info[4][0]
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if _ip_is_blocked(ip):
            return (
                f"error: refusing to fetch {host!r} — it resolves to "
                f"private/internal address {addr}"
            )
    return None


async def execute_webfetch(
    url: str,
    fmt: str,
    *,
    timeout: float,
    output_limit: int,
    max_bytes: int,
    block_private: bool = True,
) -> str:
    """Fetch a URL and return its content as markdown, plain text, or raw HTML.

    ``fmt`` is one of ``"markdown"``, ``"text"``, or ``"html"``. Non-HTML
    responses (JSON, plain text, etc.) are returned verbatim regardless of
    ``fmt``. The download is capped at ``max_bytes`` and the returned string
    at ``output_limit`` characters.

    When ``block_private`` is true (the default) the target host — and every
    redirect hop — is resolved and rejected if it points at a private,
    loopback, link-local, or otherwise non-public address (SSRF guard).
    """
    target = (url or "").strip()
    if not target:
        return "error: empty url"
    if not urlparse(target).scheme:
        target = "https://" + target

    headers = {"User-Agent": _WEBFETCH_USER_AGENT}
    content_type = ""
    chunks: list[bytes] = []
    encoding = "utf-8"
    try:
        # Redirects are followed manually so each hop can be re-validated; httpx
        # auto-following would let a redirect escape the SSRF guard.
        async with httpx.AsyncClient(
            timeout=timeout, follow_redirects=False, headers=headers
        ) as client:
            for _hop in range(_WEBFETCH_MAX_REDIRECTS + 1):
                parsed = urlparse(target)
                if parsed.scheme not in ("http", "https"):
                    return (
                        f"error: unsupported URL scheme {parsed.scheme!r} "
                        "(only http/https are allowed)"
                    )
                host = parsed.hostname
                if not host:
                    return f"error: invalid URL (no host): {target!r}"
                if block_private:
                    port = parsed.port or (443 if parsed.scheme == "https" else 80)
                    blocked = await _ssrf_check_host(host, port)
                    if blocked is not None:
                        return blocked

                async with client.stream("GET", target) as resp:
                    if resp.status_code in _WEBFETCH_REDIRECT_CODES and "location" in resp.headers:
                        target = str(resp.url.join(resp.headers["location"]))
                        continue
                    resp.raise_for_status()
                    content_type = resp.headers.get("content-type", "").lower()
                    total = 0
                    async for chunk in resp.aiter_bytes():
                        chunks.append(chunk)
                        total += len(chunk)
                        if total > max_bytes:
                            break
                    encoding = resp.encoding or "utf-8"
                break
            else:
                return f"error: too many redirects (>{_WEBFETCH_MAX_REDIRECTS})"
    except httpx.HTTPStatusError as exc:
        return f"error: HTTP {exc.response.status_code} fetching {target}"
    except httpx.HTTPError as exc:
        return f"error: request failed: {exc}"

    body = b"".join(chunks).decode(encoding, errors="replace")
    is_html = "html" in content_type

    if not is_html or fmt == "html":
        result = body
    elif fmt == "text":
        result = _html_to_text(body)
    else:  # markdown (default)
        result = _html_to_markdown(body, heading_style="ATX").strip()

    if len(result) > output_limit:
        result = result[:output_limit] + f"\n[truncated at {output_limit} chars]"
    return result or "(empty response)"


# ---------------------------------------------------------------------------
# LangChain tool schemas (for bind_tools — not invoked directly)
# ---------------------------------------------------------------------------


class _ReadFileInput(BaseModel):
    path: str = PField(description="File path relative to the project root")


class _WritePatchInput(BaseModel):
    path: str = PField(description="File path relative to the project root")
    content: str = PField(description="Full content to write (creates or overwrites the file)")


class _ShellExecInput(BaseModel):
    command: str = PField(description="Shell command to execute in the project directory")
    reason: str = PField(description="Why this command is needed (shown to the user for approval)")


class _GrepInput(BaseModel):
    pattern: str = PField(description="Regular expression pattern to search for")
    path: str = PField(description="File or directory to search in (relative to project root). Use '.' for the entire project.")


class _GlobInput(BaseModel):
    pattern: str = PField(
        description="Glob pattern to match file paths, e.g. '**/*.py' or 'src/*.ts'."
    )
    path: str = PField(
        description="Directory to search in (relative to project root). Use '.' for the entire project."
    )


class _ListFilesInput(BaseModel):
    path: str = PField(description="Directory to list (relative to project root). Use '.' for the project root.")


class _SearchReplaceInput(BaseModel):
    path: str = PField(description="File path relative to the project root")
    old_str: str = PField(description="Exact string to find (must appear exactly once in the file)")
    new_str: str = PField(description="String to replace it with")


class _WebFetchInput(BaseModel):
    url: str = PField(
        description="URL to fetch. http/https only; if no scheme is given, https:// is assumed."
    )
    format: Literal["text", "markdown", "html"] = PField(
        default="markdown",
        description=(
            "Output format: 'markdown' (HTML converted to Markdown, the default and "
            "best for reading docs), 'text' (plain text with tags stripped), or 'html' "
            "(raw HTML). Non-HTML responses are returned as-is regardless of this value."
        ),
    )


class _TodoItemInput(BaseModel):
    content: str = PField(description="Short imperative description of the subtask")
    status: Literal["pending", "in_progress", "completed", "cancelled"] = PField(
        default="pending",
        description=(
            "Current state: 'pending' (not started), 'in_progress' (exactly one "
            "at a time), 'completed' (done), or 'cancelled' (no longer needed)."
        ),
    )
    id: str | None = PField(
        default=None,
        description="Stable identifier for the item; reuse it across updates to the same task.",
    )


class _TodoWriteInput(BaseModel):
    todos: list[_TodoItemInput] = PField(
        description=(
            "The COMPLETE todo list. This replaces the previous list entirely, "
            "so always include every item with its current status — not just the "
            "ones that changed."
        )
    )


class _TodoReadInput(BaseModel):
    pass


class _QuestionInput(BaseModel):
    question: str = PField(
        description="A single, specific clarifying question to ask the user"
    )
    options: list[str] | None = PField(
        default=None,
        description=(
            "Optional list of suggested answers for the user to pick from. "
            "The user may still reply with free-form text instead of choosing one."
        ),
    )


READ_FILE_TOOL = StructuredTool.from_function(
    lambda path: "",
    name="read_file",
    description="Read the current contents of a project file.",
    args_schema=_ReadFileInput,
)

WRITE_PATCH_TOOL = StructuredTool.from_function(
    lambda path, content: "",
    name="write_patch",
    description="Create or overwrite a file with the given full content.",
    args_schema=_WritePatchInput,
)

SHELL_EXEC_TOOL = StructuredTool.from_function(
    lambda command, reason: "",
    name="shell_exec",
    description=(
        "Run a shell command in the project directory. "
        "Always requires explicit user approval before execution."
    ),
    args_schema=_ShellExecInput,
)

GREP_TOOL = StructuredTool.from_function(
    lambda pattern, path: "",
    name="grep",
    description="Search for a regex pattern across project files. Returns matching lines with file path and line number.",
    args_schema=_GrepInput,
)

GLOB_TOOL = StructuredTool.from_function(
    lambda pattern, path: "",
    name="glob",
    description=(
        "Find files by glob pattern (e.g. '**/*.py', 'src/*.ts'). "
        "Returns matching file paths sorted by modification time, newest first. "
        "Files ignored by the project's .gitignore are skipped. "
        "Use this to locate files by name or extension when you don't know exact paths."
    ),
    args_schema=_GlobInput,
)

LIST_FILES_TOOL = StructuredTool.from_function(
    lambda path: "",
    name="list_files",
    description="List files and subdirectories at a given path within the project.",
    args_schema=_ListFilesInput,
)

SEARCH_REPLACE_TOOL = StructuredTool.from_function(
    lambda path, old_str, new_str: "",
    name="search_replace",
    description=(
        "Replace an exact substring in a file. "
        "old_str must appear exactly once; include more surrounding context if needed. "
        "Prefer this over write_patch for targeted edits to existing files."
    ),
    args_schema=_SearchReplaceInput,
)

WEBFETCH_TOOL = StructuredTool.from_function(
    lambda url, format="markdown": "",
    name="webfetch",
    description=(
        "Fetch the contents of a web page or URL over http/https. "
        "Returns the page as Markdown by default (or plain text / raw HTML). "
        "Use this to read documentation, API references, or any external page "
        "the user links to. Content is returned to you only — it is not saved to "
        "the project."
    ),
    args_schema=_WebFetchInput,
)

QUESTION_TOOL = StructuredTool.from_function(
    lambda question, options=None: "",
    name="question",
    description=(
        "Ask the user a clarifying question and pause until they answer. "
        "Use this when the request is ambiguous and a wrong assumption would "
        "waste significant work — not for trivial choices you can reasonably "
        "make yourself. Ask one focused question at a time; optionally supply "
        "a few suggested answers via `options`. The user's reply is returned "
        "as the tool result."
    ),
    args_schema=_QuestionInput,
)

TODOWRITE_TOOL = StructuredTool.from_function(
    lambda todos: "",
    name="todowrite",
    description=(
        "Create or update the session todo list — a structured checklist of the "
        "subtasks for the current request. Use it for multi-step work to plan "
        "before you start and to track progress as you go: mark a task "
        "'in_progress' before working on it (only one at a time) and 'completed' "
        "as soon as it is done. Pass the COMPLETE list every time; it replaces "
        "the previous one. Skip it for trivial single-step requests."
    ),
    args_schema=_TodoWriteInput,
)

TODOREAD_TOOL = StructuredTool.from_function(
    lambda: "",
    name="todoread",
    description=(
        "Read the current session todo list. Use this to re-check your plan and "
        "remaining work before deciding what to do next."
    ),
    args_schema=_TodoReadInput,
)

ALL_TOOLS = [
    READ_FILE_TOOL,
    WRITE_PATCH_TOOL,
    SHELL_EXEC_TOOL,
    GREP_TOOL,
    GLOB_TOOL,
    LIST_FILES_TOOL,
    SEARCH_REPLACE_TOOL,
    WEBFETCH_TOOL,
    QUESTION_TOOL,
    TODOWRITE_TOOL,
    TODOREAD_TOOL,
]
