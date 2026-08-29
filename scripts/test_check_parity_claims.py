from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("check-parity-claims.py")


class ParityClaimsTest(unittest.TestCase):
    def run_check(
        self,
        rust: str,
        source: str = (
            "pub struct Server;\npub struct Pane;\n"
            "impl Server { pub fn sessions(&self) {} }\n"
        ),
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            crate = root / "libtmux"
            (crate / "src").mkdir(parents=True)
            (crate / "docs").mkdir()
            (crate / "src/lib.rs").write_text(source, encoding="utf-8")
            (crate / "docs/public-api.txt").write_text(
                "function libtmux::Server::sessions\n"
                "struct libtmux::Pane\n"
                "struct libtmux::Server\n",
                encoding="utf-8",
            )
            ledger = root / "parity.md"
            ledger.write_text(
                "| Python API | Python baseline | Rust target or delta | Delivery slice | Status |\n"
                "| --- | --- | --- | --- | --- |\n"
                f"| `sessions` | behavior | {rust} | Discovery | `implemented` |\n",
                encoding="utf-8",
            )
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(ledger), str(root)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_rejects_method_on_wrong_owner(self) -> None:
        result = self.run_check("`Pane::sessions`")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Pane::sessions", result.stdout)

    def test_rejects_unqualified_method(self) -> None:
        result = self.run_check("`sessions()`")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("names no public Rust path", result.stdout)

    def test_rejects_done_row_without_rust_path(self) -> None:
        result = self.run_check("Typed collection methods.")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("names no public Rust path", result.stdout)

    def test_checks_data_row_that_says_rust(self) -> None:
        result = self.run_check("Rust uses typed collection methods.")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("names no public Rust path", result.stdout)

    def test_accepts_method_on_its_owner(self) -> None:
        result = self.run_check("`Server::sessions`")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_private_path_does_not_replace_public_evidence(self) -> None:
        result = self.run_check(
            "Private `FormatDescriptor::profiles` metadata.",
            source=(
                "pub struct Server;\npub struct Pane;\n"
                "struct FormatDescriptor;\n"
                "impl FormatDescriptor {\n"
                "    fn profiles(&self) {}\n"
                "}\n"
            ),
        )

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("names no public Rust path", result.stdout)


if __name__ == "__main__":
    unittest.main()
