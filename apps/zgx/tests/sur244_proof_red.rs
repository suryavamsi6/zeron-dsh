//! Throwaway proof for SUR-244: a failing `zgx` leg must make `CI result`
//! non-success. Never merged; the branch exists only for its CI run.

#[test]
fn sur244_proof_red() {
    assert!(false, "SUR-244 proof: this leg fails on purpose");
}
