#!/usr/bin/env python3
"""Release the current branch or generate a Conventional Commits changelog.

Release usage:
  ./scripts/release.py [--bump auto|patch|minor|major] [--version X.Y.Z]
                       [--remote NAME] [--allow-branch] [--dry-run]
                       [--no-verify] [--yes] [--watch]

Changelog usage:
  ./scripts/release.py changelog [--from TAG] [--to REF] [--version X.Y.Z]
                                 [--from-tag TAG] [--remote NAME]
                                 [--repo-url URL] [--output PATH] [--raw]

Defaults (release mode):
  --bump auto      suggest patch/minor/major from Conventional Commits since
                   the last release tag (Enter accepts the suggestion)
  --version X.Y.Z  release X.Y.Z without the version prompt (with --yes when
                   stdin is not a terminal)
  --remote origin  remote the branch must match and the release is pushed to
  verify           run `make verify` before committing (Cargo.lock is still
                   refreshed via `cargo update --workspace --offline`;
                   --no-verify only skips the test/lint gate)
  --dry-run        run checks, preview changelog and plan without changing,
                   committing, or pushing anything
  --allow-branch   allow releasing from a branch other than the default branch
  --yes, -y        skip the interactive confirmation prompt
  --watch          after pushing, watch the release workflow with `gh`

Pushing the annotated tag triggers .github/workflows/release.yml, which
verifies, builds the universal macOS bundle, and publishes the GitHub release
directly with the tag's release notes (so installed copies' auto-updaters pick
it up as soon as CI finishes). NO_COLOR=1 disables colors.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Dict, List, Optional, Sequence, Set, Tuple

try:
    import readline  # noqa: F401  # enables line editing in input()
except ImportError:
    pass


ROOT = Path(__file__).resolve().parent.parent
U64_MAX = (1 << 64) - 1

CANONICAL_VERSION_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"
)
CONVENTIONAL_RE = re.compile(
    r"^(?P<type>[A-Za-z]+)(?:\((?P<scope>[^)]+)\))?(?P<breaking>!)?:\s*(?P<desc>.+)$"
)
BREAKING_FOOTER_RE = re.compile(r"(?m)^BREAKING[- ]CHANGE:\s*(.+)$")
RELEASE_COMMIT_RE = re.compile(r"^release\s+v", re.IGNORECASE)

MAINTENANCE_TYPES = frozenset(
    {"refactor", "docs", "build", "ci", "chore", "test", "style"}
)


class ReleaseError(Exception):
    """Expected release precondition or command failure."""


class Palette:
    """ANSI styles, enabled only when both streams are interactive terminals."""

    def __init__(self, enabled: Optional[bool] = None) -> None:
        if enabled is None:
            enabled = (
                sys.stdout.isatty()
                and sys.stderr.isatty()
                and not os.environ.get("NO_COLOR")
                and os.environ.get("TERM") != "dumb"
            )
        self.enabled = enabled
        self.bold = "\033[1m" if enabled else ""
        self.dim = "\033[2m" if enabled else ""
        self.italic = "\033[3m" if enabled else ""
        self.underline = "\033[4m" if enabled else ""
        self.strike = "\033[9m" if enabled else ""
        self.no_bold_dim = "\033[22m" if enabled else ""
        self.no_italic = "\033[23m" if enabled else ""
        self.no_underline = "\033[24m" if enabled else ""
        self.no_strike = "\033[29m" if enabled else ""
        self.fg_reset = "\033[39m" if enabled else ""
        self.red = "\033[31m" if enabled else ""
        self.green = "\033[32m" if enabled else ""
        self.yellow = "\033[33m" if enabled else ""
        self.blue = "\033[34m" if enabled else ""
        self.magenta = "\033[35m" if enabled else ""
        self.cyan = "\033[36m" if enabled else ""
        self.step_badge = "\033[1;30;46m" if enabled else ""
        self.dry_badge = "\033[1;30;43m" if enabled else ""
        self.reset = "\033[0m" if enabled else ""


ANSI_ESCAPE_RE = re.compile(r"\033\[[0-9;]*m")
INLINE_MD_RE = re.compile(
    r"`(?P<code>[^`]+)`"
    r"|\[(?P<link_label>[^\]]+)\]\((?P<link_url>https?://[^)\s]+)\)"
    r"|\*\*(?P<bold>[^*]+)\*\*"
    r"|~~(?P<strike>[^~]+)~~"
    r"|(?<!\*)\*(?P<italic>[^*\s][^*]*?)\*(?!\*)"
    r"|(?P<url>https?://[^\s<>)]+)"
    r"|(?<=\s)\((?P<sha>[0-9a-f]{7,12})\)(?=(?:\s|$))"
)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*#*$")
HR_RE = re.compile(r"^(?:[-*_]\s*){3,}$")
ULIST_RE = re.compile(r"^(\s*)[-*+]\s+(.*)$")
OLIST_RE = re.compile(r"^(\s*)(\d+[.)])\s+(.*)$")
SUMMARY_RE = re.compile(
    r"^<summary>(.*?)</summary>$", re.IGNORECASE
)


def terminal_width(
    default: int = 84, minimum: int = 60, maximum: int = 96
) -> int:
    cols = shutil.get_terminal_size((default, 24)).columns
    return max(minimum, min(cols, maximum))


def _visible_len(text: str) -> int:
    return len(ANSI_ESCAPE_RE.sub("", text))


def _update_sgr_state(active: List[str], esc: str) -> None:
    """Track active SGR attributes so wrapped lines neither bleed nor drop style."""
    codes = esc[2:-1].split(";") if len(esc) > 3 else ["0"]
    for code in codes:
        if code in ("", "0"):
            active.clear()
        elif code == "22":
            active[:] = [s for s in active if s not in ("1", "2")]
        elif code == "23":
            active[:] = [s for s in active if s != "3"]
        elif code == "24":
            active[:] = [s for s in active if s != "4"]
        elif code == "29":
            active[:] = [s for s in active if s != "9"]
        elif code == "39":
            active[:] = [s for s in active if not (s.isdigit() and (30 <= int(s) <= 37 or 90 <= int(s) <= 97))]
        elif code.isdigit():
            n = int(code)
            if 30 <= n <= 37 or 90 <= n <= 97:
                active[:] = [s for s in active if not (s.isdigit() and (30 <= int(s) <= 37 or 90 <= int(s) <= 97))]
            if code not in active:
                active.append(code)


def _wrap_ansi(
    text: str,
    width: int,
    initial_indent: str = "",
    subsequent_indent: str = "",
) -> List[str]:
    """Word-wrap text containing ANSI escapes without splitting URLs or codes."""
    if width <= 10:
        return [f"{initial_indent}{text}"]
    tokens = text.split(" ")
    raw_lines: List[Tuple[str, str]] = []
    prefix = initial_indent
    prefix_w = _visible_len(prefix)
    cur_tokens: List[str] = []
    cur_w = prefix_w

    for tok in tokens:
        tok_w = _visible_len(tok)
        if not cur_tokens:
            cur_tokens.append(tok)
            cur_w = prefix_w + tok_w
            continue
        if cur_w + 1 + tok_w <= width:
            cur_tokens.append(tok)
            cur_w += 1 + tok_w
        else:
            raw_lines.append((prefix, " ".join(cur_tokens)))
            prefix = subsequent_indent
            prefix_w = _visible_len(prefix)
            cur_tokens = [tok]
            cur_w = prefix_w + tok_w

    if cur_tokens:
        raw_lines.append((prefix, " ".join(cur_tokens)))
    if not raw_lines:
        return [initial_indent]

    lines: List[str] = []
    active: List[str] = []
    for idx, (pfx, body) in enumerate(raw_lines):
        reopen = f"\033[{';'.join(active)}m" if active else ""
        for m in ANSI_ESCAPE_RE.finditer(body):
            _update_sgr_state(active, m.group(0))
        close = "\033[0m" if active and idx < len(raw_lines) - 1 else ""
        lines.append(f"{pfx}{reopen}{body}{close}")
    return lines


def _render_inline_markdown(text: str, c: Palette) -> str:
    """Render common inline Markdown spans to ANSI (or clean plain text)."""

    def replace(m: re.Match) -> str:
        code = m.group("code")
        if code is not None:
            if c.enabled:
                return f"{c.cyan}{code}{c.fg_reset}"
            return f"`{code}`"
        link_label = m.group("link_label")
        if link_label is not None:
            url = m.group("link_url")
            if c.enabled:
                return (
                    f"{c.underline}{link_label}{c.no_underline} "
                    f"{c.dim}({url}){c.no_bold_dim}"
                )
            return f"{link_label} ({url})"
        bold = m.group("bold")
        if bold is not None:
            return f"{c.bold}{bold}{c.no_bold_dim}"
        strike = m.group("strike")
        if strike is not None:
            return f"{c.strike}{strike}{c.no_strike}"
        italic = m.group("italic")
        if italic is not None:
            return f"{c.italic}{italic}{c.no_italic}"
        url = m.group("url")
        if url is not None:
            if c.enabled:
                return f"{c.underline}{c.blue}{url}{c.no_underline}{c.fg_reset}"
            return url
        sha = m.group("sha")
        if sha is not None:
            return f"{c.dim}({sha}){c.no_bold_dim}"
        return m.group(0)

    return INLINE_MD_RE.sub(replace, text)


def render_markdown_ansi(
    markdown: str, palette: Palette, width: int = 72
) -> List[str]:
    """Fast, dependency-free Markdown renderer for terminal release notes."""
    c = palette
    out: List[str] = []
    in_code = False
    in_details = False
    in_breaking = False
    after_heading = False

    def push_blank() -> None:
        if out and out[-1] != "":
            out.append("")

    for raw_line in markdown.rstrip("\n").splitlines():
        stripped = raw_line.strip()

        if stripped.startswith("```"):
            in_code = not in_code
            after_heading = False
            if not in_code:
                push_blank()
            continue

        if in_code:
            indent = "  " * (2 if in_details else 1)
            out.append(f"{indent}{c.cyan}{raw_line}{c.reset}")
            continue

        if not stripped:
            if not after_heading:
                push_blank()
            continue

        low = stripped.lower()
        if low == "<details>":
            in_details = True
            in_breaking = False
            after_heading = False
            push_blank()
            continue
        if low == "</details>":
            in_details = False
            after_heading = False
            push_blank()
            continue

        summary_m = SUMMARY_RE.match(stripped)
        if summary_m:
            title = summary_m.group(1).strip()
            push_blank()
            out.append(
                f"{c.dim}▸ {c.reset}{c.bold}{c.dim}{title}{c.reset} "
                f"{c.dim}(collapsed on GitHub){c.reset}"
            )
            after_heading = True
            continue

        heading_m = HEADING_RE.match(stripped)
        if heading_m:
            level = len(heading_m.group(1))
            title = heading_m.group(2).strip()
            in_breaking = "breaking" in title.lower()
            push_blank()
            rendered_title = _render_inline_markdown(title, c)
            if in_breaking:
                style = f"{c.bold}{c.red}"
            elif level <= 2:
                style = f"{c.bold}{c.cyan}{c.underline}"
            else:
                style = f"{c.bold}{c.cyan}"
            out.append(f"{style}{rendered_title}{c.reset}")
            after_heading = True
            continue

        after_heading = False

        if HR_RE.match(stripped):
            push_blank()
            out.append(f"{c.dim}{'─' * min(width, 40)}{c.reset}")
            continue

        if stripped.startswith(">"):
            quote_text = stripped[1:].lstrip()
            styled = _render_inline_markdown(quote_text, c)
            base = "  " if in_details else ""
            gutter = f"{base}{c.dim}▎{c.reset} "
            out.extend(
                _wrap_ansi(
                    f"{c.italic}{styled}{c.reset}",
                    width,
                    initial_indent=gutter,
                    subsequent_indent=gutter,
                )
            )
            continue

        ulist_m = ULIST_RE.match(raw_line)
        if ulist_m:
            nest = len(ulist_m.group(1))
            item = _render_inline_markdown(ulist_m.group(2).strip(), c)
            base = "  " * (1 if in_details else 0) + " " * nest
            if in_breaking:
                bullet_color = c.red
            elif in_details:
                bullet_color = c.dim
            else:
                bullet_color = c.cyan
            initial = f"{base}{bullet_color}•{c.reset} "
            subsequent = f"{base}  "
            out.extend(
                _wrap_ansi(
                    item,
                    width,
                    initial_indent=initial,
                    subsequent_indent=subsequent,
                )
            )
            continue

        olist_m = OLIST_RE.match(raw_line)
        if olist_m:
            nest = len(olist_m.group(1))
            marker = olist_m.group(2)
            item = _render_inline_markdown(olist_m.group(3).strip(), c)
            base = "  " * (1 if in_details else 0) + " " * nest
            initial = f"{base}{c.cyan}{marker}{c.reset} "
            subsequent = f"{base}{' ' * (len(marker) + 1)}"
            out.extend(
                _wrap_ansi(
                    item,
                    width,
                    initial_indent=initial,
                    subsequent_indent=subsequent,
                )
            )
            continue

        base = "  " if in_details else ""
        styled = _render_inline_markdown(stripped, c)
        out.extend(
            _wrap_ansi(
                styled, width, initial_indent=base, subsequent_indent=base
            )
        )

    while out and out[-1] == "":
        out.pop()
    return out


def render_card(
    title: str,
    body_lines: Sequence[str],
    palette: Palette,
    *,
    width: int = 84,
    indent: str = "  ",
    border_color: Optional[str] = None,
    title_color: Optional[str] = None,
    pad_y: bool = True,
) -> str:
    """Render an open-right framed card so wrapped lines and URLs stay clean."""
    c = palette
    bc = c.cyan if border_color is None else border_color
    tc = f"{c.bold}{c.cyan}" if title_color is None else title_color
    inner_w = max(24, width - len(indent))

    header_prefix = f"╭─ {title} "
    dash_count = max(3, inner_w - _visible_len(header_prefix))
    top = (
        f"{indent}{bc}╭─{c.reset} {tc}{title}{c.reset} "
        f"{bc}{'─' * dash_count}{c.reset}"
    )
    bottom = f"{indent}{bc}╰{'─' * (inner_w - 1)}{c.reset}"
    gutter = f"{indent}{bc}│{c.reset}"

    lines = [top]
    if pad_y:
        lines.append(gutter)
    for line in body_lines:
        lines.append(f"{gutter}  {line}" if line else gutter)
    if pad_y:
        lines.append(gutter)
    lines.append(bottom)
    return "\n".join(lines)


class Reporter:
    """Structured terminal reporter with clear step banners and framed cards."""

    def __init__(self, total_steps: int, dry_run: bool = False) -> None:
        self.c = Palette()
        self.step_num = 0
        self.total_steps = total_steps
        self.dry_run = dry_run
        self.width = terminal_width()

    def _indent(self) -> str:
        return "  " if self.step_num else ""

    def banner(self, current: str, branch: str, remote: str) -> None:
        c = self.c
        meta = (
            f"{c.dim}current{c.reset} {c.bold}v{current}{c.reset}"
            f"  {c.dim}·{c.reset}  "
            f"{c.bold}{branch}{c.reset} {c.dim}->{c.reset} {c.bold}{remote}{c.reset}"
        )
        if self.dry_run:
            badge = (
                f"{c.dry_badge} DRY RUN {c.reset}"
                if c.enabled
                else "[DRY RUN]"
            )
            meta = f"{meta}   {badge}"
        print(
            render_card(
                "Cometty Release",
                [meta],
                c,
                width=self.width,
                indent="",
                border_color=c.yellow if self.dry_run else c.blue,
                title_color=f"{c.bold}{c.yellow if self.dry_run else c.blue}",
                pad_y=False,
            ),
            flush=True,
        )

    def step(self, title: str) -> None:
        self.step_num += 1
        c = self.c
        if c.enabled:
            badge_plain = f" STEP {self.step_num}/{self.total_steps} "
            badge = f"{c.step_badge}{badge_plain}{c.reset}"
        else:
            badge_plain = f"[STEP {self.step_num}/{self.total_steps}]"
            badge = badge_plain
        used = len(badge_plain) + 1 + len(title) + 1
        rule_len = max(3, self.width - used)
        rule = f"{c.dim}{'─' * rule_len}{c.reset}"
        print(f"\n{badge} {c.bold}{title}{c.reset} {rule}", flush=True)

    def markdown_card(self, title: str, markdown: str) -> None:
        content_width = max(36, self.width - 6)
        rendered = render_markdown_ansi(markdown, self.c, width=content_width)
        print(
            "\n"
            + render_card(
                title,
                rendered,
                self.c,
                width=self.width,
                indent="  ",
                border_color=self.c.cyan,
                title_color=f"{self.c.bold}{self.c.cyan}",
            ),
            flush=True,
        )

    def success_card(
        self, tag: str, repo_url: Optional[str]
    ) -> None:
        c = self.c
        lines = [
            "The release workflow will verify, build the macOS bundle, "
            f"and publish {c.bold}{tag}{c.reset} directly."
        ]
        if repo_url:
            actions_url = f"{repo_url}/actions/workflows/release.yml"
            release_url = f"{repo_url}/releases/tag/{tag}"
            url_style = f"{c.underline}{c.blue}" if c.enabled else ""
            url_end = f"{c.no_underline}{c.fg_reset}" if c.enabled else ""
            lines.append("")
            lines.append(
                f"{c.dim}Actions:{c.reset} {url_style}{actions_url}{url_end}"
            )
            lines.append(
                f"{c.dim}Release:{c.reset} {url_style}{release_url}{url_end}"
            )
        print(
            "\n"
            + render_card(
                f"✓ Released {tag}",
                lines,
                c,
                width=self.width,
                indent="",
                border_color=c.green,
                title_color=f"{c.bold}{c.green}",
            ),
            flush=True,
        )

    def ok(self, message: str) -> None:
        c = self.c
        print(
            f"{self._indent()}{c.green}{c.bold}✓{c.reset} {message}",
            flush=True,
        )

    def info(self, message: str) -> None:
        print(f"{self._indent()}{message}", flush=True)

    def warn(self, message: str) -> None:
        c = self.c
        print(
            f"{self._indent()}{c.yellow}{c.bold}warning:{c.reset} {message}",
            file=sys.stderr,
            flush=True,
        )

    def error(self, message: str) -> None:
        c = self.c
        print(
            f"{self._indent()}{c.red}{c.bold}error:{c.reset} {message}",
            file=sys.stderr,
            flush=True,
        )

    def refuse(self, message: str) -> None:
        """Stop a real release; in dry-run mode warn and continue."""
        if self.dry_run:
            self.warn(f"a real release would stop here: {message}")
        else:
            raise ReleaseError(message)


# ---------------------------------------------------------------------------
# Pure helpers: versions, Cargo files, git URLs, Conventional Commits
# ---------------------------------------------------------------------------


def parse_version(raw: str) -> Optional[Tuple[int, int, int]]:
    """Parse a canonical X.Y.Z version matching src/update/version.rs."""
    m = CANONICAL_VERSION_RE.match(raw)
    if not m:
        return None
    parts = (int(m.group(1)), int(m.group(2)), int(m.group(3)))
    if any(p > U64_MAX for p in parts):
        return None
    return parts


def parse_tag(tag: str) -> Optional[Tuple[int, int, int]]:
    """Parse a canonical vX.Y.Z tag."""
    if not tag.startswith("v"):
        return None
    return parse_version(tag[1:])


def format_version(version: Tuple[int, int, int]) -> str:
    return f"{version[0]}.{version[1]}.{version[2]}"


def bump_version(
    current: Tuple[int, int, int], bump: str
) -> Tuple[int, int, int]:
    major, minor, patch = current
    if bump == "patch":
        return (major, minor, patch + 1)
    if bump == "minor":
        return (major, minor + 1, 0)
    if bump == "major":
        return (major + 1, 0, 0)
    raise ValueError(f"unknown bump: {bump}")


def normalize_repo_url(remote_url: str) -> Optional[str]:
    """Turn a GitHub HTTPS or SSH remote URL into https://host/owner/repo."""
    url = remote_url.strip()
    if not url:
        return None
    ssh_scp = re.match(r"^[^@]+@([^:]+):(.+)$", url)
    if ssh_scp:
        host, path = ssh_scp.group(1), ssh_scp.group(2)
        url = f"https://{host}/{path}"
    elif url.startswith("ssh://"):
        m = re.match(r"^ssh://(?:[^@]+@)?([^/]+)/(.+)$", url)
        if not m:
            return None
        url = f"https://{m.group(1)}/{m.group(2)}"
    elif not re.match(r"^https?://", url):
        return None

    url = url.rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]
    return url


