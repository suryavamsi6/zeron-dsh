"""scripts/stage-plugins-builtin.sh, the step the three release packages
(linux, macOS, Windows) share: Zeron's own Luau plugins into the bundle's
plugins-builtin/, and the Captains dsh bundle beside it in dsh-plugins/ —
outside plugins-builtin, the set `SetBuiltinPlugins` scans as Zeron's own.
Stages into a temp dir; no release build."""

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

# What SUR-108 stages: the manifest, the entry, the patch and the runtime
# source. Not `test/`, not a copy of the Luau SDK.
DSH_FILES = (
    "package.json",
    "cordis.patch.yml",
    "index.js",
    "src/config.js",
    "src/prompts.js",
    "src/tools.js",
)


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
        return pathlib.Path(self.tmp)

    def test_the_captains_bundle_stages_outside_the_builtin_scan_set(self):
        root = self.stage()
        dsh = root / "dsh-plugins" / "dsh-captains"
        for rel in DSH_FILES:
            self.assertTrue((dsh / rel).is_file(), rel)
        # `SetBuiltinPlugins` scans plugins-builtin/ and treats every folder
        # there as one of Zeron's own; a folder without zeron-plugin.json
        # shows as a broken, unremovable row. The dsh bundle must be outside.
        self.assertFalse((root / "plugins-builtin" / "dsh-captains").exists())
        staged_builtins = sorted(p.name for p in (root / "plugins-builtin").iterdir())
        sources = sorted(p.name for p in BUILTIN.iterdir() if p.is_dir())
        self.assertEqual(staged_builtins, sources)

    def test_the_staged_bundle_carries_no_tests(self):
        dsh = self.stage() / "dsh-plugins" / "dsh-captains"
        self.assertTrue(dsh.is_dir(), "the dsh bundle is not staged at all")
        self.assertFalse((dsh / "test").exists())
        self.assertEqual(list(dsh.rglob("*.test.mjs")), [])
        self.assertEqual(sorted(p.name for p in dsh.iterdir()),
                         ["cordis.patch.yml", "index.js", "package.json", "src"])

    def test_the_apps_own_plugins_stage_with_the_canonical_sdk(self):
        plugins = self.stage() / "plugins-builtin"
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

    def test_the_shipped_readme_names_the_provider_screen(self):
        # The clean-profile clause: a fresh install is a dead end without a
        # provider, so the README must say where it is — and the README must
        # actually ship beside the binary for that to reach anyone.
        readme = (ROOT / "README.md").read_text()
        self.assertTrue("Settings → Providers → Mistral Vibe" in readme,
                        "README.md does not name the provider screen")
        for rel in ("scripts/package-linux.sh", "scripts/package-zgx-macos.sh",
                    "scripts/package-windows.ps1"):
            self.assertIn("README.md", (ROOT / rel).read_text(), rel)

    def test_the_release_artifact_steps_assert_the_bundle(self):
        # Only a tag push (release.yml) or a beta tag (zgx-beta.yml) executes
        # these; the pin keeps the assertions from drifting out silently.
        # zgx-build.yml is workflow_call-only, so its inspect steps run inside
        # both callers' builds.
        for rel in (".github/workflows/zgx-build.yml",
                    ".github/workflows/release.yml",
                    ".github/workflows/zgx-beta.yml"):
            self.assertTrue("dsh-plugins/dsh-captains/index.js" in (ROOT / rel).read_text(),
                            f"{rel} does not assert the packaged bundle")


if __name__ == "__main__":
    unittest.main()
