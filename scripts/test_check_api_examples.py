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
    package = "libtmux"

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="libtmux-api-manifest-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        manifest_path, inventory = CHECK.package_paths(self.package)
        self.manifest = json.loads((ROOT / manifest_path).read_text())
        files = [inventory,
                 *(item["sourceFile"] for item in self.manifest["setupFiles"]),
                 *(item["sourceFile"] for item in self.manifest["examples"])]
        for file in files:
            destination = self.root / file
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / file, destination)

    def validate(self):
        return CHECK.validate(self.root, self.manifest, self.package)

    def test_records_whole_files_and_native_targets(self):
        result = self.validate()
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
            self.validate()

    def test_rejects_duplicate_native_target(self):
        target = self.manifest["examples"][0]["targets"][0]
        self.manifest["examples"][1]["targets"].append(target)
        with self.assertRaisesRegex(ValueError, "duplicate native target"):
            self.validate()

    def test_rejects_unmapped_program(self):
        self.manifest["examples"].pop()
        with self.assertRaisesRegex(ValueError, "program files disagree"):
            self.validate()

    def test_rejects_missing_setup(self):
        self.manifest["setupFiles"].pop()
        with self.assertRaisesRegex(ValueError, "both Cargo and toolchain"):
            self.validate()

    def test_rejects_source_outside_checkout(self):
        self.manifest["setupFiles"][0]["sourceFile"] = "../Cargo.toml"
        with self.assertRaisesRegex(ValueError, "source path leaves checkout"):
            self.validate()

    def test_rejects_truncated_file(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_bytes(file.read_bytes().rstrip(b"\n"))
        with self.assertRaisesRegex(ValueError, "LF-terminated"):
            self.validate()

    def test_rejects_hidden_helper(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_text(file.read_text() + "mod helper;\n")
        with self.assertRaisesRegex(ValueError, "hidden source helper"):
            self.validate()

    def test_rejects_missing_entrypoint(self):
        file = self.root / self.manifest["examples"][0]["sourceFile"]
        file.write_text(file.read_text().replace("fn main()", "fn helper()"))
        with self.assertRaisesRegex(ValueError, "displayed entrypoint"):
            self.validate()

    def test_rejects_consumer_pointing_at_the_other_package(self):
        file = self.root / self.manifest["setupFiles"][0]["sourceFile"]
        other = "libtmux-tokio" if self.package == "libtmux" else "libtmux"
        file.write_text(file.read_text().replace(f"crates/{self.package}", f"crates/{other}"))
        with self.assertRaisesRegex(ValueError, "consumer must declare"):
            self.validate()

    def test_rejects_consumer_without_the_query_feature(self):
        file = self.root / self.manifest["setupFiles"][0]["sourceFile"]
        file.write_text(file.read_text().replace(', features = ["query"]', ""))
        with self.assertRaisesRegex(ValueError, "consumer must declare"):
            self.validate()


class TokioApiExamples(ApiExamples):
    package = "libtmux-tokio"


if __name__ == "__main__":
    unittest.main()