def read_cargo_version(text: str) -> Optional[str]:
    """Read version = "..." strictly from the [package] section of Cargo.toml."""
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and not stripped.startswith("[["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            m = re.match(r'^version\s*=\s*"([^"]*)"', stripped)
            if m:
                return m.group(1)
    return None


def replace_cargo_version(text: str, new_version: str) -> str:
    """Replace only [package].version in Cargo.toml, preserving everything else."""
    lines = text.splitlines(keepends=True)
    in_package = False
    replaced = False
    out: List[str] = []
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and not stripped.startswith("[["):
            in_package = stripped == "[package]"
        elif in_package and not replaced:
            m = re.match(r'^(\s*version\s*=\s*")[^"]*(".*)$', line, re.DOTALL)
            if m:
                line = f"{m.group(1)}{new_version}{m.group(2)}"
                replaced = True
        out.append(line)
    if not replaced:
        raise ValueError("could not locate [package] version in Cargo.toml")
    return "".join(out)


def read_lock_version(text: str, package: str = "cometty") -> Optional[str]:
    """Read the version for `package` from Cargo.lock."""
    blocks = re.split(r"(?m)^\[\[package\]\]\s*$", text)
    for block in blocks[1:]:
        name_m = re.search(r'(?m)^name\s*=\s*"([^"]*)"', block)
        if name_m and name_m.group(1) == package:
            ver_m = re.search(r'(?m)^version\s*=\s*"([^"]*)"', block)
            if ver_m:
                return ver_m.group(1)
    return None


@dataclass(frozen=True)
class Commit:
    sha: str
    short_sha: str
    subject: str
    body: str
    kind: Optional[str]
    scope: Optional[str]
    description: str
    breaking: bool
    breaking_note: Optional[str]
    is_release_commit: bool


def parse_commit(sha: str, short_sha: str, subject: str, body: str) -> Commit:
    subject = subject.strip()
    body = body.strip()
    m = CONVENTIONAL_RE.match(subject)
    footer_m = BREAKING_FOOTER_RE.search(body) if body else None
    breaking_note = footer_m.group(1).strip() if footer_m else None

    if m:
        kind = m.group("type").lower()
        scope = m.group("scope").strip() if m.group("scope") else None
        description = m.group("desc").strip()
        breaking = bool(m.group("breaking")) or breaking_note is not None
        is_release = kind == "chore" and (
            scope == "release" or bool(RELEASE_COMMIT_RE.match(description))
        )
    else:
        kind = None
        scope = None
        description = subject
        breaking = breaking_note is not None
        is_release = False

    return Commit(
        sha=sha,
        short_sha=short_sha,
        subject=subject,
        body=body,
        kind=kind,
        scope=scope,
        description=description,
        breaking=breaking,
        breaking_note=breaking_note,
        is_release_commit=is_release,
    )


def parse_git_log(raw: str) -> List[Commit]:
    """Parse git log output formatted with %H%x1f%h%x1f%s%x1f%b%x1e."""
    commits: List[Commit] = []
    for record in raw.split("\x1e"):
        record = record.strip("\r\n")
        if not record:
            continue
        fields = record.split("\x1f", 3)
        if len(fields) < 3:
            continue
        sha = fields[0].strip()
        short_sha = fields[1].strip()
        subject = fields[2].strip()
        body = fields[3] if len(fields) > 3 else ""
        if not sha or not subject:
            continue
        commits.append(parse_commit(sha, short_sha, subject, body))
    return commits


def select_previous_tag(
    tags: Sequence[str],
    exclude_tag: Optional[str] = None,
    max_version: Optional[Tuple[int, int, int]] = None,
) -> Optional[str]:
    """Pick the highest canonical vX.Y.Z tag from `tags`."""
    candidates: List[Tuple[Tuple[int, int, int], str]] = []
    for tag in tags:
        tag = tag.strip()
        if not tag or tag == exclude_tag:
            continue
        ver = parse_tag(tag)
        if ver is None:
            continue
        if max_version is not None and ver >= max_version:
            continue
        candidates.append((ver, tag))
    if not candidates:
        return None
    candidates.sort(key=lambda item: item[0], reverse=True)
    return candidates[0][1]


def suggest_bump(
    current: Tuple[int, int, int], commits: Sequence[Commit]
) -> Tuple[str, str]:
    """Suggest patch/minor/major from Conventional Commits and summarize counts."""
    meaningful = [c for c in commits if not c.is_release_commit]
    counts: Dict[str, int] = {
        "breaking": 0,
        "feat": 0,
        "fix": 0,
        "perf": 0,
        "mem": 0,
        "maint": 0,
        "other": 0,
    }
    for c in meaningful:
        if c.breaking:
            counts["breaking"] += 1
        if c.kind == "feat":
            counts["feat"] += 1
        elif c.kind in ("fix", "revert"):
            counts["fix"] += 1
        elif c.kind == "perf":
            counts["perf"] += 1
        elif c.kind == "mem":
            counts["mem"] += 1
        elif c.kind in MAINTENANCE_TYPES:
            counts["maint"] += 1
        else:
            counts["other"] += 1

    parts = [
        f"{counts[key]} {label}"
        for key, label in (
            ("breaking", "breaking"),
            ("feat", "feat"),
            ("fix", "fix"),
            ("perf", "perf"),
            ("mem", "mem"),
            ("other", "other"),
            ("maint", "maintenance"),
        )
        if counts[key]
    ]
    summary = ", ".join(parts) if parts else "no changes"

    if counts["breaking"]:
        if current[0] == 0:
            return "minor", f"{summary} (0.x breaking -> minor)"
        return "major", summary
    if counts["feat"]:
        return "minor", summary
    return "patch", summary


def _format_bullet(
    description: str,
    short_sha: str,
    scope: Optional[str] = None,
    prefix: Optional[str] = None,
) -> str:
    label = ""
    if prefix and scope:
        label = f"**{prefix}({scope}):** "
    elif prefix:
        label = f"**{prefix}:** "
    elif scope:
        label = f"**{scope}:** "
    return f"- {label}{description} ({short_sha})"


def render_changelog(
    commits: Sequence[Commit],
    prev_tag: Optional[str],
    target_ref: Optional[str],
    repo_url: Optional[str],
) -> str:
    """Render grouped GitHub-flavored Markdown release notes."""
    breaking_lines: List[str] = []
    feat_lines: List[str] = []
    fix_lines: List[str] = []
    perf_lines: List[str] = []
    other_lines: List[str] = []
    maint_lines: List[str] = []

    for c in commits:
        if c.is_release_commit:
            continue

        if c.breaking:
            note = c.breaking_note or c.description
            breaking_lines.append(
                _format_bullet(note, c.short_sha, scope=c.scope)
            )
            # If `!` had no separate footer, avoid duplicating the exact line.
            if c.breaking_note is None:
                continue

        if c.kind == "feat":
            feat_lines.append(
                _format_bullet(c.description, c.short_sha, scope=c.scope)
            )
        elif c.kind == "fix":
            fix_lines.append(
                _format_bullet(c.description, c.short_sha, scope=c.scope)
            )
        elif c.kind == "revert":
            fix_lines.append(
                _format_bullet(
                    f"Revert: {c.description}", c.short_sha, scope=c.scope
                )
            )
        elif c.kind in ("perf", "mem"):
            perf_lines.append(
                _format_bullet(c.description, c.short_sha, scope=c.scope)
            )
        elif c.kind in MAINTENANCE_TYPES:
            maint_lines.append(
                _format_bullet(
                    c.description, c.short_sha, scope=c.scope, prefix=c.kind
                )
            )
        else:
            other_lines.append(
                _format_bullet(c.description, c.short_sha, scope=c.scope)
            )

    sections: List[str] = []
    for heading, lines in (
        ("Breaking Changes", breaking_lines),
        ("Features", feat_lines),
        ("Bug Fixes", fix_lines),
        ("Performance & Memory", perf_lines),
        ("Other Changes", other_lines),
    ):
        if lines:
            sections.append(f"### {heading}\n\n" + "\n".join(lines))

    if maint_lines:
        sections.append(
            "<details>\n"
            "<summary>Maintenance</summary>\n\n"
            + "\n".join(maint_lines)
            + "\n\n</details>"
        )

    if not sections:
        sections.append("No notable changes.")

    if prev_tag and target_ref and repo_url:
        sections.append(
            f"**Full Changelog**: {repo_url}/compare/{prev_tag}...{target_ref}"
        )

    return "\n\n".join(sections) + "\n"


def format_tag_message(tag: str, notes: str) -> str:
    body = notes.strip()
    if not body:
        return f"cometty {tag}\n"
    return f"cometty {tag}\n\n{body}\n"


def extract_tag_notes(object_type: str, body: str) -> Optional[str]:
    """Return release notes from an annotated tag body, or None to regenerate."""
    if object_type.strip() != "tag":
        return None
    cleaned = body.strip()
    if not cleaned:
        return None
    return cleaned + "\n"


# ---------------------------------------------------------------------------
# Git & command helpers
# ---------------------------------------------------------------------------


def run_cmd(
    cmd: Sequence[str],
    *,
    capture: bool = True,
    check: bool = True,
    cwd: Optional[Path] = None,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        list(cmd),
        cwd=str(ROOT if cwd is None else cwd),
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=check,
    )


def run_git(
    *args: str, capture: bool = True, check: bool = True
) -> subprocess.CompletedProcess:
    return run_cmd(["git", *args], capture=capture, check=check)


def default_branch_for_remote(remote: str) -> str:
    res = run_git(
        "symbolic-ref",
        "--quiet",
        "--short",
        f"refs/remotes/{remote}/HEAD",
        check=False,
    )
    if res.returncode == 0:
        ref = res.stdout.strip()
        prefix = f"{remote}/"
        if ref.startswith(prefix) and len(ref) > len(prefix):
            return ref[len(prefix) :]
    return "main"


def resolve_repo_url(
    remote: str = "origin", explicit: Optional[str] = None
) -> Optional[str]:
    if explicit:
        return normalize_repo_url(explicit)
    res = run_git("remote", "get-url", remote, check=False)
    if res.returncode == 0:
        normalized = normalize_repo_url(res.stdout.strip())
        if normalized:
            return normalized
    server = os.environ.get("GITHUB_SERVER_URL", "").rstrip("/")
    repo = os.environ.get("GITHUB_REPOSITORY", "").strip("/")
    if server and repo:
        return f"{server}/{repo}"
    return None


def find_previous_tag(
    to_ref: str,
    exclude_tag: Optional[str] = None,
    max_version: Optional[Tuple[int, int, int]] = None,
) -> Optional[str]:
    res = run_git("tag", "--merged", to_ref, "--list", "v*", check=False)
    if res.returncode != 0:
        return None
    return select_previous_tag(
        res.stdout.splitlines(),
        exclude_tag=exclude_tag,
        max_version=max_version,
    )


def collect_commits(prev_tag: Optional[str], to_ref: str) -> List[Commit]:
    rev_range = f"{prev_tag}..{to_ref}" if prev_tag else to_ref
    res = run_git(
        "log",
        "--no-merges",
        "--format=%H%x1f%h%x1f%s%x1f%b%x1e",
        rev_range,
        check=False,
    )
    if res.returncode != 0:
        detail = res.stderr.strip() or f"git log failed for {rev_range}"
        raise ReleaseError(detail)
    return parse_git_log(res.stdout)


def read_tag_notes(tag: str) -> Optional[str]:
    ref = f"refs/tags/{tag}"
    type_res = run_git(
        "for-each-ref", "--format=%(objecttype)", ref, check=False
    )
    if type_res.returncode != 0 or not type_res.stdout.strip():
        return None
    body_res = run_git(
        "for-each-ref", "--format=%(contents:body)", ref, check=False
    )
    if body_res.returncode != 0:
        return None
    return extract_tag_notes(type_res.stdout, body_res.stdout)


def edit_notes_in_editor(notes: str) -> Optional[str]:
    editor_cmd = os.environ.get("VISUAL") or os.environ.get("EDITOR") or "vi"
    argv = shlex.split(editor_cmd)
    if not argv:
        raise ReleaseError("VISUAL/EDITOR is empty")
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        prefix="cometty-release-notes-",
        suffix=".md",
        delete=False,
    ) as tmp:
        tmp_path = Path(tmp.name)
        tmp.write(notes)
    try:
        res = subprocess.run([*argv, str(tmp_path)], check=False)
        if res.returncode != 0:
            raise ReleaseError(
                f"editor exited with status {res.returncode}: {editor_cmd}"
            )
        updated = tmp_path.read_text(encoding="utf-8").strip()
        if not updated:
            return None
        return updated + "\n"
    finally:
        tmp_path.unlink(missing_ok=True)


