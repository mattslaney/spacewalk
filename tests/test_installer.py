"""Exercise the real installer with local release assets and no network access."""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"
PAYLOAD = b"#!/bin/sh\nprintf 'spacewalk fixture\\n'\n"
TARGETS = {
    ("Linux", "x86_64"): "x86_64-unknown-linux-musl",
    ("Linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Darwin", "arm64"): "aarch64-apple-darwin",
}


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="spacewalk-installer-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.home = self.root / "home with spaces"
        self.home.mkdir()
        self.scratch = self.root / "scratch"
        self.scratch.mkdir()
        self.destination = self.home / ".local" / "bin"
        self.log = self.root / "downloads.jsonl"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": str(self.commands),
            "TMPDIR": str(self.scratch),
            "TEST_ASSETS": str(self.assets),
            "TEST_LOG": str(self.log),
            "TEST_OS": "Linux",
            "TEST_ARCH": "x86_64",
        }
        self.env.pop("SPACEWALK_VERSION", None)
        self.env.pop("SPACEWALK_INSTALL_DIR", None)
        # Restrict PATH so fallback and missing-dependency tests are deterministic.
        for tool in ("tar", "gzip", "mktemp", "mkdir", "chmod", "mv", "rm"):
            (self.commands / tool).symlink_to(shutil.which(tool))
        for tool in ("sha256sum", "shasum"):
            if executable := shutil.which(tool):
                (self.commands / tool).symlink_to(executable)
        self.write_command(
            "uname",
            'import os, sys\nprint(os.environ["TEST_OS" if sys.argv[1] == "-s" else "TEST_ARCH"])\n',
        )
        self.write_command(
            "curl",
            "import json, os, pathlib, shutil, sys\n"
            "args = sys.argv[1:]\n"
            "url = args[-1]\n"
            'with open(os.environ["TEST_LOG"], "a") as log:\n'
            "    log.write(json.dumps(url) + '\\n')\n"
            'if "--output" in args:\n'
            '    output = args[args.index("--output") + 1]\n'
            "else:\n"
            '    output = next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output-document="))\n'
            'source = pathlib.Path(os.environ["TEST_ASSETS"]) / url.rsplit("/", 1)[1]\n'
            "if not source.exists():\n"
            "    sys.exit(22)\n"
            "shutil.copyfile(source, output)\n",
        )
        for target in TARGETS.values():
            self.make_asset(target)

    def write_command(self, name, source):
        path = self.commands / name
        path.write_text(f"#!{sys.executable}\n" + source)
        path.chmod(0o755)

    def make_asset(self, target, member="spacewalk", payload=PAYLOAD):
        asset = self.assets / f"spacewalk-{target}.tar.gz"
        with tarfile.open(asset, "w:gz") as archive:
            info = tarfile.TarInfo(member)
            info.size = len(payload)
            info.mode = 0o755
            archive.addfile(info, io.BytesIO(payload))
        digest = hashlib.sha256(asset.read_bytes()).hexdigest()
        asset.with_name(asset.name + ".sha256").write_text(f"{digest}  {asset.name}\n")
        return asset

    def run_installer(self, *args, success=True):
        result = subprocess.run(
            ["/bin/sh", str(INSTALLER), *args],
            env=self.env,
            capture_output=True,
            text=True,
            cwd=self.root,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(list(self.scratch.iterdir()), [], "temporary downloads leaked")
        if self.destination.exists():
            self.assertEqual(list(self.destination.glob(".spacewalk.*")), [], "staged binary leaked")
        return result

    def downloads(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_latest_install_on_every_supported_platform(self):
        for (os_name, arch), target in TARGETS.items():
            with self.subTest(os=os_name, arch=arch):
                self.env.update(TEST_OS=os_name, TEST_ARCH=arch)
                result = self.run_installer()
                binary = self.destination / "spacewalk"
                self.assertEqual(binary.read_bytes(), PAYLOAD)
                self.assertTrue(os.access(binary, os.X_OK))
                self.assertIn("PATH", result.stdout)
                self.assertEqual(
                    self.downloads()[-2],
                    f"https://github.com/mattslaney/spacewalk/releases/latest/download/spacewalk-{target}.tar.gz",
                )

    def test_pinned_version_custom_directory_and_update(self):
        self.destination = self.root / "custom bin"
        self.destination.mkdir()
        (self.destination / "spacewalk").write_text("previous version")
        self.run_installer("--version", "v0.1.0", "--install-dir", str(self.destination))
        self.assertEqual((self.destination / "spacewalk").read_bytes(), PAYLOAD)
        self.assertIn("/releases/download/v0.1.0/", self.downloads()[0])

    def test_environment_options(self):
        self.destination = self.root / "environment bin"
        self.env.update(SPACEWALK_VERSION="v0.2.0-rc.1", SPACEWALK_INSTALL_DIR=str(self.destination))
        self.run_installer()
        self.assertTrue((self.destination / "spacewalk").exists())
        self.assertIn("/download/v0.2.0-rc.1/", self.downloads()[0])

    def test_relative_directory(self):
        self.run_installer("--install-dir", "relative bin")
        self.assertEqual((self.root / "relative bin" / "spacewalk").read_bytes(), PAYLOAD)

    def test_wget_fallback(self):
        (self.commands / "curl").rename(self.commands / "wget")
        self.run_installer()
        self.assertEqual((self.destination / "spacewalk").read_bytes(), PAYLOAD)

    def test_shasum_fallback(self):
        if not (self.commands / "shasum").exists():
            self.skipTest("shasum is not installed")
        (self.commands / "sha256sum").unlink(missing_ok=True)
        self.run_installer()
        self.assertEqual((self.destination / "spacewalk").read_bytes(), PAYLOAD)

    def test_bad_checksum_preserves_existing_binary(self):
        self.destination.mkdir(parents=True)
        binary = self.destination / "spacewalk"
        binary.write_text("keep this version")
        asset = self.assets / "spacewalk-x86_64-unknown-linux-musl.tar.gz"
        asset.write_bytes(asset.read_bytes() + b"tampered")
        result = self.run_installer(success=False)
        self.assertIn("Checksum verification failed", result.stderr)
        self.assertEqual(binary.read_text(), "keep this version")

    def test_missing_release_does_not_install(self):
        for asset in self.assets.iterdir():
            asset.unlink()
        result = self.run_installer(success=False)
        self.assertIn("release is published", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_malformed_checksum(self):
        for checksum in self.assets.glob("*.sha256"):
            checksum.write_text("invalid\n")
        result = self.run_installer(success=False)
        self.assertIn("Invalid checksum", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_archive_cannot_extract_outside_installation(self):
        self.make_asset(TARGETS[("Linux", "x86_64")], member="../escaped")
        result = self.run_installer(success=False)
        self.assertIn("Cannot extract", result.stderr)
        self.assertFalse((self.root / "escaped").exists())
        self.assertFalse((self.destination / "spacewalk").exists())

    def test_empty_binary_is_rejected(self):
        self.make_asset(TARGETS[("Linux", "x86_64")], payload=b"")
        result = self.run_installer(success=False)
        self.assertIn("binary is empty", result.stderr)
        self.assertFalse((self.destination / "spacewalk").exists())

    def test_directory_named_spacewalk_is_preserved(self):
        (self.destination / "spacewalk").mkdir(parents=True)
        result = self.run_installer(success=False)
        self.assertIn("is a directory", result.stderr)
        self.assertTrue((self.destination / "spacewalk").is_dir())

    def test_unsupported_platform_fails_before_download(self):
        self.env["TEST_ARCH"] = "armv7l"
        result = self.run_installer(success=False)
        self.assertIn("No prebuilt binary", result.stderr)
        self.assertFalse(self.log.exists())

    def test_missing_tools_fail_before_download(self):
        for tools in (("curl",), ("sha256sum", "shasum")):
            with self.subTest(tools=tools):
                saved_paths = []
                for tool in tools:
                    path = self.commands / tool
                    if path.exists():
                        saved = self.root / tool
                        path.rename(saved)
                        saved_paths.append((saved, path))
                result = self.run_installer(success=False)
                self.assertIn("Install", result.stderr)
                self.assertFalse(self.log.exists())
                for saved, path in saved_paths:
                    saved.rename(path)

    def test_help_and_invalid_arguments_do_not_download(self):
        result = self.run_installer("--help")
        self.assertIn("Usage:", result.stdout)
        for args in (
            ("--unknown",),
            ("--version",),
            ("--version", "../../main"),
            ("--version", "v1?query"),
            ("--install-dir", ""),
        ):
            with self.subTest(args=args):
                self.run_installer(*args, success=False)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
