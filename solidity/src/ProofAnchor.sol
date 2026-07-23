// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @notice Minimal append-only proof-anchoring contract. Stores nothing
/// beyond a running counter — the digest itself lives only in the emitted
/// event's log data, which every full node retains and any client can
/// re-fetch and re-verify via `eth_getLogs`. Mirrors proof-anchor's
/// LocalLogAnchor design: the chain's consensus + block finality stands in
/// for the local hash-chain's prev-hash linkage as the tamper-evidence
/// mechanism.
contract ProofAnchor {
    /// Emitted once per anchored proof. `index` is a simple incrementing
    /// counter, not a claim about ordering relative to other contracts.
    event ProofAnchored(bytes32 indexed digest, uint256 indexed index);

    uint256 private nextIndex;

    /// Anchors `digest` (a proof-core Digest's 32 bytes) and returns its
    /// position. Anyone may call this — the contract does not gate who can
    /// anchor, since authenticity comes from the Proof's own signatures,
    /// not from who submitted the anchoring transaction.
    function anchor(bytes32 digest) external returns (uint256 index) {
        index = nextIndex;
        nextIndex = index + 1;
        emit ProofAnchored(digest, index);
    }
}
