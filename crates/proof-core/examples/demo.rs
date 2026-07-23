//! Runnable end-to-end demonstration of proof-core.
//!
//! Usage:
//!   cargo run -p proof-core --example demo -- <path-to-file>
//!
//! Hashes the given file, signs the hash, builds a proof, verifies it, then
//! shows what happens when the file is tampered with or the proof bytes are
//! corrupted. Nothing here touches a blockchain — this only exercises the
//! hash -> sign -> build -> verify loop that anchoring will sit on top of.

use proof_core::hash::{self, HashAlgorithm};
use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey, SigningPublicKey};
use proof_core::{Proof, ProofBuilder};
use rand_core::OsRng;
use std::env;
use std::fs;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: demo <path-to-file>");
        return ExitCode::FAILURE;
    };

    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: could not read '{path}': {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("Read {} bytes from {path}\n", data.len());

    // 1. Hash the record.
    let digest = hash::hash(HashAlgorithm::Blake3, &data);
    println!("1. Hashed record with {}", digest.algorithm());
    println!("   digest: {}\n", digest.to_hex());

    // 2. Sign the digest. In a real deployment this key belongs to the
    //    organization/service committing the record, loaded from secure
    //    storage rather than generated fresh each run.
    let signing_key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
    println!("2. Generated a {} signing key\n", signing_key.algorithm());

    // 3. Build the proof: digest + signer attestation + caller metadata.
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs();
    let proof: Proof = ProofBuilder::new(digest, timestamp)
        .attest("demo-signer", &signing_key)
        .metadata("source_file", &path)
        .build()
        .expect("at least one attestation was added");
    println!(
        "3. Built proof (format v1, {} attestation(s))",
        proof.attestations().len()
    );

    let proof_bytes = proof
        .to_canonical_bytes()
        .expect("proof serializes to canonical bytes");
    println!("   canonical proof size: {} bytes\n", proof_bytes.len());

    // 4. Verify: this is what an anchoring/verification service would run
    //    on the far side, having only the proof bytes and (separately) the
    //    original file to check against. It also needs to know which
    //    public key it actually trusts for this signer -- here that's
    //    simply the key we just generated, standing in for a key a real
    //    verifier would load from a registry.
    let trusted_key = signing_key.public_key();
    println!("4. Verifying proof against the untouched file...");
    run_verification(&proof_bytes, &data, &trusted_key);

    // 5. Tamper with the record and show verification catching it.
    println!("\n5. Tampering with the record and re-verifying...");
    let mut tampered = data.clone();
    if let Some(byte) = tampered.first_mut() {
        *byte ^= 0xFF;
    } else {
        tampered.push(0xFF);
    }
    run_verification(&proof_bytes, &tampered, &trusted_key);

    // 6. Simulate a forgery attempt: an attacker who does not hold the
    //    original signing key builds their own proof over the *same*
    //    digest and tries to pass it off as authentic. The signature alone
    //    can't catch this -- it's a perfectly valid signature, just from
    //    the wrong signer -- so a real verifier must also check the
    //    signer's public key against a trusted registry. This crate
    //    deliberately doesn't decide who's trusted; it only makes that
    //    check possible by exposing the public key on each attestation.
    println!("\n6. Building a forged proof with a different signing key over the same digest...");
    let forger_key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
    let forged_proof = ProofBuilder::new(digest, timestamp)
        .attest("attacker-claiming-to-be-demo-signer", &forger_key)
        .build()
        .expect("at least one attestation was added");
    let forged_bytes = forged_proof
        .to_canonical_bytes()
        .expect("proof serializes to canonical bytes");
    println!("   forged proof's signature verifies on its own (it's internally consistent):");
    run_verification(&forged_bytes, &data, &trusted_key);

    ExitCode::SUCCESS
}

/// Runs the checks a verifier performs: signature validity, digest match
/// against a candidate copy of the record, and -- the check the signature
/// alone cannot do -- whether the signer's public key is the one this
/// verifier actually trusts.
fn run_verification(proof_bytes: &[u8], candidate_record: &[u8], trusted_key: &SigningPublicKey) {
    let proof = match Proof::from_canonical_bytes(proof_bytes) {
        Ok(p) => p,
        Err(e) => {
            println!("   [FAIL] proof bytes did not deserialize: {e}");
            return;
        }
    };

    match proof.verify_attestations() {
        Ok(()) => println!("   [OK]   all attestations verify"),
        Err(e) => println!("   [FAIL] attestation verification failed: {e}"),
    }

    let record_matches = hash::verify(proof.digest(), candidate_record);
    if record_matches {
        println!("   [OK]   candidate record matches the proof's digest");
    } else {
        println!(
            "   [FAIL] candidate record does NOT match the proof's digest (tampered or wrong file)"
        );
    }

    let signed_by_trusted_key = proof
        .attestations()
        .iter()
        .any(|a| a.public_key() == trusted_key);
    if signed_by_trusted_key {
        println!("   [OK]   signed by the trusted key");
    } else {
        println!(
            "   [FAIL] NOT signed by the trusted key (signature is valid, but from an unknown signer)"
        );
    }
}
