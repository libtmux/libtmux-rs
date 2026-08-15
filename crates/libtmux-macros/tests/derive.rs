//! Compile-time contract tests for the `Filterable` derive.

#[test]
fn filterable_ui_contract() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
    tests.compile_fail("tests/ui/fail/*.rs");
}