def watch_release_workflow(tag: str, ui: Reporter) -> None:
    if not shutil.which("gh"):
        ui.warn("`gh` not found; skipping --watch")
        return
    ui.info(f"waiting for release workflow for {tag}...")
    run_id: Optional[str] = None
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        res = run_cmd(
            [
                "gh",
                "run",
                "list",
                "--workflow",
                "release.yml",
                "--limit",
                "10",
                "--json",
                "databaseId,headBranch",
            ],
            check=False,
        )
        if res.returncode == 0 and res.stdout.strip():
            try:
                runs = json.loads(res.stdout)
            except json.JSONDecodeError:
                runs = []
            for run in runs:
                if run.get("headBranch") == tag and run.get("databaseId"):
                    run_id = str(run["databaseId"])
                    break
            if run_id:
                break
        time.sleep(2.0)

    if not run_id:
        ui.warn(
            f"release workflow for {tag} did not appear within 30s; "
            "check GitHub Actions directly"
        )
        return

    res = run_cmd(
        ["gh", "run", "watch", run_id, "--exit-status"],
        capture=False,
        check=False,
    )
    if res.returncode != 0:
        raise ReleaseError(
            f"release workflow run {run_id} failed (exit {res.returncode})"
        )


# ---------------------------------------------------------------------------
# Subcommand: changelog
# ---------------------------------------------------------------------------


