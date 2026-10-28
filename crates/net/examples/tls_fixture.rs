//! Generate disposable local test certificates without committing private keys.
#[allow(dead_code)]
#[path = "../tests/support/tls.rs"]
mod fixture;
fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: tls_fixture NEW_DIRECTORY");
    let fixture = fixture::Fixture::create(std::path::Path::new(&root));
    for user in ["alice", "bob"] {
        let cert = torse_net::tls::certificates(&fixture.path(&format!("{user}.pem"))).unwrap();
        let digest = torse_net::tls::certificate_fingerprint(cert[0].as_ref());
        let hex: String = digest.0.iter().map(|byte| format!("{byte:02x}")).collect();
        println!("{user}_certificate_sha256={hex}");
    }
}
