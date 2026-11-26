#!/usr/bin/env python3
"""Offline unit & integration tests for install.sh (stdlib only, headless-safe)."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = ROOT / "install.sh"
PLIST_TEMPLATE = (ROOT / "packaging" / "macos" / "Info.plist").read_text(
    encoding="utf-8"
)
REPO = "jjunac/cometty"


def archive_name(version: str) -> str:
    return f"Cometty-{version}-macos-universal.zip"


def release_url(version: str, filename: str) -> str:
    return f"https://github.com/{REPO}/releases/download/v{version}/{filename}"


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def make_fake_curl(bin_dir: Path) -> Path:
    """Create a fake curl binary that verifies TLS flags and serves local fixtures."""
    shim = bin_dir / "curl"
    write_executable(
        shim,
        textwrap.dedent(
            """\
            #!{python}
            import json
            import os
            import shutil
            import sys

            args = sys.argv[1:]
            required_pairs = [
                ("--proto", "=https"),
                ("--proto-redir", "=https"),
            ]
            for flag, val in required_pairs:
                if flag not in args or args[args.index(flag) + 1] != val:
                    sys.stderr.write(f"fake curl: missing {{flag}} {{val}}\\n")
                    sys.exit(90)
            if "--tlsv1.2" not in args or "--fail" not in args:
                sys.stderr.write("fake curl: missing --tlsv1.2 or --fail\\n")
                sys.exit(91)

            url = args[-1]
            log_path = os.environ.get("FAKE_CURL_LOG")
            if log_path:
                with open(log_path, "a", encoding="utf-8") as fh:
                    fh.write(url + "\\n")

            if "--head" in args:
                latest = os.environ.get("FAKE_CURL_LATEST_URL", "")
                if not latest:
                    sys.stderr.write("fake curl: 404 latest\\n")
                    sys.exit(22)
                if "-w" in args:
                    fmt = args[args.index("-w") + 1]
                    sys.stdout.write(fmt.replace("%{{url_effective}}", latest))
                sys.exit(0)

            manifest_path = os.environ["FAKE_CURL_MANIFEST"]
            with open(manifest_path, encoding="utf-8") as fh:
                manifest = json.load(fh)
            src = manifest.get(url)
            if not src:
                sys.stderr.write(f"fake curl: 404 {{url}}\\n")
                sys.exit(22)
            out = args[args.index("-o") + 1]
            shutil.copyfile(src, out)
            """
        ).format(python=sys.executable),
    )
    return shim


def make_bundle(
    parent: Path,
    version: str,
    *,
    app_name: str = "Cometty.app",
    bundle_id: str = "com.github.jjunac.cometty",
    source_binary: str = "/usr/bin/true",
    executable: bool = True,
    sign: bool = True,
    quarantine: bool = False,
) -> Path:
    app = parent / app_name
    macos_dir = app / "Contents" / "MacOS"
    macos_dir.mkdir(parents=True, exist_ok=True)
    binary = macos_dir / "cometty"
    shutil.copyfile(source_binary, binary)
    binary.chmod(0o755 if executable else 0o644)

    plist_text = (
        PLIST_TEMPLATE.replace("__VERSION__", version)
        .replace("__BUILD__", "1")
        .replace("com.github.jjunac.cometty", bundle_id)
    )
    (app / "Contents" / "Info.plist").write_text(plist_text, encoding="utf-8")

    if sign:
        subprocess.run(
            ["codesign", "--force", "--sign", "-", str(app)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    if quarantine:
        subprocess.run(
            [
                "xattr",
                "-w",
                "com.apple.quarantine",
                "0081;00000000;Safari;",
                str(app),
            ],
            check=True,
        )
    return app


def zip_dir(src_dir: Path, zip_path: Path) -> None:
    subprocess.run(
        ["ditto", "-c", "-k", str(src_dir), str(zip_path)],
        check=True,
    )


def make_release_assets(
    work_dir: Path,
    version: str,
    *,
    bundle_kwargs: dict | None = None,
    extra_top_level: str | None = None,
    tamper_after_sign: bool = False,
    symlink_app: bool = False,
    symlink_exec: bool = False,
    checksum_override: str | None = None,
) -> tuple[Path, Path]:
    stage = work_dir / f"stage-{version}-{os.urandom(4).hex()}"
    stage.mkdir()
    kwargs = dict(bundle_kwargs or {})

    if symlink_app:
        real_dir = work_dir / f"outside-{os.urandom(4).hex()}"
        real_dir.mkdir()
        (stage / "Cometty.app").symlink_to(real_dir)
    else:
        app = make_bundle(stage, version, **kwargs)
        if symlink_exec:
            exe = app / "Contents" / "MacOS" / "cometty"
            exe.unlink()
            exe.symlink_to("/usr/bin/true")
        if tamper_after_sign:
            (app / "Contents" / "Resources").mkdir(exist_ok=True)
            (app / "Contents" / "Resources" / "tampered.txt").write_text(
                "bad", encoding="utf-8"
            )

    if extra_top_level is not None:
        (stage / extra_top_level).mkdir(exist_ok=True)

    name = archive_name(version)
    zip_path = work_dir / f"{os.urandom(4).hex()}-{name}"
    zip_dir(stage, zip_path)

    sha_path = work_dir / f"{os.urandom(4).hex()}-{name}.sha256"
    if checksum_override is not None:
        sha_path.write_bytes(checksum_override.encode("utf-8"))
    else:
        digest = sha256_file(zip_path)
        sha_path.write_text(f"{digest}  {name}\n", encoding="utf-8")

    return zip_path, sha_path


class PortableInstallTests(unittest.TestCase):
    """Tests that do not require macOS-specific tools."""

    def test_sh_syntax_is_valid(self) -> None:
        res = subprocess.run(
            ["sh", "-n", str(INSTALL_SH)],
            text=True,
            capture_output=True,
        )
        self.assertEqual(res.returncode, 0, res.stderr)

    def test_help_works_directly_and_when_piped(self) -> None:
        direct = subprocess.run(
            ["sh", str(INSTALL_SH), "--help"],
            text=True,
            capture_output=True,
        )
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertIn("Usage:", direct.stdout)
        self.assertIn("--version", direct.stdout)
        self.assertIn("--dir", direct.stdout)

        piped = subprocess.run(
            ["sh", "-s", "--", "--help"],
            input=INSTALL_SH.read_text(encoding="utf-8"),
            text=True,
            capture_output=True,
        )
        self.assertEqual(piped.returncode, 0, piped.stderr)
        self.assertEqual(piped.stdout, direct.stdout)

    def test_unknown_option_and_missing_arg_rejected(self) -> None:
        for args in (["--nope"], ["--version"], ["--dir"]):
            res = subprocess.run(
                ["sh", str(INSTALL_SH), *args],
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(res.returncode, 0, args)
            self.assertIn("error:", res.stderr)

    def test_non_darwin_platform_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bin_dir = Path(tmp) / "bin"
            bin_dir.mkdir()
            write_executable(bin_dir / "uname", "#!/bin/sh\necho Linux\n")
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
            res = subprocess.run(
                ["sh", str(INSTALL_SH)],
                env=env,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(res.returncode, 0)
            self.assertIn("macOS-only", res.stderr)
            self.assertIn("cargo install", res.stderr)

    def test_old_macos_version_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bin_dir = Path(tmp) / "bin"
            bin_dir.mkdir()
            write_executable(bin_dir / "uname", "#!/bin/sh\necho Darwin\n")
            write_executable(bin_dir / "sw_vers", "#!/bin/sh\necho 12.7.5\n")
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
            res = subprocess.run(
                ["sh", str(INSTALL_SH)],
                env=env,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(res.returncode, 0)
            self.assertIn("macOS 13 or later", res.stderr)
            self.assertIn("12.7.5", res.stderr)


@unittest.skipUnless(sys.platform == "darwin", "macOS-only integration tests")
class MacOSInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        make_fake_curl(self.bin_dir)

        self.work_dir = self.root / "fixtures"
        self.work_dir.mkdir()
        self.tmp_dir = self.root / "tmpdir"
        self.tmp_dir.mkdir()
        self.home_dir = self.root / "home"
        self.home_dir.mkdir()
        self.default_apps = self.root / "Applications"
        self.default_apps.mkdir()

        self.log_file = self.root / "curl.log"
        self.manifest_file = self.root / "manifest.json"
        self.manifest: dict[str, str] = {}
        self.latest_url = ""

    def tearDown(self) -> None:
        # Restore permissions on any read-only dirs created by tests before cleanup.
        for path in [self.default_apps, *self.root.rglob("*")]:
            try:
                if path.is_dir() and not path.is_symlink():
                    path.chmod(0o755)
            except OSError:
                pass
        self._tmp.cleanup()

    def register_release(
        self, version: str, zip_path: Path, sha_path: Path, *, set_latest: bool = True
    ) -> None:
        name = archive_name(version)
        self.manifest[release_url(version, name)] = str(zip_path)
        self.manifest[release_url(version, f"{name}.sha256")] = str(sha_path)
        if set_latest:
            self.latest_url = f"https://github.com/{REPO}/releases/tag/v{version}"

    def run_installer(
        self,
        args: list[str] | None = None,
        *,
        piped: bool = False,
        extra_env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        self.manifest_file.write_text(json.dumps(self.manifest), encoding="utf-8")
        if self.log_file.exists():
            self.log_file.unlink()
        env = {
            "PATH": f"{self.bin_dir}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.home_dir),
            "TMPDIR": str(self.tmp_dir),
            "COMETTY_DEFAULT_INSTALL_DIR": str(self.default_apps),
            "FAKE_CURL_MANIFEST": str(self.manifest_file),
            "FAKE_CURL_LOG": str(self.log_file),
            "FAKE_CURL_LATEST_URL": self.latest_url,
        }
        if extra_env:
            env.update(extra_env)

        cmd_args = args or []
        if piped:
            return subprocess.run(
                ["sh", "-s", "--", *cmd_args],
                input=INSTALL_SH.read_text(encoding="utf-8"),
                env=env,
                text=True,
                capture_output=True,
            )
        return subprocess.run(
            ["sh", str(INSTALL_SH), *cmd_args],
            env=env,
            text=True,
            capture_output=True,
        )

    def curl_log(self) -> list[str]:
        if not self.log_file.exists():
            return []
        return self.log_file.read_text(encoding="utf-8").splitlines()

    def installed_version(self, app_path: Path) -> str:
        return subprocess.run(
            [
                "plutil",
                "-extract",
                "CFBundleShortVersionString",
                "raw",
                str(app_path / "Contents" / "Info.plist"),
            ],
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()

    def test_fresh_latest_install_via_piped_stdin(self) -> None:
        zip_path, sha_path = make_release_assets(
            self.work_dir, "0.9.0", bundle_kwargs={"quarantine": True}
        )
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(piped=True)
        self.assertEqual(res.returncode, 0, res.stderr)

        installed = self.default_apps / "Cometty.app"
        self.assertTrue(installed.is_dir())
        self.assertEqual(self.installed_version(installed), "0.9.0")

        # Exact request order: latest redirect -> checksum -> archive.
        name = archive_name("0.9.0")
        self.assertEqual(
            self.curl_log(),
            [
                f"https://github.com/{REPO}/releases/latest",
                release_url("0.9.0", f"{name}.sha256"),
                release_url("0.9.0", name),
            ],
        )

        # Quarantine attribute was stripped and signature verifies.
        xattr = subprocess.run(
            ["xattr", "-p", "com.apple.quarantine", str(installed)],
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(xattr.returncode, 0)
        verify = subprocess.run(
            ["codesign", "--verify", "--deep", "--strict", str(installed)],
            text=True,
            capture_output=True,
        )
        self.assertEqual(verify.returncode, 0, verify.stderr)

        # No staging siblings or temp dirs left behind.
        self.assertEqual([p.name for p in self.default_apps.iterdir()], ["Cometty.app"])
        self.assertEqual(list(self.tmp_dir.iterdir()), [])

    def test_explicit_version_accepts_plain_and_v_prefix_without_latest_lookup(
        self,
    ) -> None:
        zip_path, sha_path = make_release_assets(self.work_dir, "0.4.2")
        self.register_release("0.4.2", zip_path, sha_path, set_latest=False)

        for flag in ("0.4.2", "v0.4.2"):
            dest = self.root / f"dest-{flag}"
            res = self.run_installer(["--version", flag, "--dir", str(dest)])
            self.assertEqual(res.returncode, 0, res.stderr)
            self.assertEqual(self.installed_version(dest / "Cometty.app"), "0.4.2")
            self.assertNotIn(
                f"https://github.com/{REPO}/releases/latest", self.curl_log()
            )

    def test_invalid_version_values_rejected(self) -> None:
        for bad in (
            "",
            "v",
            "vv1.2.3",
            "V1.2.3",
            "01.2.3",
            "0.02.3",
            "1.2.03",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc.1",
            "1.2.3+meta",
            "1.2.3 ",
            "../1.2.3",
        ):
            res = self.run_installer(["--version", bad])
            self.assertNotEqual(res.returncode, 0, repr(bad))
            self.assertIn("canonical X.Y.Z", res.stderr, repr(bad))
            self.assertEqual(self.curl_log(), [])

    def test_bad_latest_redirects_rejected(self) -> None:
        for bad_url in (
            f"https://github.com/{REPO}/releases",
            f"https://github.com/{REPO}/releases/tag/v0.9.0-rc.1",
            f"https://github.com/{REPO}/releases/tag/0.9.0",
            "https://evil.example/jjunac/cometty/releases/tag/v0.9.0",
        ):
            self.latest_url = bad_url
            res = self.run_installer()
            self.assertNotEqual(res.returncode, 0, bad_url)
            self.assertEqual(len(self.curl_log()), 1)

    def test_malformed_checksum_rejected_before_downloading_archive(self) -> None:
        name = archive_name("0.9.0")
        good_hex = "a" * 64
        bad_bodies = [
            "",
            f"{good_hex}  Other-0.9.0-macos-universal.zip\n",
            f"{good_hex}  {name}\n\n",
            f"{good_hex}  {name}\n{good_hex}  {name}\n",
            f"{good_hex}  {name}\r\n",
            f"{good_hex}  {name}\r",
            f"{good_hex}  {name} \n",
            f"{good_hex} *{name}\n",
            f"{good_hex}  *{name}\n",
            f"{good_hex[:-1]}  {name}\n",
            f"g{good_hex[1:]}  {name}\n",
        ]
        for body in bad_bodies:
            zip_path, sha_path = make_release_assets(
                self.work_dir, "0.9.0", checksum_override=body
            )
            self.register_release("0.9.0", zip_path, sha_path)
            res = self.run_installer(["--version", "0.9.0"])
            self.assertNotEqual(res.returncode, 0, repr(body))
            # Archive download must never happen after a bad checksum file.
            self.assertEqual(
                self.curl_log(), [release_url("0.9.0", f"{name}.sha256")], repr(body)
            )
            self.assertEqual(list(self.tmp_dir.iterdir()), [])

    def test_uppercase_hex_and_missing_trailing_newline_accepted(self) -> None:
        zip_path, _ = make_release_assets(self.work_dir, "0.9.0")
        name = archive_name("0.9.0")
        upper_digest = sha256_file(zip_path).upper()
        sha_path = self.work_dir / "upper.sha256"
        sha_path.write_text(f"{upper_digest}  {name}", encoding="utf-8")
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(["--version", "0.9.0"])
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertTrue((self.default_apps / "Cometty.app").is_dir())

    def test_digest_mismatch_rejected_and_preserves_existing_install(self) -> None:
        make_bundle(self.default_apps, "0.8.0")
        name = archive_name("0.9.0")
        zip_path, sha_path = make_release_assets(
            self.work_dir, "0.9.0", checksum_override=f"{'0' * 64}  {name}\n"
        )
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(["--version", "0.9.0"])
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("SHA-256 mismatch", res.stderr)
        self.assertEqual(
            self.installed_version(self.default_apps / "Cometty.app"), "0.8.0"
        )
        self.assertEqual(list(self.tmp_dir.iterdir()), [])

    def test_invalid_bundles_rejected(self) -> None:
        cases = [
            ("extra top-level", {"extra_top_level": "__MACOSX"}, "only Cometty.app"),
            (
                "wrong app name",
                {"bundle_kwargs": {"app_name": "Other.app"}},
                "only Cometty.app",
            ),
            ("symlinked app", {"symlink_app": True}, "not a directory"),
            (
                "non-executable binary",
                {"bundle_kwargs": {"executable": False}},
                "missing an executable",
            ),
            ("symlinked executable", {"symlink_exec": True}, "missing an executable"),
            (
                "wrong bundle id",
                {"bundle_kwargs": {"bundle_id": "com.example.evil"}},
                "unexpected CFBundleIdentifier",
            ),
            (
                "version mismatch",
                {"bundle_kwargs": {"bundle_id": "com.github.jjunac.cometty"}},
                "unexpected CFBundleShortVersionString",
            ),
            (
                "tampered signature",
                {"tamper_after_sign": True},
                "failed codesign verification",
            ),
        ]
        for label, opts, expected_err in cases:
            if label == "version mismatch":
                # Build bundle claiming 0.8.0 inside a 0.9.0 archive.
                zip_path, sha_path = make_release_assets(self.work_dir, "0.8.0")
                name = archive_name("0.9.0")
                sha_path.write_text(
                    f"{sha256_file(zip_path)}  {name}\n", encoding="utf-8"
                )
            else:
                zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0", **opts)

            self.register_release("0.9.0", zip_path, sha_path)
            res = self.run_installer(["--version", "0.9.0"])
            self.assertNotEqual(res.returncode, 0, label)
            self.assertIn(expected_err, res.stderr, label)
            self.assertFalse((self.default_apps / "Cometty.app").exists(), label)
            self.assertEqual(list(self.tmp_dir.iterdir()), [], label)

    def test_upgrade_replaces_existing_cleanly(self) -> None:
        make_bundle(self.default_apps, "0.8.0")
        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(["--version", "0.9.0"])
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("upgrading Cometty 0.8.0 -> 0.9.0", res.stdout)
        self.assertEqual(
            self.installed_version(self.default_apps / "Cometty.app"), "0.9.0"
        )
        self.assertEqual([p.name for p in self.default_apps.iterdir()], ["Cometty.app"])

    def test_failed_upgrade_swap_rolls_back_previous_bundle(self) -> None:
        make_bundle(self.default_apps, "0.8.0")
        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        mv_shim_dir = self.root / "mv-shim"
        mv_shim_dir.mkdir()
        write_executable(
            mv_shim_dir / "mv",
            textwrap.dedent(
                """\
                #!/bin/sh
                case "$1" in
                    *.install-new.*) exit 1 ;;
                esac
                exec /bin/mv "$@"
                """
            ),
        )
        res = self.run_installer(
            ["--version", "0.9.0"],
            extra_env={
                "PATH": f"{mv_shim_dir}:{self.bin_dir}:/usr/bin:/bin:/usr/sbin:/sbin"
            },
        )
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("previous version restored", res.stderr)
        self.assertEqual(
            self.installed_version(self.default_apps / "Cometty.app"), "0.8.0"
        )
        self.assertEqual([p.name for p in self.default_apps.iterdir()], ["Cometty.app"])
        self.assertEqual(list(self.tmp_dir.iterdir()), [])

    def test_running_instance_refused_unless_forced(self) -> None:
        installed = make_bundle(
            self.default_apps, "0.8.0", source_binary="/bin/sleep"
        )
        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        proc = subprocess.Popen([str(installed / "Contents" / "MacOS" / "cometty"), "60"])
        try:
            blocked = self.run_installer(["--version", "0.9.0"])
            self.assertNotEqual(blocked.returncode, 0)
            self.assertIn("currently running", blocked.stderr)
            self.assertEqual(self.installed_version(installed), "0.8.0")
            self.assertEqual(self.curl_log(), [])

            forced = self.run_installer(["--version", "0.9.0", "--force"])
            self.assertEqual(forced.returncode, 0, forced.stderr)
            self.assertEqual(self.installed_version(installed), "0.9.0")
        finally:
            proc.terminate()
            proc.wait(timeout=5)

    def test_non_writable_default_dir_falls_back_to_home_applications(self) -> None:
        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        self.default_apps.chmod(stat.S_IRUSR | stat.S_IXUSR)
        res = self.run_installer(["--version", "0.9.0"])
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("not writable; installing to", res.stderr)

        fallback_app = self.home_dir / "Applications" / "Cometty.app"
        self.assertTrue(fallback_app.is_dir())
        self.assertEqual(self.installed_version(fallback_app), "0.9.0")

    def test_non_writable_explicit_dir_fails_without_fallback(self) -> None:
        ro_dir = self.root / "readonly"
        ro_dir.mkdir()
        ro_dir.chmod(stat.S_IRUSR | stat.S_IXUSR)

        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(["--version", "0.9.0", "--dir", str(ro_dir)])
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("not writable", res.stderr)
        self.assertFalse((self.home_dir / "Applications").exists())

    def test_install_dir_with_spaces(self) -> None:
        spaced = self.root / "My Apps Dir"
        zip_path, sha_path = make_release_assets(self.work_dir, "0.9.0")
        self.register_release("0.9.0", zip_path, sha_path)

        res = self.run_installer(["--version", "0.9.0", "--dir", str(spaced)])
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertEqual(self.installed_version(spaced / "Cometty.app"), "0.9.0")


if __name__ == "__main__":
    unittest.main()
