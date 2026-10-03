"""Negative controls for the source-owned complete-program manifest."""

import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("check_api_examples", ROOT / "scripts/check-api-examples.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class ApiExamples(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="libtmux-api-manifest-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.manifest = json.loads((ROOT / CHECK.MANIFEST).read_text())
        files = [CHECK.INVENTORY,
                 *(item["sourceFile"] for item in self.manifest["setupFiles"]),
                 *(item["sourceFile"] for item in self.manifest["examples"])]
        for file in files:
            destination = self.root / file
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / file, destination)

    def test_records_whole_files_and_native_targets(self):
        result = CHECK.validate(self.root, self.manifest)
        self.assertEqual(len(result["examples"]), 7)
        self.assertEqual(sum(len(item["targets"]) for item in result["examples"]), 31)
        for example in result["examples"]:
            self.assertEqual([item["name"] for item in example["files"]],
                             ["Cargo.toml", "rust-toolchain.toml", "src/main.rs"])
            for item in example["files"]:
                self.assertEqual(item["code"], (self.root / item["sourceFile"]).read_text())

    def test_rejects_unknown_native_target(self):
        self.manifest["examples"][0]["targets"][0] = "libtmux::Server::invented"
        with self.assertRaisesRegex(ValueError, "unknown native target"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_duplicate_native_target(self):
        target = self.manifest["examples"][0]["targets"][0]
        self.manifest["examples"][1]["targets"].append(target)
        with self.assertRaisesRegex(ValueError, "duplicate native target"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_unmapped_program(self):
        self.manifest["examples"].pop()
        with self.assertRaisesRegex(ValueError, "program files disagree"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_missing_setup(self):
        self.manifest["setupFiles"].pop()
        with self.assertRaisesRegex(ValueError, "both Cargo and toolchain"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_source_outside_checkout(self):
        self.manifest["setupFiles"][0]["sourceFile"] = "../Cargo.toml"
        with self.assertRaisesRegex(ValueError, "source path leaves checkout"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_truncated_file(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_bytes(file.read_bytes().rstrip(b"\n"))
        with self.assertRaisesRegex(ValueError, "LF-terminated"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_hidden_helper(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_text(file.read_text() + "mod helper;\n")
        with self.assertRaisesRegex(ValueError, "hidden source helper"):
            CHECK.validate(self.root, self.manifest)

    def test_rejects_missing_entrypoint(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_text(file.read_text().replace("async fn main()", "async fn helper()"))
        with self.assertRaisesRegex(ValueError, "displayed entrypoint"):
            CHECK.validate(self.root, self.manifest)


if __name__ == "__main__":
    unittest.main()
