// SPDX-License-Identifier: MIT OR Apache-2.0
//! The archives of tests/fixtures/package-vectors.txt were built from the text of the format by a
//! separate generator (packages/tools/scripts/package-vectors.mjs); the reader of the packer in
//! JavaScript answers the same lines. Both must agree with the expectation of every line.
use alef_package::{Package, Signer};
use std::fs;

fn bytes_of(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "hex of even length");
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex digit"))
        .collect()
}

#[test]
fn every_shared_vector_gets_the_answer_its_line_expects() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/package-vectors.txt"
    );
    let text = fs::read_to_string(path).expect("the shared vectors are in the repository");
    let mut checked = 0;
    let mut wrong = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut parts = line.split('\t');
        let (name, expect, hex) = (
            parts.next().expect("name"),
            parts.next().expect("expectation"),
            parts.next().expect("archive"),
        );
        let (got, detail) = match Package::from_bytes(bytes_of(hex)) {
            Ok(package) => match package.signer() {
                Signer::Signed { fingerprint, .. } => {
                    (format!("signed:{fingerprint}"), String::new())
                }
                Signer::Unsigned => ("unsigned".to_owned(), String::new()),
            },
            Err(error) => ("reject".to_owned(), error.to_string()),
        };
        if got != expect {
            wrong.push(format!("{name}: expected {expect}, got {got} {detail}"));
        }
        checked += 1;
    }
    assert!(checked >= 80, "only {checked} vectors");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