def build_changelog_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="release.py changelog",
        description="Generate Conventional Commits release notes from git history.",
    )
    parser.add_argument(
        "--from",
        dest="from_tag",
        help="starting tag (default: latest reachable canonical vX.Y.Z tag)",
    )
    parser.add_argument(
        "--to",
        dest="to_ref",
        default="HEAD",
        help="ending git ref (default: HEAD)",
    )
    parser.add_argument(
        "--version",
        dest="version",
        help="target X.Y.Z version for the compare link",
    )
    parser.add_argument(
        "--from-tag",
        dest="annotated_tag",
        help="read notes from annotated tag TAG if present, else generate for TAG",
    )
    parser.add_argument(
        "--remote",
        default="origin",
        help="git remote used to infer the repository URL (default: origin)",
    )
    parser.add_argument(
        "--repo-url",
        help="override repository base URL (e.g. https://github.com/owner/repo)",
    )
    parser.add_argument(
        "-o",
        "--output",
        help="write notes to PATH instead of stdout",
    )
    parser.add_argument(
        "--raw",
        action="store_true",
        help="emit raw Markdown even when writing to an interactive terminal",
    )
    return parser


def run_changelog(argv: Sequence[str]) -> int:
    args = build_changelog_parser().parse_args(argv)
    os.chdir(ROOT)

    if not shutil.which("git"):
        raise ReleaseError("git not found")

    notes: Optional[str] = None
    card_title = "Release Notes"
    if args.annotated_tag:
        card_title = f"Release Notes · {args.annotated_tag}"
        notes = read_tag_notes(args.annotated_tag)

    if notes is None:
        to_ref = args.annotated_tag or args.to_ref
        target_version: Optional[Tuple[int, int, int]] = None
        target_ref: Optional[str] = None
        exclude_tag: Optional[str] = None

        if args.version:
            target_version = parse_version(args.version)
            if target_version is None:
                raise ReleaseError(
                    f"version must be canonical X.Y.Z: {args.version}"
                )
            target_ref = f"v{args.version}"
            exclude_tag = target_ref
        elif args.annotated_tag:
            target_version = parse_tag(args.annotated_tag)
            target_ref = args.annotated_tag
            exclude_tag = args.annotated_tag
        elif parse_tag(to_ref) is not None:
            target_version = parse_tag(to_ref)
            target_ref = to_ref
            exclude_tag = to_ref
        else:
            target_ref = to_ref

        if target_ref:
            card_title = f"Release Notes · {target_ref}"

        prev_tag = args.from_tag or find_previous_tag(
            to_ref,
            exclude_tag=exclude_tag,
            max_version=target_version,
        )
        commits = collect_commits(prev_tag, to_ref)
        repo_url = resolve_repo_url(args.remote, args.repo_url)
        notes = render_changelog(commits, prev_tag, target_ref, repo_url)

    if args.output:
        out_path = Path(args.output)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(notes, encoding="utf-8")
    elif sys.stdout.isatty() and not args.raw:
        palette = Palette()
        width = terminal_width()
        rendered = render_markdown_ansi(
            notes, palette, width=max(36, width - 4)
        )
        print(
            render_card(
                card_title,
                rendered,
                palette,
                width=width,
                indent="",
            )
        )
    else:
        sys.stdout.write(notes)
    return 0


