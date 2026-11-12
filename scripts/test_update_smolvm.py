import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import update_smolvm


SMOLVM_PATCH = """--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1 @@
-old
+new
"""

LIBKRUN_PATCH = """--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
"""


class PatchTreeTest(unittest.TestCase):
    def test_smolvm_patches_apply_to_the_source_root_that_the_build_names_dot(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            patches = work / "patches"
            patches.mkdir()
            (patches / "smolvm-0001-change.patch").write_text(SMOLVM_PATCH)
            (patches / "libkrun-0001-change.patch").write_text(LIBKRUN_PATCH)
            root = work / "stage"
            (root / "src").mkdir(parents=True)
            (root / "src" / "main.rs").write_text("old\n")
            (root / "libkrun" / "src").mkdir(parents=True)
            (root / "libkrun" / "src" / "lib.rs").write_text("old\n")

            records = update_smolvm.patch_records(patches)
            update_smolvm.apply_patches(root, patches, records)

            trees = {name: tree for name, tree, _ in records}
            self.assertEqual(trees, {"smolvm-0001-change.patch": ".", "libkrun-0001-change.patch": "libkrun"})
            self.assertEqual((root / "src" / "main.rs").read_text(), "new\n")
            self.assertEqual((root / "libkrun" / "src" / "lib.rs").read_text(), "new\n")
            for name, tree, _ in records:
                check = subprocess.run(
                    ["patch", "-p1", "-d", str(root / tree), "--dry-run", "--batch", "--reverse", "--silent", "-i", str(patches / name)],
                    capture_output=True,
                )
                self.assertEqual(check.returncode, 0, name)

    def test_a_patch_applied_with_fuzz_leaves_no_backup_file(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            patches = work / "patches"
            patches.mkdir()
            (patches / "smolvm-0001-change.patch").write_text(
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n one\n-old\n+new\n three\n"
            )
            root = work / "stage"
            (root / "src").mkdir(parents=True)
            (root / "src" / "main.rs").write_text("zero\none\nold\nTHREE\n")

            records = update_smolvm.patch_records(patches)
            update_smolvm.apply_patches(root, patches, records)

            self.assertEqual((root / "src" / "main.rs").read_text(), "zero\none\nnew\nTHREE\n")
            self.assertEqual(sorted(p.name for p in (root / "src").iterdir()), ["main.rs"])


if __name__ == "__main__":
    unittest.main()
