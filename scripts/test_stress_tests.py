"""The stress report counts a failure, and reports a clean run as clean."""

import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location(
    "stress_tests", pathlib.Path(__file__).with_name("stress-tests.py")
)
stress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stress)

PASSING = """\
     Running unittests src/lib.rs (target/debug/deps/libtmux-1)
test a::first ... ok
test a::second ... ok
     Running tests/control.rs (target/debug/deps/control-2)
test first ... ok
"""

FAILING = PASSING.replace("test a::second ... ok", "test a::second ... FAILED")


class Tally(unittest.TestCase):
    def test_a_failing_test_is_counted_per_run(self):
        runs = [(101, stress.parse(FAILING)), (0, stress.parse(PASSING)), (101, stress.parse(FAILING))]
        self.assertEqual(
            stress.tally(runs),
            [("unittests src/lib.rs::a::second", 3, 2)],
        )

    def test_a_clean_control_reports_nothing(self):
        runs = [(0, stress.parse(PASSING))] * 3
        self.assertEqual(stress.tally(runs), [])
        self.assertIn("No test failed in 3 runs.", stress.table([], 3, "control"))

    def test_same_name_in_two_binaries_stays_apart(self):
        results = stress.parse(PASSING)
        self.assertIn("tests/control.rs::first", results)

    def test_a_crash_with_no_test_line_is_not_a_green_run(self):
        rows = stress.tally([(101, stress.parse("error: could not compile\n"))])
        self.assertEqual(rows, [(stress.UNREPORTED, 1, 1)])


if __name__ == "__main__":
    unittest.main()
