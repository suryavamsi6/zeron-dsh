"""scripts/stage-plugins-builtin.sh, the step the three release packages
(linux, macOS, Windows) share to fill the bundle's plugins-builtin/: Zeron's
own plugins — the app's (apps/zgx/plugins-builtin/) and the repository's
Captains dsh bundle (plugins/dsh-captains/) — each Luau plugin handed the
canonical SDK. Stages into a temp dir; no release build."""

import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "stage-plugins-builtin.sh"
BUILTIN = ROOT / "apps" / "zgx" / "plugins-builtin"
CAPTAINS = ROOT / "plugins" / "dsh-captains"
SDK = ROOT / "apps" / "zgx" / "plugins-sdk" / "zeron.luau"


class StagePluginsBuiltinTests(unittest.TestCase):
    def setUp(self):
        if not BUILTIN.is_dir() or not CAPTAINS.is_dir() or not SDK.is_file():
            self.skipTest("the plugin sources are not checked out")
        self.tmp = tempfile.mkdtemp(prefix="stage-plugins-builtin-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def stage(self):
        proc = subprocess.run(["bash", str(SCRIPT), self.tmp],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return pathlib.Path(self.tmp) / "plugins-builtin"

    def test_the_bundle_carries_the_captains_plugin(self):
        plugins = self.stage()
        for rel in ("package.json", "index.js", "cordis.patch.yml",
                    "src/config.js", "src/prompts.js", "src/tools.js"):
            self.assertTrue((plugins / "dsh-captains" / rel).is_file(), rel)
        # A dsh bundle, not a Luau plugin: it gets no copy of the Luau SDK.
        self.assertFalse((plugins / "dsh-captains" / "zeron.luau").exists())

    def test_the_apps_own_plugins_stage_with_the_canonical_sdk(self):
        plugins = self.stage()
        canonical = SDK.read_bytes()
        for source in BUILTIN.iterdir():
            if not source.is_dir():
                continue
            staged = plugins / source.name
            self.assertTrue((staged / "zeron-plugin.json").is_file(), source.name)
            self.assertEqual((staged / "zeron.luau").read_bytes(), canonical, source.name)

    def test_each_release_package_runs_the_staging_step(self):
        for rel in ("scripts/package-linux.sh", "scripts/package-zgx-macos.sh",
                    "scripts/package-windows.ps1"):
            self.assertIn("stage-plugins-builtin.sh", (ROOT / rel).read_text(), rel)


if __name__ == "__main__":
    unittest.main()
