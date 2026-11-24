#!/usr/bin/env python3
"""Unit tests for scripts/release.py (stdlib only, headless-safe)."""
from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import release  # noqa: E402


class VersionTests(unittest.TestCase):
    def test_parse_version_matches_updater_rules(self) -> None:
        self.assertEqual(release.parse_version("0.2.0"), (0, 2, 0))
        self.assertEqual(release.parse_version("10.0.1"), (10, 0, 1))
        for bad in (
            "01.2.3",
            "0.02.0",
            "1.2.03",
            "v1.2.3",
            "1.2",
            "1.2.3-rc.1",
            "1.2.3+meta",
            "1.2.3 ",
            "",
        ):
            self.assertIsNone(release.parse_version(bad), bad)

    def test_parse_tag(self) -> None:
        self.assertEqual(release.parse_tag("v0.3.0"), (0, 3, 0))
        self.assertIsNone(release.parse_tag("0.3.0"))
        self.assertIsNone(release.parse_tag("v0.03.0"))
        self.assertIsNone(release.parse_tag("v0.3.0-rc1"))

    def test_bump_version(self) -> None:
        self.assertEqual(release.bump_version((0, 2, 0), "patch"), (0, 2, 1))
        self.assertEqual(release.bump_version((0, 2, 4), "minor"), (0, 3, 0))
        self.assertEqual(release.bump_version((0, 2, 4), "major"), (1, 0, 0))

    def test_select_previous_tag_uses_numeric_semver_order(self) -> None:
        tags = ["v0.1.0", "v0.2.0", "v0.10.0", "v0.9.0", "v0.10.0-rc1", "notes"]
        self.assertEqual(release.select_previous_tag(tags), "v0.10.0")
        self.assertEqual(
            release.select_previous_tag(tags, exclude_tag="v0.10.0"),
            "v0.9.0",
        )
        self.assertEqual(
            release.select_previous_tag(tags, max_version=(0, 9, 0)),
            "v0.2.0",
        )
        self.assertIsNone(
            release.select_previous_tag(["v0.1.0"], exclude_tag="v0.1.0")
        )


class CargoFileTests(unittest.TestCase):
    SAMPLE_TOML = """\
[package]
name = "cometty"
version = "0.2.0"
edition = "2024"

[dependencies]
# Keep version line inside dependencies untouched
cosmic-text = { version = "0.19", features = ["shape-run-cache"] }

[other]
version = "9.9.9"
"""

    def test_read_and_replace_package_version_only(self) -> None:
        self.assertEqual(release.read_cargo_version(self.SAMPLE_TOML), "0.2.0")
        updated = release.replace_cargo_version(self.SAMPLE_TOML, "0.3.0")
        self.assertEqual(release.read_cargo_version(updated), "0.3.0")
        self.assertIn('version = "9.9.9"', updated)
        self.assertIn('version = "0.19"', updated)

    def test_read_lock_version(self) -> None:
        lock = """\
[[package]]
name = "anyhow"
version = "1.0.100"

[[package]]
name = "cometty"
version = "0.3.0"
dependencies = []
"""
        self.assertEqual(release.read_lock_version(lock), "0.3.0")
        self.assertIsNone(release.read_lock_version(lock, package="missing"))


class RepoUrlTests(unittest.TestCase):
    def test_normalize_repo_url(self) -> None:
        cases = {
            "https://github.com/jjunac/cometty.git": "https://github.com/jjunac/cometty",
            "https://github.com/jjunac/cometty/": "https://github.com/jjunac/cometty",
            "git@github.com:jjunac/cometty.git": "https://github.com/jjunac/cometty",
            "ssh://git@github.com/jjunac/cometty.git": "https://github.com/jjunac/cometty",
        }
        for raw, expected in cases.items():
            self.assertEqual(release.normalize_repo_url(raw), expected)
        self.assertIsNone(release.normalize_repo_url("/local/path/repo"))