# ---------------------------------------------------------------------------
# Main command: release
# ---------------------------------------------------------------------------


def build_release_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="release.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    bump_group = parser.add_mutually_exclusive_group()
    bump_group.add_argument(
        "--bump",
        choices=("auto", "patch", "minor", "major"),
        default=None,
        help="version increment to suggest (default: auto)",
    )
    bump_group.add_argument(
        "--version",
        default=None,
        help="release X.Y.Z without prompting for the version",
    )
    parser.add_argument(
        "--remote",
        default="origin",
        help="remote the branch must match and receive the push (default: origin)",
    )
    parser.add_argument(
        "--allow-branch",
        action="store_true",
        help="allow releasing from a branch other than the default branch",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="run checks and print changelog + plan without modifying anything",
    )
    parser.add_argument(
        "--no-verify",
        action="store_true",
        help="skip `make verify` (Cargo.lock is still refreshed)",
    )
    parser.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="skip the final confirmation prompt",
    )
    parser.add_argument(
        "--watch",
        action="store_true",
        help="after pushing, watch the GitHub Actions release workflow with `gh`",
    )
    return parser


def run_release(argv: Sequence[str]) -> int:
    args = build_release_parser().parse_args(argv)
    os.chdir(ROOT)

    cargo_bin = os.environ.get("CARGO", "cargo")
    make_bin = os.environ.get("MAKE", "make")

    for tool in ("git", cargo_bin):
        if not shutil.which(tool):
            raise ReleaseError(f"{tool} not found")

    cargo_toml_path = ROOT / "Cargo.toml"
    cargo_lock_path = ROOT / "Cargo.lock"
    cargo_text = cargo_toml_path.read_text(encoding="utf-8")
    current_str = read_cargo_version(cargo_text)
    if not current_str:
        raise ReleaseError("could not read [package] version from Cargo.toml")
    current_ver = parse_version(current_str)
    if current_ver is None:
        raise ReleaseError(
            f"Cargo.toml version is not canonical X.Y.Z: {current_str}"
        )

    remote = args.remote
    branch_res = run_git("symbolic-ref", "--quiet", "--short", "HEAD", check=False)
    branch = branch_res.stdout.strip()

    ui = Reporter(total_steps=6, dry_run=args.dry_run)
    ui.banner(current_str, branch or "(detached)", remote)

    # 1. Working tree & branch
    ui.step("Checking the working tree")
    if branch_res.returncode != 0 or not branch:
        raise ReleaseError("HEAD is detached; check out a branch before releasing")

    default_branch = default_branch_for_remote(remote)
    if branch != default_branch and not args.allow_branch:
        ui.refuse(
            f"on branch {branch} (expected {default_branch}); "
            f"switch to {default_branch} or pass --allow-branch"
        )

    dirty = run_git("status", "--porcelain").stdout.rstrip("\r\n")
    if dirty:
        indented = "\n".join(f"      {line}" for line in dirty.splitlines())
        ui.refuse(
            f"uncommitted changes; commit, stash, or remove them first:\n{indented}"
        )
    else:
        head_short = run_git("rev-parse", "--short", "HEAD").stdout.strip()
        ui.ok(f"on {branch} at {head_short}, nothing uncommitted")

    # 2. Remote comparison & tag sync
    ui.step(f"Comparing {branch} with {remote}")
    remote_tags: Set[str] = set()
    if run_git("remote", "get-url", remote, check=False).returncode != 0:
        ui.refuse(f"no remote named {remote}")
    else:
        ls_res = run_git(
            "ls-remote",
            "--refs",
            remote,
            f"refs/heads/{branch}",
            "refs/tags/*",
            check=False,
        )
        if ls_res.returncode != 0:
            ui.refuse(f"could not reach {remote}")
        else:
            remote_sha = ""
            for line in ls_res.stdout.splitlines():
                parts = line.split()
                if len(parts) < 2:
                    continue
                sha, ref = parts[0], parts[1]
                if ref.startswith("refs/tags/"):
                    remote_tags.add(ref)
                elif ref == f"refs/heads/{branch}":
                    remote_sha = sha

            fetch_tags = run_git(
                "fetch", "--quiet", "--tags", remote, check=False
            )
            if fetch_tags.returncode != 0:
                ui.warn(f"could not fetch tags from {remote}")

            if not remote_sha:
                ui.warn(
                    f"{remote} has no {branch} branch yet; the push will create it"
                )
            else:
                fetch_branch = run_git(
                    "fetch", "--quiet", remote, f"refs/heads/{branch}", check=False
                )
                if fetch_branch.returncode != 0:
                    ui.refuse(f"could not fetch {branch} from {remote}")
                else:
                    counts_out = run_git(
                        "rev-list",
                        "--left-right",
                        "--count",
                        f"{remote_sha}...HEAD",
                    ).stdout.split()
                    behind, ahead = int(counts_out[0]), int(counts_out[1])
                    if behind and ahead:
                        ui.refuse(
                            f"{branch} has diverged from {remote}/{branch} "
                            f"({ahead} local, {behind} remote commits); "
                            "rebase or merge first"
                        )
                    elif behind:
                        ui.refuse(
                            f"{branch} is {behind} commit(s) behind "
                            f"{remote}/{branch}; pull first"
                        )
                    elif ahead:
                        ui.warn(
                            f"{ahead} local commit(s) not on {remote} yet "
                            "will be pushed with the release"
                        )
                    else:
                        ui.ok(f"{branch} is up to date with {remote}/{branch}")

    # 3. Review release notes, choose version, and confirm
    ui.step("Review release & confirm")
    prev_tag = find_previous_tag("HEAD")
    commits = collect_commits(prev_tag, "HEAD")
    meaningful_commits = [c for c in commits if not c.is_release_commit]
    if not meaningful_commits:
        since = f"since {prev_tag}" if prev_tag else "on this branch"
        ui.refuse(f"no commits {since}; nothing to release")

    auto_bump, commit_summary = suggest_bump(current_ver, commits)
    bump_mode = args.bump or "auto"
    effective_bump = auto_bump if bump_mode == "auto" else bump_mode
    suggested = format_version(bump_version(current_ver, effective_bump))

    repo_url = resolve_repo_url(remote)
    since_label = f"since {prev_tag}" if prev_tag else "from initial history"
    ui.ok(f"{len(meaningful_commits)} commit(s) {since_label}: {commit_summary}")

    preview_target = f"v{args.version or suggested}"
    preview_notes = render_changelog(commits, prev_tag, preview_target, repo_url)
    ui.markdown_card(f"Release Notes · {preview_target}", preview_notes)
    print(flush=True)

    def version_problem(candidate: str) -> Optional[str]:
        parsed = parse_version(candidate)
        if parsed is None:
            return (
                f"version must be canonical X.Y.Z without leading zeros: "
                f"{candidate}"
            )
        if parsed <= current_ver:
            return f"new version {candidate} must be greater than {current_str}"
        if (
            run_git(
                "rev-parse",
                "-q",
                "--verify",
                f"refs/tags/v{candidate}",
                check=False,
            ).returncode
            == 0
        ):
            return f"tag already exists locally: v{candidate}"
        if f"refs/tags/v{candidate}" in remote_tags:
            return f"{remote} already has tag v{candidate}"
        return None

    c = ui.c
    if args.version is not None:
        new_version = args.version
        problem = version_problem(new_version)
        if problem:
            raise ReleaseError(problem)
    elif not sys.stdin.isatty():
        if args.dry_run:
            new_version = suggested
            problem = version_problem(new_version)
            if problem:
                ui.refuse(problem)
            ui.info(f"non-interactive dry run: using suggested {new_version}")
        else:
            raise ReleaseError(
                "no terminal to ask for the version; pass --version X.Y.Z --yes "
                "(make: RELEASE_VERSION=X.Y.Z RELEASE_YES=1)"
            )
    else:
        hint = (
            f"auto: {effective_bump}"
            if bump_mode == "auto"
            else f"--bump {effective_bump}"
        )
        ui.info(f"{c.dim}suggested {suggested} ({hint}){c.reset}")
        while True:
            try:
                answer = input(
                    f"  {c.bold}Next version{c.reset} "
                    f"(current {current_str}) [{c.cyan}{suggested}{c.reset}]: "
                ).strip()
            except EOFError:
                print(file=sys.stderr)
                raise ReleaseError("aborted: no version entered")
            candidate = answer or suggested
            problem = version_problem(candidate)
            if not problem:
                new_version = candidate
                break
            ui.error(problem)

    tag = f"v{new_version}"
    notes = render_changelog(commits, prev_tag, tag, repo_url)
    verify_plan = (
        "skip make verify (--no-verify)" if args.no_verify else "run make verify"
    )

    if args.dry_run:
        ui.ok(f"releasing {tag} (from {current_str})")
        print(
            f"\n  {c.yellow}{c.bold}Dry run:{c.reset} nothing was changed. "
            "A real release would:",
            flush=True,
        )
        print(f"    • update Cargo.toml and Cargo.lock to {new_version}")
        print(f"    • {verify_plan}")
        print(
            f'    • commit "chore: release {tag}" on {branch} and create '
            f"annotated tag {tag} with the release notes above"
        )
        print(
            f"    • push {branch} and {tag} atomically to {remote} "
            "(publishing directly to GitHub Releases & auto-updater)",
            flush=True,
        )
        return 0

    if not args.yes:
        if not sys.stdin.isatty():
            raise ReleaseError(
                "no terminal to confirm release; pass --yes "
                "(make: RELEASE_YES=1)"
            )
        print(flush=True)
        ui.info(
            f"{c.dim}Release:{c.reset} {c.bold}{current_str} -> {new_version}{c.reset} "
            f"(tag {c.cyan}{tag}{c.reset})"
        )
        ui.info(
            f"{c.dim}Target:{c.reset}  {branch} -> {remote} ({verify_plan})"
        )
        ui.info(
            f"{c.dim}Publish:{c.reset} direct "
            "(GitHub Release goes live & reaches the auto-updater once CI finishes)"
        )
        while True:
            try:
                reply = input(
                    f"  {c.bold}Proceed?{c.reset} [y/N/e (e = edit notes)]: "
                ).strip().lower()
            except EOFError:
                print(file=sys.stderr)
                raise ReleaseError("aborted by user")
            if reply in ("y", "yes"):
                break
            if reply in ("e", "edit"):
                edited = edit_notes_in_editor(notes)
                if edited is None:
                    ui.warn("release notes were empty; keeping previous notes")
                else:
                    notes = edited
                    ui.markdown_card(f"Release Notes · {tag} (edited)", notes)
                    print(flush=True)
                continue
            if reply in ("", "n", "no"):
                raise ReleaseError("aborted by user")
            ui.error("enter y to release, e to edit notes, or n to abort")
    else:
        ui.ok(f"releasing {tag} (from {current_str}) — confirmed (--yes)")

    # 4-6. Mutate, verify, commit, tag, push (with rollback before commit)
    modified = False
    committed = False
    try:
        ui.step("Updating Cargo.toml and Cargo.lock")
        modified = True
        updated_cargo = replace_cargo_version(cargo_text, new_version)
        cargo_toml_path.write_text(updated_cargo, encoding="utf-8")
        actual = read_cargo_version(cargo_toml_path.read_text(encoding="utf-8"))
        if actual != new_version:
            raise ReleaseError(f"failed to update Cargo.toml to {new_version}")
        ui.ok(f"Cargo.toml: {current_str} -> {new_version}")

        lock_cmd = [cargo_bin, "update", "--workspace", "--offline", "--quiet"]
        lock_res = run_cmd(lock_cmd, capture=True, check=False)
        if lock_res.returncode != 0:
            detail = lock_res.stderr.strip() or lock_res.stdout.strip()
            raise ReleaseError(
                f"failed to refresh Cargo.lock via {' '.join(lock_cmd)}: {detail}"
            )
        lock_version = read_lock_version(
            cargo_lock_path.read_text(encoding="utf-8")
        )
        if lock_version != new_version:
            raise ReleaseError(
                f"Cargo.lock did not update to {new_version} "
                f"(found: {lock_version or 'none'})"
            )
        ui.ok("Cargo.lock refreshed (offline)")

        ui.step("Verifying")
        if args.no_verify:
            ui.warn("skipped (--no-verify)")
        else:
            if not shutil.which(make_bin):
                raise ReleaseError(f"{make_bin} not found")
            verify_res = run_cmd(
                [make_bin, "verify"], capture=False, check=False
            )
            if verify_res.returncode != 0:
                raise ReleaseError("make verify failed")
            ui.ok("make verify passed")

        ui.step("Committing, tagging, and pushing")
        run_git("add", "--", "Cargo.toml", "Cargo.lock")
        commit_res = run_git(
            "commit", "-q", "-m", f"chore: release {tag}", check=False
        )
        if commit_res.returncode != 0:
            detail = commit_res.stderr.strip() or commit_res.stdout.strip()
            raise ReleaseError(f"git commit failed: {detail}")
        committed = True

        tag_msg = format_tag_message(tag, notes)
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            prefix="cometty-tag-msg-",
            suffix=".txt",
            delete=False,
        ) as tag_tmp:
            tag_tmp_path = Path(tag_tmp.name)
            tag_tmp.write(tag_msg)
        try:
            tag_res = run_git(
                "tag",
                "-a",
                "--cleanup=verbatim",
                tag,
                "-F",
                str(tag_tmp_path),
                check=False,
            )
        finally:
            tag_tmp_path.unlink(missing_ok=True)
        if tag_res.returncode != 0:
            detail = tag_res.stderr.strip() or tag_res.stdout.strip()
            raise ReleaseError(
                f"git tag failed: {detail}\n"
                "    undo commit: git reset --keep HEAD~1"
            )

        commit_sha = run_git("rev-parse", "--short", "HEAD").stdout.strip()
        ui.ok(f'committed {commit_sha} "chore: release {tag}" and tagged {tag}')

        push_res = run_git(
            "push",
            "--atomic",
            remote,
            f"HEAD:refs/heads/{branch}",
            f"refs/tags/{tag}",
            capture=False,
            check=False,
        )
        if push_res.returncode != 0:
            raise ReleaseError(
                f"push failed; the release commit and {tag} are still local.\n"
                f"    retry: git push --atomic {remote} "
                f"HEAD:refs/heads/{branch} refs/tags/{tag}\n"
                f"    undo:  git tag -d {tag} && git reset --keep HEAD~1"
            )
        ui.ok(f"pushed {branch} and {tag} to {remote}")
    finally:
        if modified and not committed:
            ui.warn("restoring Cargo.toml and Cargo.lock")
            run_git("reset", "-q", "--", "Cargo.toml", "Cargo.lock", check=False)
            run_git("checkout", "--", "Cargo.toml", "Cargo.lock", check=False)

    ui.success_card(tag, repo_url)

    if args.watch:
        print()
        watch_release_workflow(tag, ui)

    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    try:
        if raw_argv and raw_argv[0] == "changelog":
            return run_changelog(raw_argv[1:])
        return run_release(raw_argv)
    except ReleaseError as err:
        c = Palette()
        print(f"{c.red}{c.bold}error:{c.reset} {err}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        c = Palette()
        print(f"\n{c.red}{c.bold}error:{c.reset} aborted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
