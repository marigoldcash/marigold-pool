//! Types and pure logic shared by every other pool crate. Nothing here names a
//! chain: PLAN.md ground rule 8.

#[cfg(test)]
mod tests {
    /// Placeholder that proves the workspace compiles and the test runner on the
    /// rig is wired up. M2 replaces it with the real domain tests.
    #[test]
    fn the_workspace_builds_and_tests_run() {
        assert_eq!(env!("CARGO_PKG_NAME"), "pool-domain");
    }
}