class ChangelogTests(unittest.TestCase):
    def _commit(
        self, subject: str, body: str = "", sha: str = "abcdef1234567"
    ) -> release.Commit:
        return release.parse_commit(sha, sha[:7], subject, body)

    def test_parse_git_log_and_filter_release_commits(self) -> None:
        raw = (
            "1111111aaaa\x1f1111111\x1fchore: release v0.2.0\x1f\x1e"
            "2222222bbbb\x1f2222222\x1ffeat(links): open local paths\x1f\x1e"
            "3333333cccc\x1f3333333\x1fmem: pack scrollback rows\x1fbody line\n\x1e"
        )
        commits = release.parse_git_log(raw)
        self.assertEqual(len(commits), 3)
        self.assertTrue(commits[0].is_release_commit)
        self.assertEqual(commits[1].kind, "feat")
        self.assertEqual(commits[1].scope, "links")
        self.assertEqual(commits[2].kind, "mem")

    def test_suggest_bump_rules(self) -> None:
        fix_only = [self._commit("fix: jitter"), self._commit("perf: faster")]
        self.assertEqual(release.suggest_bump((0, 2, 0), fix_only)[0], "patch")

        with_feat = [self._commit("feat: file links"), self._commit("fix: bug")]
        self.assertEqual(release.suggest_bump((0, 2, 0), with_feat)[0], "minor")

        breaking = [self._commit("feat!: drop old config format")]
        # On 0.x.y breaking suggests minor; on >=1.0.0 it suggests major.
        self.assertEqual(release.suggest_bump((0, 2, 0), breaking)[0], "minor")
        self.assertEqual(release.suggest_bump((1, 2, 0), breaking)[0], "major")

        breaking_footer = [
            self._commit(
                "refactor: rename setting",
                body="BREAKING CHANGE: old key removed",
            )
        ]
        self.assertEqual(
            release.suggest_bump((1, 0, 0), breaking_footer)[0], "major"
        )

    def test_render_changelog_sections_and_collapsed_maintenance(self) -> None:
        commits = [
            self._commit("chore: release v0.3.0", sha="0000000111111"),
            self._commit(
                "feat(links): open file:// links",
                sha="1111111aaaaaa",
            ),
            self._commit("fix: SGR mouse release", sha="2222222bbbbbb"),
            self._commit("revert: broken toggle", sha="3333333cccccc"),
            self._commit("perf: retain cell geometry", sha="4444444dddddd"),
            self._commit("mem: compact scrollback", sha="5555555eeeeee"),
            self._commit("docs: update development guide", sha="6666666ffffff"),
            self._commit("build(ci): cache rust targets", sha="7777777000000"),
            self._commit("misc tweak without prefix", sha="8888888111111"),
            self._commit(
                "feat!: change default title format", sha="9999999222222"
            ),
            self._commit(
                "feat(config): new schema",
                body="Details.\n\nBREAKING CHANGE: remove legacy key",
                sha="aaaaaaa333333",
            ),
        ]
        md = release.render_changelog(
            commits,
            prev_tag="v0.2.0",
            target_ref="v0.3.0",
            repo_url="https://github.com/jjunac/cometty",
        )
        self.assertNotIn("release v0.3.0", md)
        self.assertIn("### Breaking Changes\n\n", md)
        self.assertIn("- change default title format (9999999)", md)
        self.assertIn("- **config:** remove legacy key (aaaaaaa)", md)
        # `!` without separate footer is not duplicated under Features; footer is.
        feat_section = md.split("### Features\n\n", 1)[1].split("\n\n### ", 1)[0]
        self.assertNotIn("9999999", feat_section)
        self.assertIn("- **links:** open file:// links (1111111)", feat_section)
        self.assertIn("- **config:** new schema (aaaaaaa)", feat_section)
        self.assertIn("- SGR mouse release (2222222)", md)
        self.assertIn("- Revert: broken toggle (3333333)", md)
        self.assertIn("### Performance & Memory\n\n", md)
        self.assertIn("- retain cell geometry (4444444)", md)
        self.assertIn("- compact scrollback (5555555)", md)
        self.assertIn("### Other Changes\n\n- misc tweak without prefix (8888888)", md)
        self.assertIn(
            "<details>\n<summary>Maintenance</summary>\n\n"
            "- **docs:** update development guide (6666666)\n"
            "- **build(ci):** cache rust targets (7777777)\n\n"
            "</details>",
            md,
        )
        self.assertTrue(
            md.endswith(
                "**Full Changelog**: "
                "https://github.com/jjunac/cometty/compare/v0.2.0...v0.3.0\n"
            )
        )

    def test_extract_tag_notes_ignores_lightweight_and_single_line_tags(self) -> None:
        self.assertIsNone(release.extract_tag_notes("commit", "commit body"))
        self.assertIsNone(release.extract_tag_notes("tag", "   \n"))
        self.assertEqual(
            release.extract_tag_notes("tag", "\n### Features\n\n- hi\n"),
            "### Features\n\n- hi\n",
        )


class GitTagIntegrationTests(unittest.TestCase):
    """Exercise annotated vs lightweight tag extraction against a temp git repo."""

    def test_annotated_and_lightweight_tag_notes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            env = {
                "GIT_AUTHOR_NAME": "Test",
                "GIT_AUTHOR_EMAIL": "test@example.com",
                "GIT_COMMITTER_NAME": "Test",
                "GIT_COMMITTER_EMAIL": "test@example.com",
            }

            def git(*args: str) -> str:
                return subprocess.run(
                    ["git", *args],
                    cwd=str(repo),
                    env={**dict(release.os.environ), **env},
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    check=True,
                ).stdout

            git("init", "-q")
            (repo / "a.txt").write_text("1\n", encoding="utf-8")
            git("add", "a.txt")
            git("commit", "-q", "-m", "feat: first\n\ncommit body text")
            git("tag", "v0.1.0")  # lightweight tag

            (repo / "a.txt").write_text("2\n", encoding="utf-8")
            git("add", "a.txt")
            git("commit", "-q", "-m", "fix: second")
            msg = release.format_tag_message("v0.1.1", "### Bug Fixes\n\n- custom\n")
            msg_file = repo / "tag-msg.txt"
            msg_file.write_text(msg, encoding="utf-8")
            # Even if a user configures strip cleanup, --cleanup=verbatim keeps
            # Markdown headings starting with '#'.
            git("config", "commit.cleanup", "strip")
            git("tag", "-a", "--cleanup=verbatim", "v0.1.1", "-F", str(msg_file))
            msg_file.unlink()

            orig_root = release.ROOT
            try:
                release.ROOT = repo
                # Lightweight tag must NOT return the commit body as release notes.
                self.assertIsNone(release.read_tag_notes("v0.1.0"))
                # Annotated tag returns its body verbatim including '#' headings.
                self.assertEqual(
                    release.read_tag_notes("v0.1.1"),
                    "### Bug Fixes\n\n- custom\n",
                )
            finally:
                release.ROOT = orig_root


if __name__ == "__main__":
    unittest.main()
